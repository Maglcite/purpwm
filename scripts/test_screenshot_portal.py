"""Run with dbus-run-session; uses a nested compositor and an isolated portal frontend."""

import importlib.machinery
import importlib.util
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import struct
import subprocess
import tempfile
import threading
import time

# The private test bus has no systemd-managed accessibility services.
os.environ.setdefault("GTK_A11Y", "none")

import gi
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("purpwm_portal", str(ROOT / "purpwm-portal"))
spec = importlib.util.spec_from_loader(loader.name, loader)
portal = importlib.util.module_from_spec(spec)
loader.exec_module(portal)


def wait_for(check, seconds=10):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.025)
    raise AssertionError("Timed out waiting for test service")


def main():
    assert os.environ.pop("PURPWM_TEST_PRIVATE_BUS", None) == "1", "Use a private dbus-run-session"
    processes = []
    files = []
    loop = GLib.MainLoop()
    thread = threading.Thread(target=loop.run, daemon=True)
    thread.start()
    owner = None
    try:
        with tempfile.TemporaryDirectory(prefix="purpwm-portal-test-") as directory:
            temporary = Path(directory)
            environment = os.environ.copy()
            environment.pop("PURPWM_STANDALONE", None)
            environment.update(PURPWM_SCALE="1", RUST_LOG="info")
            compositor_log = temporary / "compositor.log"
            with compositor_log.open("w") as log:
                compositor = subprocess.Popen(
                    [str(ROOT / "target/release/purpwm"), "--no-autostart"],
                    env=environment, stdout=log, stderr=log,
                )
            processes.append(compositor)
            def socket_ready():
                assert compositor.poll() is None, compositor_log.read_text()
                match = re.search(r"listening on WAYLAND_DISPLAY=(wayland-\d+)", compositor_log.read_text())
                if match:
                    path = Path(environment["XDG_RUNTIME_DIR"]) / f"purpwm-capture-{match[1]}.sock"
                    if path.exists():
                        return path, match[1]
            endpoint, wayland = wait_for(socket_ready)
            os.environ.update(WAYLAND_DISPLAY=wayland, GDK_BACKEND="wayland")
            connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
            backend = portal.Backend(connection, endpoint)
            def approve_test_dialog(handle, app):
                backend.ask_permission(handle, app)
                GLib.timeout_add(250, backend.approve, handle)
            backend.ask = approve_test_dialog
            acquired = threading.Event()
            owner = Gio.bus_own_name_on_connection(
                connection, portal.BUS_NAME, Gio.BusNameOwnerFlags.NONE,
                lambda *_: acquired.set(), None,
            )
            assert acquired.wait(5)
            client = Gio.DBusConnection.new_for_address_sync(
                environment["DBUS_SESSION_BUS_ADDRESS"],
                Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
                None, None,
            )
            descriptors = temporary / "portals"
            descriptors.mkdir()
            (descriptors / "purpwm.portal").write_text(portal.DESCRIPTOR)
            shutil_source = Path("/usr/share/xdg-desktop-portal/portals/gtk.portal")
            (descriptors / "gtk.portal").symlink_to(shutil_source)
            configuration = temporary / "config/xdg-desktop-portal"
            configuration.mkdir(parents=True)
            (configuration / "purpwm-portals.conf").write_text(
                "[preferred]\ndefault=gtk\norg.freedesktop.impl.portal.Screenshot=purpwm\n"
            )
            (descriptors / "purpwm-portals.conf").write_text(
                (configuration / "purpwm-portals.conf").read_text()
            )
            frontend_environment = environment | {
                "XDG_CURRENT_DESKTOP": "purpwm", "XDG_DESKTOP_PORTAL_DIR": str(descriptors),
                "XDG_CONFIG_HOME": str(temporary / "config"), "XDG_DATA_HOME": str(temporary / "data"),
            }
            frontend_log = temporary / "frontend.log"
            with frontend_log.open("w") as log:
                frontend = subprocess.Popen(["/usr/lib/xdg-desktop-portal", "--replace", "--verbose"],
                                            env=frontend_environment, stdout=log, stderr=log)
            processes.append(frontend)
            def frontend_ready():
                assert frontend.poll() is None, frontend_log.read_text()
                return client.call_sync(
                    "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                    "NameHasOwner", GLib.Variant("(s)", (portal.FRONTEND,)),
                    None, Gio.DBusCallFlags.NONE, 1000, None,
                ).unpack()[0]
            wait_for(frontend_ready)
            def screenshot_ready():
                try:
                    return client.call_sync(
                        portal.FRONTEND, portal.OBJECT, "org.freedesktop.DBus.Properties", "Get",
                        GLib.Variant("(ss)", ("org.freedesktop.portal.Screenshot", "version")),
                        None, Gio.DBusCallFlags.NO_AUTO_START, 1000, None,
                    ) is not None
                except GLib.Error:
                    assert frontend.poll() is None, frontend_log.read_text()
                    return False
            try:
                wait_for(screenshot_ready)
            except AssertionError:
                raise AssertionError(frontend_log.read_text())
            try:
                client.call_sync(portal.BUS_NAME, portal.OBJECT, portal.INTERFACE, "Screenshot",
                                 GLib.Variant("(ossa{sv})", ("/request/test", "test", "", {})),
                                 None, Gio.DBusCallFlags.NONE, 2000, None)
                raise AssertionError("Direct backend call bypassed frontend authentication")
            except GLib.Error as error:
                assert "AccessDenied" in str(error)

            def request(token, cancel=False):
                expected = ("/org/freedesktop/portal/desktop/request/"
                            + client.get_unique_name().lstrip(":").replace(".", "_") + "/" + token)
                responses = []
                done = threading.Event()
                subscription = client.signal_subscribe(
                    portal.FRONTEND, "org.freedesktop.portal.Request", "Response", expected,
                    None, Gio.DBusSignalFlags.NONE,
                    lambda *args: (responses.append(args[5].unpack()), done.set()),
                )
                try:
                    handle = client.call_sync(
                        portal.FRONTEND, portal.OBJECT, "org.freedesktop.portal.Screenshot", "Screenshot",
                        GLib.Variant("(sa{sv})", ("", {"handle_token": GLib.Variant("s", token),
                                                      "interactive": GLib.Variant("b", True)})),
                        None, Gio.DBusCallFlags.NONE, 10000, None,
                    ).unpack()[0]
                    assert handle == expected
                    if cancel:
                        wait_for(lambda: backend.pending)
                        client.call_sync(portal.FRONTEND, handle, "org.freedesktop.portal.Request", "Close",
                                         None, None, Gio.DBusCallFlags.NONE, 3000, None)
                        wait_for(lambda: not backend.pending)
                        return
                    assert done.wait(15), frontend_log.read_text()
                    return responses[0]
                finally:
                    client.signal_unsubscribe(subscription)

            response, results = request("capture")
            assert response == 0, (response, results, frontend_log.read_text())
            image = Path(GLib.filename_from_uri(results["uri"])[0])
            files.append(image)
            data = image.read_bytes()
            assert data.startswith(b"\x89PNG\r\n\x1a\n")
            width, height = struct.unpack(">II", data[16:24])
            assert width >= 320 and height >= 200
            assert stat.S_IMODE(image.stat().st_mode) == 0o600
            print(f"PASS: public Screenshot portal returned a real {width}x{height} framebuffer PNG")

            backend.ask = lambda *_: None
            request("cancel", cancel=True)
            print("PASS: frontend cancellation closes the backend request")
            backend.ask = approve_test_dialog
            if shutil.which("flameshot"):
                delivered = threading.Event()
                original_finish = backend.finish
                def observe_finish(handle, response, uri=None):
                    if response == 0 and uri:
                        files.append(Path(GLib.filename_from_uri(uri)[0]))
                        delivered.set()
                    return original_finish(handle, response, uri)
                backend.finish = observe_finish
                flameshot_log = temporary / "flameshot.log"
                with flameshot_log.open("w") as log:
                    flameshot = subprocess.Popen(["flameshot", "gui"], env=environment | {
                        "WAYLAND_DISPLAY": wayland, "QT_QPA_PLATFORM": "wayland",
                        "XDG_CURRENT_DESKTOP": "purpwm", "XDG_SESSION_TYPE": "wayland",
                        "XDG_CONFIG_HOME": str(temporary / "flameshot-config"),
                    }, stdout=log, stderr=log)
                processes.append(flameshot)
                assert delivered.wait(15), flameshot_log.read_text()
                time.sleep(0.5)
                assert "Unable to capture" not in flameshot_log.read_text(), flameshot_log.read_text()
                artifact = os.environ.get("PURPWM_TEST_ARTIFACT")
                if artifact:
                    Path(artifact).write_bytes(portal.capture_png(endpoint))
                flameshot.terminate()
                flameshot.wait(timeout=5)
                print("PASS: Flameshot gui captured the nested compositor through the native portal")
                backend.finish = original_finish
            os.kill(compositor.pid, signal.SIGUSR2)
            time.sleep(0.3)
            response, results = request("locked")
            assert response == 2 and "uri" not in results
            print("PASS: locked compositor refuses portal screenshots")
            print("PASS: direct backend callers cannot impersonate the portal frontend")
    finally:
        for process in reversed(processes):
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
        for path in files:
            path.unlink(missing_ok=True)
        if owner is not None:
            Gio.bus_unown_name(owner)
        loop.quit()
        thread.join(timeout=2)


if __name__ == "__main__":
    main()
