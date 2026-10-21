"""Installer integration tests: all writes stay inside temporary directories."""

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("purpwm_setup", Path(__file__).with_name("install.py"))
setup = importlib.util.module_from_spec(spec)
spec.loader.exec_module(setup)


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="purpwm-setup-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        # Exercise shell and Desktop Entry escaping, including parameter-expansion braces.
        self.prefix = self.root / "space 'quote\" $dollar `tick` % } \\"
        self.config = self.root / "config ' } $settings"
        self.env = {key: value for key, value in os.environ.items() if not key.startswith("PURPWM_")}
        self.env.update(XDG_CONFIG_HOME=str(self.root / "xdg-config"),
                        XDG_DATA_HOME=str(self.root / "xdg-data"),
                        XDG_RUNTIME_DIR=str(self.root / "runtime"))
        self.stub = self.root / "test compositor"
        self.stub.write_text(f"#!{sys.executable}\n" +
            "import json, os, sys\nprint(json.dumps({'args': sys.argv[1:], 'env': "
            "{k: v for k, v in os.environ.items() if k.startswith('PURPWM_') or k == 'PATH'}}))\n")
        self.stub.chmod(0o755)

    def install(self, *options, success=True):
        command = [str(setup.REPO / "install.sh"), "--user", "--non-interactive",
                   "--no-build", "--binary", str(self.stub), "--dependencies", "none",
                   "--prefix", str(self.prefix), "--config-dir", str(self.config),
                   "--spotlight", "none", "--terminal", str(self.stub), "--autostart", "no",
                   *map(str, options)]
        result = subprocess.run(command, cwd=self.root, env=self.env, capture_output=True, text=True)
        if success:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        else:
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def launch(self, *options, standalone=False, **overrides):
        env = dict(self.env, **overrides)
        if standalone:
            env.pop("WAYLAND_DISPLAY", None)
            env.pop("DISPLAY", None)
        else:
            env["WAYLAND_DISPLAY"] = "purpwm-test-display"
        result = subprocess.run([str(self.prefix / "bin/start-purpwm"), *options],
                                cwd=self.root, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return json.loads(result.stdout)

    def test_user_install_and_launch_with_special_paths(self):
        self.install("--scale", "1.75", "--refresh-rate", "144")
        for name in ("purpwm", "start-purpwm", "purpwm-wifi", "purpwm-bt", "purpwm-polkit-agent",
                     "purpwm-keyring", "purpwm-portal"):
            self.assertTrue(os.access(self.prefix / "bin" / name, os.X_OK))
        self.assertFalse((self.root / "xdg-data/wayland-sessions").exists())
        report = self.launch("--spawn-terminal")
        self.assertEqual(report["args"], ["--no-autostart", "--spawn-terminal"])
        self.assertEqual(report["env"]["PURPWM_TERMINAL"], str(self.stub))
        self.assertEqual(report["env"]["PURPWM_SPOTLIGHT"], "none")
        self.assertEqual(report["env"]["PURPWM_DEFAULT_SCALE"], "1.75")
        self.assertEqual(report["env"]["PATH"].split(":")[0], str(self.prefix / "bin"))
        self.assertNotIn(str(setup.REPO), (self.prefix / "bin/start-purpwm").read_text())

    def test_environment_and_per_user_settings_override_install_defaults(self):
        self.install("--refresh-rate", "144")
        report = self.launch(PURPWM_REFRESH_RATE="165")
        self.assertEqual(report["env"]["PURPWM_REFRESH_RATE"], "165")
        overrides = self.root / "xdg-config/purpwm/session.conf"
        overrides.parent.mkdir(parents=True)
        overrides.write_text("export PURPWM_REFRESH_RATE=75\n")
        report = self.launch()
        self.assertEqual(report["env"]["PURPWM_REFRESH_RATE"], "75")

    def test_standalone_uses_selected_resolution_refresh_and_binary(self):
        self.install("--refresh-rate", "144", "--resolution", "2560x1440")
        gamescope = self.prefix / "bin/gamescope"
        shutil.copyfile(self.stub, gamescope)
        gamescope.chmod(0o755)
        report = self.launch("--spawn-terminal", standalone=True)
        self.assertEqual(report["args"], ["-W", "2560", "-H", "1440", "-w", "2560", "-h", "1440",
            "-r", "144", "-f", "--expose-wayland", "--", str(self.prefix / "bin/purpwm"),
            "--standalone", "--no-autostart", "--spawn-terminal"])

    def test_dry_run_changes_nothing(self):
        result = self.install("--dry-run", "--session", "yes")
        self.assertIn("Dry run: no changes made.", result.stdout)
        self.assertFalse(self.prefix.exists())
        self.assertFalse(self.config.exists())
        self.assertFalse((self.root / "xdg-data").exists())

    def test_custom_executable_and_session_directories(self):
        binaries = self.root / "custom executables"
        sessions = self.root / "custom sessions"
        self.install("--bin-dir", binaries, "--session-dir", sessions, "--session", "yes")
        self.assertTrue((sessions / "purpwm.desktop").is_file())
        result = subprocess.run([str(binaries / "start-purpwm")],
                                env=dict(self.env, WAYLAND_DISPLAY="test"), capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(json.loads(result.stdout)["env"]["PATH"].split(":")[0], str(binaries))

    def test_arch_dependency_profile_avoids_conflicting_rust_providers(self):
        args = setup.parser().parse_args(["--dependencies", "build"])
        with patch.object(setup, "package_manager", return_value="pacman"), patch.object(shutil, "which", return_value=None):
            command, = setup.package_commands(args)
        self.assertIn("rust", command)
        self.assertNotIn("cargo", command)
        self.assertIn("seatd", command)

    def test_invalid_values_do_not_write(self):
        for options in (("--scale", "nan"), ("--scale", "0.5"), ("--refresh-rate", "0"),
                        ("--resolution", "bad-mode"), ("--bin-dir", "/tmp/bad:directory"),
                        ("--prefix", "/tmp/bad\npath"), ("--terminal", "not-a-real-terminal")):
            with self.subTest(options=options):
                self.install(*options, success=False)
                self.assertFalse(self.prefix.exists())
                self.assertFalse(self.config.exists())

    def test_reinstall_keeps_first_config_backup(self):
        self.install("--refresh-rate", "144")
        original = (self.config / "session.conf").read_bytes()
        self.install("--refresh-rate", "75")
        self.assertEqual((self.config / "session.conf.bak").read_bytes(), original)
        self.install("--refresh-rate", "60")
        self.assertEqual((self.config / "session.conf.bak").read_bytes(), original)
        self.assertEqual(self.launch()["env"]["PURPWM_REFRESH_RATE"], "60")

    def test_system_staging_generates_paths_and_preserves_existing_pam(self):
        stage = self.root / "package root"
        pam_dir = stage / "etc/pam.d"
        pam_dir.mkdir(parents=True)
        (pam_dir / "common-auth").write_text("auth required pam_unix.so\n")
        (pam_dir / "common-account").write_text("account required pam_unix.so\n")
        options = ["--system", "--non-interactive", "--no-build", "--binary", str(self.stub),
                   "--dependencies", "none", "--destdir", str(stage), "--prefix", "/opt/purpwm",
                   "--spotlight", "none", "--terminal", str(self.stub)]
        for existing in (False, True):
            if existing:
                (pam_dir / "purpwm").write_text("# custom PAM config\n")
            result = subprocess.run([str(setup.REPO / "install.sh"), *options],
                                    env=self.env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            expected = "# custom PAM config\n" if existing else setup.pam_config(pam_dir)
            self.assertEqual((pam_dir / "purpwm").read_text(), expected)
            desktop = (stage / "usr/share/wayland-sessions/purpwm.desktop").read_text()
            self.assertIn('Exec=/bin/sh "/opt/purpwm/bin/start-purpwm"', desktop)
            launcher = (stage / "opt/purpwm/bin/start-purpwm").read_text()
            self.assertNotIn(str(stage), launcher)
            self.assertIn("/opt/purpwm/share/purpwm/session.conf", launcher)

    def test_keyring_pam_cannot_be_staged(self):
        result = subprocess.run([str(setup.REPO / "install.sh"), "--system", "--non-interactive",
                                 "--no-build", "--binary", str(self.stub), "--dependencies", "none",
                                 "--destdir", str(self.root / "stage"), "--keyring-pam", "yes"],
                                env=self.env, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--keyring-pam", result.stderr)
        self.assertFalse((self.root / "stage").exists())

    def test_font_must_be_an_outline_font(self):
        bitmap = self.root / "terminus.pcf"
        bitmap.write_bytes(b"\x01fcp" + bytes(60))
        with self.assertRaises(ValueError):
            setup.font_path(str(bitmap))
        outline = self.root / "mono.ttf"
        outline.write_bytes(b"\x00\x01\x00\x00" + bytes(60))
        self.assertEqual(setup.font_path(str(outline)), str(outline))

    def test_pam_distro_selection_and_unknown_stack(self):
        directory = self.root / "pam"
        directory.mkdir()
        with self.assertRaises(ValueError):
            setup.pam_config(directory)
        (directory / "system-auth").touch()
        self.assertIn("include      system-auth", setup.pam_config(directory))
        (directory / "system-auth").unlink()
        (directory / "password-auth").touch()
        self.assertIn("include      password-auth", setup.pam_config(directory))

    def test_desktop_entry_with_special_paths(self):
        self.install("--session", "yes")
        desktop = self.root / "xdg-data/wayland-sessions/purpwm.desktop"
        if shutil.which("desktop-file-validate"):
            result = subprocess.run(["desktop-file-validate", str(desktop)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        if not shutil.which("gio"):
            self.skipTest("gio is needed to exercise Desktop Entry launching")
        env = dict(self.env, WAYLAND_DISPLAY="purpwm-test-display")
        result = subprocess.run(["gio", "launch", str(desktop)], env=env,
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        reports = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
        self.assertEqual(len(reports), 1, result.stdout + result.stderr)
        self.assertEqual(reports[0]["env"]["PURPWM_TERMINAL"], str(self.stub))

    def test_wizard_collects_options_without_writes(self):
        args = setup.parser().parse_args(["--dry-run"])
        answers = iter(["user", str(self.prefix), "no", "no", "no", "none", str(self.stub), "none", "auto",
                        "1.25", "144", "auto", "no", str(self.prefix / "bin"), str(self.config),
                        str(self.root / "sessions")])
        with patch.object(sys.stdin, "isatty", return_value=True), patch("builtins.input", side_effect=lambda _: next(answers)):
            with contextlib.redirect_stdout(io.StringIO()):
                options, interactive = setup.configure(args)
        self.assertTrue(interactive)
        self.assertEqual(options.scope, "user")
        self.assertEqual(options.scale, 1.25)
        self.assertEqual(options.refresh_rate, 144)
        self.assertEqual(options.keyring_pam, "no")
        self.assertFalse(self.prefix.exists())


if __name__ == "__main__":
    unittest.main()
