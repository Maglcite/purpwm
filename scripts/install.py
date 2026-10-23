#!/usr/bin/env python3
"""Interactive purpwm installer. Uses only the Python standard library."""

import argparse
import json
import math
import os
from pathlib import Path
import pwd
import re
import shlex
import shutil
import subprocess
import sys
import tempfile


REPO = Path(__file__).resolve().parent.parent
TERMINALS = ("alacritty", "foot", "kitty", "wezterm", "cosmic-term", "gnome-terminal", "xterm")
BUILD_PACKAGES = {
    "pacman": "base-devel pkgconf wayland libxkbcommon mesa libinput seatd systemd-libs pam",
    "apt-get": "build-essential pkg-config libwayland-dev libxkbcommon-dev libegl1-mesa-dev "
               "libgles2-mesa-dev libgbm-dev libinput-dev libseat-dev libudev-dev libpam0g-dev",
    "dnf": "gcc gcc-c++ make pkgconf-pkg-config wayland-devel libxkbcommon-devel "
           "mesa-libEGL-devel mesa-libGLES-devel mesa-libgbm-devel libinput-devel "
           "libseat-devel systemd-devel pam-devel",
}
DESKTOP_PACKAGES = {
    "pacman": "gamescope alacritty networkmanager bluez-utils wireplumber pavucontrol "
              "playerctl brightnessctl btop python-gobject gtk4 polkit gnome-keyring "
              "libsecret fontconfig ttf-dejavu flameshot xdg-desktop-portal xdg-desktop-portal-gtk",
    "apt-get": "alacritty network-manager bluez wireplumber pavucontrol playerctl "
               "brightnessctl btop python3-gi gir1.2-gtk-4.0 gir1.2-polkit-1.0 "
               "gnome-keyring libsecret-1-0 fontconfig fonts-dejavu-core flameshot "
               "xdg-desktop-portal xdg-desktop-portal-gtk",
    "dnf": "gamescope alacritty NetworkManager bluez wireplumber pavucontrol playerctl "
           "brightnessctl btop python3-gobject gtk4 polkit gnome-keyring libsecret "
           "fontconfig dejavu-sans-mono-fonts flameshot xdg-desktop-portal "
           "xdg-desktop-portal-gtk",
}


def user_home():
    # Legacy sudo invocations still need the login user's paths.
    if os.geteuid() == 0 and os.environ.get("SUDO_USER"):
        return Path(pwd.getpwnam(os.environ["SUDO_USER"]).pw_dir)
    return Path.home()


def absolute_path(value, home):
    value = str(value)
    if any(c in value for c in ("\n", "\r", "\0")):
        raise ValueError("Paths cannot contain line breaks or NUL characters.")
    if value == "~" or value.startswith("~/"):
        value = str(home) + value[1:]
    return Path(os.path.abspath(value))


def package_manager():
    # Detect the distro, rather than an unrelated package manager on PATH.
    release = {}
    try:
        for line in Path("/etc/os-release").read_text().splitlines():
            key, sep, value = line.partition("=")
            if sep:
                release[key] = value.strip('"\'')
    except OSError:
        return None
    families = (release.get("ID", "") + " " + release.get("ID_LIKE", "")).split()
    for family in families:
        manager = {"arch": "pacman", "debian": "apt-get", "ubuntu": "apt-get",
                   "fedora": "dnf"}.get(family)
        if manager and shutil.which(manager):
            return manager
    return None


def ask(label, default, choices=None):
    while True:
        suffix = " (" + "/".join(choices) + ")" if choices else ""
        answer = input(f"{label}{suffix} [{default}]: ").strip() or str(default)
        if choices is None or answer in choices:
            return answer
        print("Choose " + ", ".join(choices) + ".")


