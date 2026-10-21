# Installation

## Guided setup

```bash
git clone https://github.com/Maglcite/purpwm.git
cd purpwm
./install.sh
```

Run the wizard as your regular user. It offers a system installation (for a login-screen
session) or a user installation, lets you change the paths and session preferences,
optionally installs dependencies, builds with `Cargo.lock`, and installs the binary
and helpers. Only package installation and system file installation use `sudo`.
Python 3.9+ must already be installed. If Rust/Cargo is missing, a dependency profile
also installs the distro toolchain. Use a current stable Rust if your distro's version
is too old to compile the project.

Automatic dependency profiles are provided for Arch/CachyOS (`pacman`), Debian/Ubuntu
(`apt-get`), and Fedora (`dnf`). `build` installs development libraries; `desktop` adds
gamescope, a terminal, networking/audio tools, fonts, and the keyring/polkit runtime.
An existing Rust installation is kept. Other distributions can install the requirements
below manually and select `none`. Dependency installation does not enable or replace
system services; NetworkManager, Bluetooth, and an audio server must be configured
for their respective features. Spotlight is installed separately; select its executable
or `none` to disable it. Some Debian/Ubuntu repositories lack gamescope; setup reports
this and it must be installed before using a standalone login session.

The wizard offers:

| Setting | Options/default |
| :--- | :--- |
| Scope | `system` (wizard default) or `user` |
| Prefix/executables | `/usr/local/bin` system; `~/.local/bin` user; custom paths allowed |
| Session defaults | `PREFIX/share/purpwm/session.conf` system; `$XDG_CONFIG_HOME/purpwm/session.conf` user |
| Login session | System default `/usr/share/wayland-sessions/purpwm.desktop`; optional |
| Lock PAM | Detects the distro auth/account stack; preserves an existing `purpwm` service |
| Dependencies | `none`, `build`, or `desktop` |
| Terminal/Spotlight/font | Automatic detection or an executable/font path; Spotlight can be disabled |
| Display | Scale `1.0`–`3.0`, native or explicit resolution, refresh rate (default `120` Hz) |
| Autostart | Whether to start session helpers |

The executable, configuration, and session directories can each be overridden.
`XDG_CONFIG_HOME` defaults to `~/.config`; `XDG_DATA_HOME` defaults to `~/.local/share`.
Many display managers ignore user session directories, so choose `system` to have
purpwm listed reliably on the login screen.

### Scripted installation

```bash
# Review a plan without writing files, installing packages, or building.
./install.sh --system --dry-run

# Install for your user, without prompts or package changes.
./install.sh --user --non-interactive --scale 1.25 --spotlight none

# Install for all users, choosing preferences and dependency installation.
./install.sh --system --non-interactive --dependencies desktop \
    --prefix /opt/purpwm --terminal foot --spotlight /path/to/spotlight \
    --scale 1.5 --refresh-rate 144 --resolution auto

# Reuse a prebuilt binary and stage files for packaging (no live system changes).
./install.sh --system --non-interactive --no-build --binary ./target/release/purpwm \
    --dependencies none --destdir /tmp/purpwm-package

./install.sh --help
```

Noninteractive installs default to `user`, no package installation, and no login
session/PAM changes unless `--system` is selected. `--yes` aliases `--non-interactive`.
`--no-build` uses the checkout's release binary (honoring `CARGO_TARGET_DIR`) unless
`--binary` is supplied. Normal builds use Cargo's reported artifact path, including
custom Cargo target directories. A first backup of an overwritten `session.conf` is
saved as `session.conf.bak`. Theme, layouts and keybindings remain compile-time settings.

## Requirements

**Build**

- Rust (stable, edition 2021) and Cargo
- Python 3.9+ for the installer
- A C compiler, `pkg-config`, and libraries used by Smithay: `wayland`, `libxkbcommon`, `mesa` (EGL / GLES2 / GBM), `libinput`, `libseat`, udev, and PAM development/linker files

**Runtime (optional, each feature degrades gracefully when missing)**

