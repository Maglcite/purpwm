import importlib.util
from pathlib import Path
import tempfile
import unittest


spec = importlib.util.spec_from_file_location(
    "keyring_pam", Path(__file__).with_name("configure-keyring-pam.py"),
)
pam = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pam)


class KeyringPamTests(unittest.TestCase):
    def test_keeps_authentication_and_adds_optional_hooks_once(self):
        original = ("#%PAM-1.0\nauth requisite pam_nologin.so\n"
                    "auth include system-local-login\n"
                    "session include system-local-login\n")
        configured = pam.with_keyring(original)
        self.assertTrue(configured.startswith(original))
        self.assertIn("auth       optional     pam_gnome_keyring.so\n", configured)
        self.assertIn("session    optional     pam_gnome_keyring.so auto_start\n", configured)
        self.assertEqual(pam.with_keyring(configured), configured)

    def test_existing_hooks_keep_controls_comments_and_gain_options(self):
        original = ("auth optional pam_gnome_keyring.so\n"
                    "-session optional /usr/lib/security/pam_gnome_keyring.so # existing\n")
        configured = pam.with_keyring(original)
        self.assertIn("auto_start # existing", configured)
        self.assertEqual(configured.count("pam_gnome_keyring.so"), 2)
        self.assertEqual(pam.with_keyring(configured), configured)

    def test_password_hook_preserves_password_stack(self):
        original = "password include system-auth"  # missing final newline
        configured = pam.with_keyring(original, password_service=True)
        self.assertTrue(configured.startswith(original + "\n"))
        self.assertIn("pam_gnome_keyring.so use_authtok\n", configured)
        self.assertNotIn("auto_start", configured)

    def test_check_does_not_write_and_backup_survives_repeated_runs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "greetd"
            original = "auth include system-local-login\n"
            path.write_text(original)
            self.assertTrue(pam.configure(root, ["greetd"], check=True))
            self.assertEqual(path.read_text(), original)
            self.assertEqual(len(list(root.iterdir())), 1)
            self.assertTrue(pam.configure(root, ["greetd"]))
            backup = root / "greetd.purpwm-keyring.bak"
            self.assertEqual(backup.read_text(), original)
            self.assertFalse(pam.configure(root, ["greetd"]))
            self.assertEqual(backup.read_text(), original)


if __name__ == "__main__":
    unittest.main()