def parser():
    p = argparse.ArgumentParser(description=__doc__, epilog=
        "Run without arguments for the wizard. Builds run as your user; only system "
        "file copies and requested package installs use sudo. Python 3.9+ is required.")
    scope = p.add_mutually_exclusive_group()
    scope.add_argument("--user", dest="scope", action="store_const", const="user")
    scope.add_argument("--system", dest="scope", action="store_const", const="system")
    p.add_argument("--prefix", help="Install prefix (user: ~/.local; system: /usr/local)")
    p.add_argument("--bin-dir", help="Executable directory (default: PREFIX/bin)")
    p.add_argument("--config-dir", help="Session defaults directory (user: XDG config; system: PREFIX/share/purpwm)")
    p.add_argument("--session-dir", help="Login session directory (system: /usr/share/wayland-sessions)")
    p.add_argument("--session", choices=("yes", "no"), help="Register a login session (default: system only)")
    p.add_argument("--pam", choices=("yes", "no"), help="Create lock PAM service (default: system only)")
    p.add_argument("--pam-dir", default="/etc/pam.d", help="PAM service directory")
    p.add_argument("--keyring-pam", choices=("yes", "no"),
                   help="Unlock GNOME Keyring at login by adding hooks to the greetd/login/passwd "
                        "PAM services (default: no)")
    p.add_argument("--dependencies", choices=("none", "build", "desktop"), help="Distro packages; desktop includes build dependencies")
    p.add_argument("--terminal", help="Terminal executable name/path, or auto")
    p.add_argument("--spotlight", help="Spotlight executable name/path, auto, or none")
    p.add_argument("--font", help="TTF/OTF file path, or auto to use fontconfig")
    p.add_argument("--wallpaper", help="Wallpaper image path, or none")
    p.add_argument("--scale", type=float, help="Default UI scale, 1.0 through 3.0")
    p.add_argument("--refresh-rate", type=int, help="Standalone refresh rate in Hz (default: 120)")
    p.add_argument("--resolution", help="Standalone resolution: auto or WIDTHxHEIGHT")
    p.add_argument("--autostart", choices=("yes", "no"), help="Start session helpers (default: yes)")
    p.add_argument("--no-build", action="store_true", help="Install an existing release binary")
    p.add_argument("--binary", help="Existing purpwm executable; requires --no-build")
    p.add_argument("--non-interactive", "--yes", action="store_true", help="Use flags/defaults without prompts")
    p.add_argument("--dry-run", action="store_true", help="Show the plan without building or changing files/packages")
    p.add_argument("--destdir", help="Stage files here; embedded runtime paths exclude DESTDIR")
    return p


