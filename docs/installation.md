# Installation

## Requirements

**Build**

- Rust (stable, edition 2021) and Cargo
- System libraries used by Smithay's winit/EGL backend: `wayland`, `libxkbcommon`, `mesa` (EGL / GLES2), `libinput`, `libseat`, `systemd-libs` (udev)

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
git clone <this repository> purpwm
cd purpwm
cargo build --release
```

The release profile uses thin LTO, a single codegen unit and stripping; the binary is
`target/release/purpwm` (about 5–6 MB).

Run the tests with `cargo test`.

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
sudo ./install-session.sh
```

This installs:

| File | Purpose |
| :--- | :--- |
| `/usr/local/bin/purpwm` | The compositor |
| `/usr/local/bin/start-purpwm` | Session launcher (environment, keyring, D-Bus, gamescope) |
| `/usr/local/bin/purpwm-wifi`, `purpwm-bt` | Wi-Fi / Bluetooth manager helpers |
| `/usr/local/bin/purpwm-polkit-agent` | Themed polkit authentication dialog |
| `/usr/share/wayland-sessions/purpwm.desktop` | Session entry shown by display managers |

Choose **purpwm** on your display manager's session menu and log in.

### What `start-purpwm` does

1. If a project checkout exists at the path hard-coded at its top, re-executes that copy,
   so script changes in the checkout take effect without reinstalling.
2. Sets `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS` and the usual Wayland session
   variables (`XDG_CURRENT_DESKTOP=purpwm`, `QT_QPA_PLATFORM`, `GDK_BACKEND`, `MOZ_ENABLE_WAYLAND`, ...).
3. Starts `gnome-keyring-daemon` (secrets, pkcs11, ssh) and exports `SSH_AUTH_SOCK`.
4. Imports the environment into the systemd user session.
5. Turns on networking, Wi-Fi and Bluetooth in the background.
6. Picks the binary: the checkout's `target/release/purpwm`, then `~/.local/bin/purpwm`,
   then `/usr/local/bin/purpwm`.
7. Nested (`WAYLAND_DISPLAY` or `DISPLAY` set): runs purpwm directly. Otherwise detects
   the panel's native mode from `/sys/class/drm` and runs purpwm `--standalone` inside
   `gamescope` at that resolution and 120 Hz.

> **Note:** `start-purpwm`, `install-session.sh` and `src/config.rs` contain absolute paths
> under `/home/mag`. Change them to your own checkout location before installing.

## Updating

```bash
git pull
cargo build --release
```

Log out and back in. Re-run `sudo ./install-session.sh` only if you want the system-wide
copies refreshed (the session launcher already prefers the checkout's build).
