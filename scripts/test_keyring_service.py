"""Integration test using a private D-Bus session and disposable keyring only."""

import os
from pathlib import Path
import subprocess
import tempfile
import time


def run(*arguments, **kwargs):
    return subprocess.run(arguments, text=True, capture_output=True, check=True, **kwargs)


def main():
    # Refuse to run on the desktop's real session bus.
    assert os.environ.pop("PURPWM_TEST_PRIVATE_BUS", None) == "1"
    helper = str(Path(__file__).resolve().parents[1] / "purpwm-keyring")
    with tempfile.TemporaryDirectory(prefix="purpwm-keyring-test-") as directory:
        root = Path(directory)
        (root / "data/keyrings").mkdir(parents=True, mode=0o700)
        os.environ.update(HOME=directory, XDG_DATA_HOME=str(root / "data"),
                          XDG_CONFIG_HOME=str(root / "config"), XDG_RUNTIME_DIR=directory,
                          GNOME_KEYRING_CONTROL=str(root / "keyring"))
        daemon = subprocess.Popen([
            "gnome-keyring-daemon", "--foreground", "--login", "--components=pkcs11,secrets",
            "--control-directory", str(root / "keyring"),
        ], stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        daemon.stdin.write(b"disposable-test-password")
        daemon.stdin.close()
        try:
            for _ in range(100):
                if (root / "keyring/control").exists():
                    break
                if daemon.poll() is not None:
                    raise RuntimeError("Disposable keyring daemon exited early")
                time.sleep(0.02)
            else:
                raise RuntimeError("Disposable keyring daemon did not become ready")
            run(helper, "start")
            import gi
            gi.require_version("Secret", "1")
            from gi.repository import Gio, GLib, Secret
            service = Secret.Service.get_sync(Secret.ServiceFlags.LOAD_COLLECTIONS, None)
            owner_pid = service.get_connection().call_sync(
                "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                "GetConnectionUnixProcessID",
                GLib.Variant("(s)", (service.get_name_owner(),)),
                GLib.VariantType.new("(u)"), Gio.DBusCallFlags.NONE, 5000, None,
            ).unpack()[0]
            assert owner_pid == daemon.pid, "Refusing to use a non-test Secret Service"
            login = Secret.Collection.for_alias_sync(service, "login", Secret.CollectionFlags.NONE, None)
            assert login is not None, run(helper, "status").stdout
            assert not login.get_locked()
            default = Secret.Collection.for_alias_sync(
                service, "default", Secret.CollectionFlags.NONE, None,
            )
            assert default is not None
            assert default.get_object_path() == login.get_object_path()
            run("secret-tool", "store", "--label=purpwm disposable test",
                "application", "purpwm-private-test", input="disposable-test-secret")
            result = run("secret-tool", "lookup", "application", "purpwm-private-test")
            assert result.stdout.strip() == "disposable-test-secret"
            assert "unlocked" in run(helper, "status").stdout
            assert "Keyring unlocked." in run(helper, "unlock").stdout
            run("secret-tool", "clear", "application", "purpwm-private-test")
            result = subprocess.run(
                ["secret-tool", "lookup", "application", "purpwm-private-test"],
                text=True, capture_output=True,
            )
            assert result.returncode == 1 and not result.stdout
            service.lock_sync([login], None)
            assert f"{login.get_label()}: locked" in run(helper, "status").stdout
            print("PASS: isolated keyring startup, store, lookup, status, unlock, deletion and lock")
        finally:
            daemon.terminate()
            try:
                daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait()


if __name__ == "__main__":
    main()