def configure(args):
    home = user_home()
    interactive = not args.non_interactive and sys.stdin.isatty()
    if not interactive and not args.non_interactive and not args.dry_run:
        raise ValueError("No interactive terminal. Pass --non-interactive and your desired options.")
    if interactive:
        print("purpwm setup — press Enter to accept a default.\n")
        args.scope = args.scope or ask("Install for", "system", ("system", "user"))
    args.scope = args.scope or "user"
    system = args.scope == "system"
    defaults = {
        "prefix": "/usr/local" if system else str(home / ".local"),
        "session": "yes" if system else "no", "pam": "yes" if system else "no",
        "keyring_pam": "no",
        "dependencies": ("desktop" if system else "build") if interactive and package_manager() else "none",
        "terminal": "auto", "spotlight": "auto", "font": "auto", "wallpaper": "none", "scale": 1.5,
        "refresh_rate": 120, "resolution": "auto", "autostart": "yes",
    }
    labels = {"prefix": "Install prefix", "session": "Add a login-screen session",
              "pam": "Create screen-lock PAM service (preserve existing)",
              "keyring_pam": "Unlock the keyring at login (edits login PAM, keeps a backup)",
              "dependencies": "Install dependencies", "terminal": "Terminal executable",
              "spotlight": "Spotlight executable (auto/path/none)", "font": "Font file (auto/path)",
              "wallpaper": "Wallpaper image (path/none)",
              "scale": "Default UI scale (1.0–3.0)", "refresh_rate": "Refresh rate (Hz)",
              "resolution": "Resolution (auto or WIDTHxHEIGHT)", "autostart": "Start session helpers"}
    for key, default in defaults.items():
        if getattr(args, key) is None:
            choices = ("yes", "no") if key in ("session", "pam", "keyring_pam", "autostart") else None
            if key == "dependencies":
                choices = ("none", "build", "desktop")
            setattr(args, key, ask(labels[key], default, choices) if interactive else default)
    args.prefix = absolute_path(args.prefix, home)
    if args.wallpaper != "none":
        wallpaper = absolute_path(args.wallpaper, home)
        if not wallpaper.is_file():
            raise ValueError(f"Wallpaper image not found: {wallpaper}")
        if wallpaper.suffix.lower() not in {".png", ".jpg", ".jpeg", ".webp", ".bmp"}:
            raise ValueError("Wallpaper must be a PNG, JPEG, WebP, or BMP image.")
        args.wallpaper = str(wallpaper)
    xdg_config = absolute_path(os.environ.get("XDG_CONFIG_HOME") or home / ".config", home)
    xdg_data = absolute_path(os.environ.get("XDG_DATA_HOME") or home / ".local/share", home)
    defaults = {
        "bin_dir": args.prefix / "bin",
        "config_dir": args.prefix / "share/purpwm" if system else xdg_config / "purpwm",
        "session_dir": Path("/usr/share/wayland-sessions") if system else xdg_data / "wayland-sessions",
    }
    for key, default in defaults.items():
        value = getattr(args, key)
        if value is None:
            value = ask(key.replace("_", " ").capitalize(), default) if interactive else default
        setattr(args, key, absolute_path(value, home))
    args.pam_dir = absolute_path(args.pam_dir, home)
    if args.destdir:
        args.destdir = absolute_path(args.destdir, home)
        if args.destdir == Path("/"):
            raise ValueError("DESTDIR must be a staging directory, not /.")
        if args.dependencies != "none":
            raise ValueError("Use --dependencies none when staging with --destdir.")
    if ":" in str(args.bin_dir):
        raise ValueError("The executable directory cannot contain ':' because it is added to PATH.")
    args.scale, args.refresh_rate = float(args.scale), int(args.refresh_rate)
    if not math.isfinite(args.scale) or not 1.0 <= args.scale <= 3.0:
        raise ValueError("Scale must be between 1.0 and 3.0.")
    if not 1 <= args.refresh_rate <= 1000:
        raise ValueError("Refresh rate must be between 1 and 1000 Hz.")
    if args.resolution != "auto" and not re.fullmatch(r"[1-9][0-9]{2,4}x[1-9][0-9]{2,4}", args.resolution):
        raise ValueError("Resolution must be auto or WIDTHxHEIGHT, for example 2560x1600.")
    if args.keyring_pam == "yes" and args.destdir:
        raise ValueError("--keyring-pam edits the live /etc/pam.d and cannot be staged; use --keyring-pam no.")
    if args.pam == "yes" and not system and not args.destdir:
        raise ValueError("Use --system for a PAM service, or --pam no for a user installation.")
    if args.binary and not args.no_build:
        raise ValueError("--binary requires --no-build.")
    return args, interactive


def run(command, privileged=False, **kwargs):
    if privileged and os.geteuid() != 0:
        if not shutil.which("sudo"):
            raise ValueError("sudo is required for system paths or package installation.")
        command = ["sudo", "--", *command]
    print("+ " + shlex.join(map(str, command)), flush=True)
    return subprocess.run(list(map(str, command)), check=True, **kwargs)


