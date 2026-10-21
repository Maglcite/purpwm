import importlib.machinery
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


loader = importlib.machinery.SourceFileLoader(
    "portal_setup", str(Path(__file__).resolve().parents[1] / "purpwm-portal"),
)
spec = importlib.util.spec_from_loader(loader.name, loader)
portal = importlib.util.module_from_spec(spec)
loader.exec_module(portal)


class PortalSetupTests(unittest.TestCase):
    def test_setup_preserves_existing_portals_and_is_repeatable(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            configuration = root / "config/xdg-desktop-portal"
            configuration.mkdir(parents=True)
            original = "[preferred]\ndefault=gtk\norg.freedesktop.impl.portal.FileChooser=gtk\n"
            config = configuration / "purpwm-portals.conf"
            config.write_text(original)
            other = configuration / "cosmic-portals.conf"
            other.write_text("[preferred]\ndefault=cosmic\n")
            environment = {
                "HOME": temporary, "XDG_RUNTIME_DIR": temporary,
                "XDG_CONFIG_HOME": str(root / "config"), "XDG_DATA_HOME": str(root / "data"),
            }
            with patch.dict(os.environ, environment), patch.object(portal.subprocess, "run"):
                portal.setup()
                first = config.read_text()
                self.assertIn("org.freedesktop.impl.portal.Screenshot=purpwm", first)
                self.assertIn("org.freedesktop.impl.portal.FileChooser=gtk", first)
                self.assertIn("default=gtk", first)
                self.assertEqual(config.with_suffix(".conf.bak").read_text(), original)
                merged = root / "data/purpwm/portals"
                self.assertEqual((merged / "purpwm-portals.conf").read_text(), first)
                self.assertEqual((merged / other.name).resolve(), other.resolve())
                self.assertTrue((merged / "gtk.portal").exists())
                portal.setup()
                self.assertEqual(config.read_text(), first)
                self.assertEqual(config.with_suffix(".conf.bak").read_text(), original)


if __name__ == "__main__":
    unittest.main()
