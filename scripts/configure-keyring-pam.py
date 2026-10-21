#!/usr/bin/env python3
"""Enable GNOME Keyring login unlock without changing authentication requirements."""

import argparse
import difflib
import os
from pathlib import Path
import shutil
import stat
import tempfile


def with_keyring(text, password_service=False):
    required = {"password": "use_authtok"} if password_service else {
        "auth": "", "session": "auto_start",
    }
    lines = text.splitlines(keepends=True)
    for kind, option in required.items():
        found = False
        for index, line in enumerate(lines):
            fields = line.split("#", 1)[0].split()
            if (len(fields) >= 3 and fields[0].lstrip("-") == kind
                    and Path(fields[2]).name == "pam_gnome_keyring.so"):
                found = True
                if option and option not in fields[3:]:
                    body, marker, comment = line.rstrip("\n").partition("#")
                    lines[index] = (body.rstrip() + " " + option
                                    + (" #" + comment if marker else "") + "\n")
        if not found:
            if lines and not lines[-1].endswith("\n"):
                lines[-1] += "\n"
            lines.append(f"{kind:<10} optional     pam_gnome_keyring.so"
                         + (f" {option}" if option else "") + "\n")
    return "".join(lines)


def configure(directory, services, check=False):
    changed = False
    for service in services:
        path = (directory / service).resolve()
        if not path.is_file():
            print(f"Skipping {service}: not installed.")
            continue
        before = path.read_text()
        after = with_keyring(before, service == "passwd")
        if before == after:
            print(f"{service}: keyring hooks already configured.")
            continue
        changed = True
        print("".join(difflib.unified_diff(
            before.splitlines(keepends=True), after.splitlines(keepends=True),
            fromfile=str(path), tofile=str(path),
        )), end="")
        if check:
            continue
        # Preserve the first pre-keyring configuration for recovery.
        backup = path.with_name(path.name + ".purpwm-keyring.bak")
        if not backup.exists():
            shutil.copy2(path, backup)
        metadata = path.stat()
        with tempfile.NamedTemporaryFile(mode="w", dir=path.parent, delete=False) as staged:
            staged.write(after)
            os.fchmod(staged.fileno(), stat.S_IMODE(metadata.st_mode))
            os.fchown(staged.fileno(), metadata.st_uid, metadata.st_gid)
            staged_path = Path(staged.name)
        staged_path.replace(path)
    return changed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Only show proposed changes")
    parser.add_argument("--service", action="append", choices=(
        "greetd", "login", "gdm-password", "lightdm", "sddm", "passwd",
    ), help="PAM service to configure (default: greetd, login, passwd)")
    args = parser.parse_args()
    if not args.check and os.geteuid() != 0:
        parser.error("Run with sudo to update the login PAM configuration.")
    module_dirs = (Path("/usr/lib/security"), Path("/usr/lib64/security"),
                   Path("/lib/security"), *Path("/usr/lib").glob("*-linux-gnu/security"))
    if not any((directory / "pam_gnome_keyring.so").is_file() for directory in module_dirs):
        parser.error("Install gnome-keyring (including pam_gnome_keyring.so) first.")
    configure(Path("/etc/pam.d"), args.service or ["greetd", "login", "passwd"], args.check)


if __name__ == "__main__":
    main()