def package_commands(args):
    if args.dependencies == "none":
        return []
    manager = package_manager()
    if manager is None:
        raise ValueError("Automatic packages support Arch/CachyOS, Debian/Ubuntu and Fedora. "
                         "Install the libraries in docs/installation.md and use --dependencies none.")
    packages = BUILD_PACKAGES[manager].split()
    if args.dependencies == "desktop":
        packages += DESKTOP_PACKAGES[manager].split()
        if manager == "apt-get":
            # Gamescope isn't shipped in every Debian/Ubuntu release/repository.
            policy = subprocess.run(["apt-cache", "policy", "gamescope"], capture_output=True, text=True)
            if re.search(r"Candidate:\s+(?!\(none\))\S+", policy.stdout):
                packages.append("gamescope")
            else:
                print("Note: no gamescope package candidate; install it separately before a standalone login.")
    if not shutil.which("cargo") or not shutil.which("rustc"):
        # Arch's rust package includes Cargo; the virtual `cargo` target may select
        # rustup, which conflicts with rust when both are requested together.
        packages += ["rust"] if manager == "pacman" else ["rustc", "cargo"]
    flags = {"pacman": ["-S", "--needed", "--noconfirm"], "apt-get": ["install", "-y"],
             "dnf": ["install", "-y"]}[manager]
    return [[manager, *flags, *dict.fromkeys(packages)]]


def resolve_program(value, candidates=()):
    if value == "none":
        return "none"
    if value == "auto":
        return next((shutil.which(p) for p in candidates if shutil.which(p)), "")
    if "/" in value:
        value = str(absolute_path(value, user_home()))
    found = shutil.which(value)
    if not found:
        raise ValueError(f"Executable not found: {value}. Supply a name or full path without arguments.")
    return found


def is_outline_font(path):
    # The bar's rasterizer reads TrueType/OpenType (and collections), not bitmap or web fonts.
    try:
        with open(path, "rb") as font:
            return font.read(4) in (b"\x00\x01\x00\x00", b"OTTO", b"true", b"ttcf")
    except OSError:
        return False


def font_path(value):
    if value != "auto":
        path = absolute_path(value, user_home())
        if not path.is_file():
            raise ValueError(f"Font file not found: {path}")
        if not is_outline_font(path):
            raise ValueError(f"Not a TrueType/OpenType font: {path}")
        return str(path)
    if shutil.which("fc-match"):
        result = subprocess.run(["fc-match", "-f", "%{file}", "monospace"], capture_output=True, text=True)
        if result.returncode == 0 and is_outline_font(result.stdout):
            return result.stdout
    # purpwm also asks fontconfig at startup and has a bundled fallback font.
    return ""


def session_config(args, terminal, spotlight, font):
    wallpaper = "" if args.wallpaper == "none" else args.wallpaper
    values = {"PURPWM_DEFAULT_SCALE": str(args.scale), "PURPWM_REFRESH_RATE": str(args.refresh_rate),
              "PURPWM_RESOLUTION": args.resolution, "PURPWM_AUTOSTART": "1" if args.autostart == "yes" else "0",
              "PURPWM_TERMINAL": terminal, "PURPWM_SPOTLIGHT": spotlight, "PURPWM_FONT": font,
              "PURPWM_WALLPAPER": wallpaper}
    if args.pam == "yes" and args.pam_dir != Path("/etc/pam.d"):
        values["PURPWM_PAM_CONFDIR"] = str(args.pam_dir)
    # Fill unset environment variables; quote paths as data, never shell commands.
    return "# Generated by purpwm install.sh. Environment variables override these defaults.\n" + "".join(
        f"export {key}=${{{key}-{shlex.quote(value)}}}\n" for key, value in values.items())


def desktop_entry(launcher):
    # Desktop Exec syntax is independent of shell quoting; escape both parsing layers.
    executable = str(launcher).replace("\\", "\\\\").replace('"', '\\"').replace("`", "\\`").replace("$", "\\$")
    executable = executable.replace("%", "%%").replace("\\", "\\\\")
    try_exec = str(launcher).replace("\\", "\\\\").replace(" ", "\\s").replace("\t", "\\t")
    return ("[Desktop Entry]\nName=purpwm\nComment=Purple dynamic tiling Wayland compositor\n"
            # An explicit interpreter also works around loaders checking the unexpanded
            # executable name when the install path contains a literal percent sign.
            f'Exec=/bin/sh "{executable}"\nTryExec={try_exec}\nType=Application\n')