| Feature | Uses |
| :--- | :--- |
| Standalone login session | `gamescope` (hosts purpwm full-screen at native resolution) |
| Terminal / scratchpad | `alacritty`, `foot`, `kitty`, `wezterm`, `cosmic-term`, `gnome-terminal`, ... (first found) |
| Launcher | Spotlight (`spotlight --toggle`), see [Configuration](configuration.md#commands) |
| Wi-Fi pill & manager | NetworkManager (`nmcli`, `nmtui`, `nm-connection-editor`) |
| Bluetooth pill & manager | BlueZ (`bluetoothctl`), `rfkill` |
| Volume pill & keys | PipeWire `wpctl` (or `pactl`), `pavucontrol` for the mixer |
| Brightness keys | `brightnessctl`, or logind via `busctl` (no root needed) |
| Media keys | `playerctl` |
| System monitor | `btop` or `top` |
| Polkit dialog | Python 3, PyGObject, GTK 4, `polkit` (GI typelibs `Polkit-1.0`, `PolkitAgent-1.0`) |
| Keyring | `gnome-keyring-daemon` |
| Status bar font | Any TTF from `FONT_PATHS` (JetBrains Mono, DejaVu Sans Mono, ...) |

On Arch / CachyOS, for example:

```bash
sudo pacman -S --needed rust wayland libxkbcommon mesa libinput seatd gamescope \
    alacritty networkmanager bluez-utils pipewire wireplumber pavucontrol playerctl \
    btop python-gobject gtk4 polkit gnome-keyring ttf-jetbrains-mono
```

## Building

```bash
git clone https://github.com/Maglcite/purpwm.git
cd purpwm
cargo build --release
```

The release profile uses thin LTO, a single codegen unit and stripping; the binary is
`target/release/purpwm` (about 5–6 MB).

Run the tests with `cargo test`.
Run installer integration tests with `python3 -m unittest discover -s scripts -p 'test_installer.py'`.
They install only into temporary directories; `gio` and `desktop-file-validate` add
Desktop Entry launch/validation coverage when installed.

## Running nested (trying it out)

Inside an existing Wayland or X11 session:

```bash
./target/release/purpwm --no-autostart
```

`--no-autostart` skips everything that touches the rest of your desktop: starting the
Spotlight daemon, the polkit agent, autostart programs and turning on Wi-Fi/Bluetooth.
Without it, purpwm behaves like a full session.

Start a client inside it by pointing `WAYLAND_DISPLAY` at the socket purpwm prints on
startup (`purpwm listening on WAYLAND_DISPLAY=wayland-2 ...`):

```bash
WAYLAND_DISPLAY=wayland-2 alacritty
```

Command-line options:

| Option | Effect |
| :--- | :--- |
| `--standalone` / `--fullscreen` | Borderless full-screen at native resolution (also `PURPWM_STANDALONE=1`) |
| `--no-autostart` | Don't start daemons / helpers (see above) |
| `--spawn-terminal` | Open a terminal on startup |
| `-h`, `--help` | Print usage and keybindings |

## Installing as a login session

```bash
./install.sh --system
```

With the default directories this installs:

| File | Purpose |
| :--- | :--- |
| `/usr/local/bin/purpwm` | The compositor |
| `/usr/local/bin/start-purpwm` | Session launcher (environment, keyring, D-Bus, gamescope) |
| `/usr/local/bin/purpwm-wifi`, `purpwm-bt` | Wi-Fi / Bluetooth manager helpers |
| `/usr/local/bin/purpwm-polkit-agent` | Themed polkit authentication dialog |
| `/usr/local/share/purpwm/session.conf` | Selected session defaults |
| `/etc/pam.d/purpwm` | Screen-lock authentication; created only if absent |
| `/usr/share/wayland-sessions/purpwm.desktop` | Session entry shown by display managers |

Choose **purpwm** on your display manager's session menu and log in.
The older `sudo ./install-session.sh` entry point still installs a prebuilt release
with system defaults; use `./install.sh` for the wizard and automatic builds.
The installer never modifies your display manager's login PAM services.

### What `start-purpwm` does

1. Loads the installation's `session.conf`, followed by a per-user override at
   `${XDG_CONFIG_HOME:-~/.config}/purpwm/session.conf` when different.
2. Sets `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS` and the usual Wayland session
   variables (`XDG_CURRENT_DESKTOP=purpwm`, `QT_QPA_PLATFORM`, `GDK_BACKEND`, `MOZ_ENABLE_WAYLAND`, ...).
3. Starts `gnome-keyring-daemon` (secrets, pkcs11, ssh) and exports `SSH_AUTH_SOCK`.
4. Imports the environment into the systemd user session.
5. Turns on networking, Wi-Fi and Bluetooth in the background. `--no-autostart` or
   `PURPWM_AUTOSTART=0` skips keyring startup, environment import, radios, and compositor helpers.
6. Runs the binary next to the installed launcher, or a release binary in the same
   checkout when uninstalled. `PURPWM_BIN` can select an explicit binary. Helpers are
   resolved from the launcher's directory first, then the rest of `PATH`.
7. Nested (`WAYLAND_DISPLAY` or `DISPLAY` set): runs purpwm directly. Otherwise detects
   the panel's native mode from `/sys/class/drm` and runs purpwm `--standalone` inside
   `gamescope` at the configured resolution and refresh rate. Without gamescope it
   reports the missing prerequisite instead of trying to open a nested backend on a VT.

The installed session works after moving or deleting the source checkout.
Edit `session.conf` to change terminal/Spotlight paths and display defaults without
rebuilding; see [session settings](configuration.md#session-settings).

## Updating

```bash
git pull
./install.sh
```

Choose the same install directories/preferences, then log out and back in. For a
scripted update, repeat the original noninteractive install command. The installer
replaces executables by rename so it can update files used by a running session.