def pam_config(directory):
    if (directory / "common-auth").is_file() and (directory / "common-account").is_file():
        auth, account = "common-auth", "common-account"
    elif (directory / "system-auth").is_file():
        auth = account = "system-auth"
    elif (directory / "password-auth").is_file():
        auth = account = "password-auth"
    else:
        raise ValueError(f"No supported PAM auth/account stack in {directory}. "
                         "Configure /etc/pam.d/purpwm manually, or pass --pam no.")
    return f"#%PAM-1.0\nauth       include      {auth}\naccount    include      {account}\n"


def build_binary(args):
    if args.no_build:
        target = Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target"))
        if not target.is_absolute():
            target = REPO / target
        path = absolute_path(args.binary or target / "release/purpwm", user_home())
    else:
        if os.geteuid() == 0:
            raise ValueError("Run ./install.sh without sudo so Cargo doesn't build as root.")
        if not shutil.which("cargo"):
            raise ValueError("Rust/Cargo is missing. Install Rust stable, or select --dependencies build.")
        # Cargo reports the actual artifact, including custom target directories/targets.
        command = ["cargo", "build", "--locked", "--release", "--bin", "purpwm",
                   "--message-format=json-render-diagnostics"]
        print("+ " + shlex.join(command), flush=True)
        path = None
        with subprocess.Popen(command, cwd=REPO, stdout=subprocess.PIPE, text=True) as process:
            for line in process.stdout:
                message = json.loads(line)
                if message.get("reason") == "compiler-artifact" and message["target"]["name"] == "purpwm":
                    if message.get("executable"):
                        path = Path(message["executable"])
                if message.get("reason") == "compiler-message":
                    print(message["message"].get("rendered", ""), end="", file=sys.stderr)
            if process.wait() != 0:
                raise ValueError("Release build failed. Check the output above and use a current stable Rust toolchain.")
    if path is None or not path.is_file() or not os.access(path, os.X_OK):
        raise ValueError(f"Release executable not found: {path}. Build first, or use --no-build --binary /path/to/purpwm.")
    return path


def staged_path(path, args):
    return args.destdir / path.relative_to("/") if args.destdir else path


def install_file(source, destination, mode, args, backup=False):
    destination = staged_path(destination, args)
    privileged = args.scope == "system" and not args.destdir
    if destination.is_dir():
        raise ValueError(f"Cannot replace a directory: {destination}")
    if backup and destination.exists() and not destination.with_name(destination.name + ".bak").exists():
        run(["cp", "-p", "--", destination, str(destination) + ".bak"], privileged)
    # Rename over the old executable so updating a running compositor is safe.
    temporary = destination.with_name(destination.name + f".install-{os.getpid()}")
    run(["install", "-D", "-m", mode, "--", source, temporary], privileged)
    run(["mv", "-fT", "--", temporary, destination], privileged)


def main(argv=None):
    args, interactive = configure(parser().parse_args(argv))
    if sys.platform != "linux":
        raise ValueError("purpwm requires Linux.")
    commands = package_commands(args)
    print(f"\nInstall scope: {args.scope}\nExecutables: {args.bin_dir}\n"
          f"Session defaults: {args.config_dir / 'session.conf'}\n"
          f"Login session: {args.session_dir / 'purpwm.desktop' if args.session == 'yes' else 'disabled'}\n"
          f"Lock PAM: {args.pam_dir / 'purpwm' if args.pam == 'yes' else 'unchanged'}\n"
          f"Keyring login unlock: {args.keyring_pam}\n"
          f"Scale: {args.scale}; refresh: {args.refresh_rate} Hz; resolution: {args.resolution}\n"
          f"Terminal: {args.terminal}; Spotlight: {args.spotlight}; font: {args.font}; wallpaper: {args.wallpaper}\n"
          f"Autostart: {args.autostart}; build: {'reuse binary' if args.no_build else 'release (Cargo.lock)'}")
    for command in commands:
        print("Packages: " + shlex.join(command))
    if args.destdir:
        print(f"Staging root: {args.destdir} (runtime paths remain as shown above)")
    if args.dry_run:
        print("Dry run: no changes made.")
        return
    if interactive and ask("Install now", "yes", ("yes", "no")) == "no":
        return
    if os.geteuid() == 0 and not args.no_build:
        raise ValueError("Run ./install.sh without sudo. It asks sudo only when needed.")
    for command in commands:
        run(command, privileged=True)
    terminal = resolve_program(args.terminal, TERMINALS)
    spotlight = resolve_program(args.spotlight, ("spotlight",))
    font = font_path(args.font)
    print(f"Detected terminal: {terminal or 'not installed'}\n"
          f"Detected Spotlight: {spotlight or 'not installed'}\nFont: {font or 'built-in search paths'}")
    binary = build_binary(args)
    pam = None
    pam_destination = staged_path(args.pam_dir / "purpwm", args)
    if args.pam == "yes" and not pam_destination.exists():
        # Prefer a staged distro PAM stack when one has been supplied.
        directory = staged_path(args.pam_dir, args) if args.destdir and pam_destination.parent.is_dir() else args.pam_dir
        pam = pam_config(directory)
    with tempfile.TemporaryDirectory(prefix="purpwm-install-") as directory:
        stage = Path(directory)
        launcher = (REPO / "start-purpwm").read_text().replace(
            "# INSTALLER_SESSION_CONFIG", "PURPWM_SESSION_CONFIG=${PURPWM_SESSION_CONFIG-"
            + shlex.quote(str(args.config_dir / "session.conf")) + "}")
        (stage / "start-purpwm").write_text(launcher)
        (stage / "session.conf").write_text(session_config(args, terminal, spotlight, font))
        install_file(binary, args.bin_dir / "purpwm", "755", args)
        install_file(stage / "start-purpwm", args.bin_dir / "start-purpwm", "755", args)
        for helper in ("purpwm-wifi", "purpwm-bt", "purpwm-polkit-agent", "purpwm-keyring", "purpwm-portal"):
            install_file(REPO / helper, args.bin_dir / helper, "755", args)
        install_file(stage / "session.conf", args.config_dir / "session.conf", "644", args, backup=True)
        if pam is not None:
            (stage / "pam").write_text(pam)
            install_file(stage / "pam", args.pam_dir / "purpwm", "644", args)
        if args.session == "yes":
            (stage / "purpwm.desktop").write_text(desktop_entry(args.bin_dir / "start-purpwm"))
            install_file(stage / "purpwm.desktop", args.session_dir / "purpwm.desktop", "644", args)
    if args.keyring_pam == "yes":
        # The login (greeter) PAM stack, separate from the screen-lock service above.
        run([sys.executable, REPO / "scripts/configure-keyring-pam.py"], privileged=True)
    print(f"\npurpwm installed. Session settings: {args.config_dir / 'session.conf'}")
    if args.destdir:
        print("Files were staged only; the live system was not installed.")
    else:
        if not terminal:
            print("No terminal found. Install one or set PURPWM_TERMINAL in session.conf.")
        if not spotlight:
            print("Spotlight not found. Install it separately or set PURPWM_SPOTLIGHT to its executable.")
        if args.session == "yes":
            if not shutil.which("gamescope"):
                print("Install gamescope before logging in to the standalone session.")
            if args.scope == "user":
                print("Many display managers ignore user session directories; use --system if purpwm isn't listed.")
            print("Log out and select purpwm at the login screen. Quit with Mod + Shift + q.")
        else:
            print(f"Try it nested: {shlex.quote(str(args.bin_dir / 'purpwm'))} --no-autostart")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"purpwm setup: {error}", file=sys.stderr)
        sys.exit(1)
    except (KeyboardInterrupt, EOFError):
        print("\nSetup cancelled.", file=sys.stderr)
        sys.exit(1)
