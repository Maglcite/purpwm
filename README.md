# `purpwm` — Translucent Purple `dwm`-Inspired Wayland Compositor in Rust

`purpwm` is an ultra-lightweight, daily-driver dynamic tiling Wayland compositor written in Rust on top of **Smithay (`0.7`)**. It brings `dwm`'s classic 9-tag bitmask workflow, **Per-Tag (`pertag`) layouts**, **5 layout modes**, a **drop-down Scratchpad terminal**, **built-in Wi-Fi & Bluetooth status pills and managers**, **native OS Keyring & Polkit authentication**, **compile-time window rules**, and `src/config.rs` configuration philosophy to Wayland with native OpenGL ES (`GlesRenderer`) alpha transparency, a glowing purple aesthetic, and native `wlr-layer-shell` integration for **`Spotlight`** (`/home/mag/Documents/Spotlight`).

---

## Daily-Driver Features & Optimizations

- **Native Resolution (`2560x1600@120Hz`) & High-Contrast Purple Cursor**:
  - Automatically detects your connected DRM panel's native resolution (`/sys/class/drm/card*-*/modes`, `2560x1600`) and runs at full native resolution with a `20x26` high-contrast white/lavender/purple software cursor that is always visible.
- **Ultra-Low RAM & CPU Usage (Aggressive Memory Optimizations)**:
  - **Event-Driven Frame Loop**: Zero CPU spin when idle. Renders frames only upon user input, client buffer commits, or status updates, sleeping with responsive 8ms throttling otherwise.
  - **Active Kernel Memory Reclamation (`malloc_trim`)**: Automatically calls `libc::malloc_trim(0)` upon window close and periodically every 30 seconds, returning unused heap allocations directly to the Linux kernel.
  - **Direct `/proc` and `/sys` Metric Sampling**: System status checks read `/proc/loadavg`, `/proc/meminfo`, and `/sys/class/power_supply` without spawning child shell forks, eliminating subprocess RAM spikes.
- **Integrated OS Keyring & Themed Polkit Authentication Agent**:
  - Automatically initializes `gnome-keyring-daemon` on login to store and unlock Wi-Fi, SSH, and secret-service passwords via standard D-Bus (`org.freedesktop.secrets`).
  - Bundled with **`purpwm-polkit-agent`**, a custom lightweight GTK4 + PolkitAgent dialog matching the translucent purple aesthetic to authenticate administrative tasks (e.g., package management, disk partitioning, Wi-Fi keys).
- **Built-In Wi-Fi & Bluetooth Support**:
  - Automatically connects to saved Wi-Fi networks (`NetworkManager` + `gnome-keyring` on the real systemd user D-Bus `/run/user/$UID/bus`) and powers on Bluetooth at session startup.
  - **Live Clickable Top-Bar Pills**:
    - **`[ WIFI: <SSID> ]`**: Left-click (or `Mod + w`) opens the floating Wi-Fi Manager (`purpwm-wifi` / `nmtui`); Right-click (or `Mod + Shift + w`) toggles Wi-Fi radio On/Off.
    - **`[ BT: <Device/On> ]`**: Left-click (or `Mod + Shift + b`) opens the interactive floating Bluetooth Control Center (`purpwm-bt`); Right-click (or `Mod + Ctrl + b`) toggles Bluetooth power On/Off.
- **Interactive System Monitor**:
  - Left-click on the `CPU:`, `MEM:`, or `BAT:` section of the top status bar (or press `Mod + Escape`) to launch `btop` / `top` in a floating purple terminal.
- **Native `Spotlight` App Launcher Integration (`/home/mag/Documents/Spotlight`)**:
  - Automatically pre-starts `Spotlight --daemon` bound to `purpwm`'s Wayland display.
  - Tapping `Mod` alone (or pressing `Mod + p`) toggles `Spotlight` (`--toggle`) centered over your workspace via `zwlr_layer_shell_v1` with exclusive keyboard focus.
- **5 Dynamic Layouts + `dwm` Per-Tag State (`pertag`)**:
  - **Tile (`[]=`)**, **CenteredMaster (`|M|`, `Mod + u`)**, **Grid (`###`, `Mod + g`)**, **Monocle (`[M]`, `Mod + m`)**, and **Floating (`><>`, `Mod + f`)**.
  - Each tag `1..9` independently remembers its own layout, `mfact`, and `nmaster`.
- **Drop-Down Scratchpad Terminal (`Mod + s` or ``Mod + ` ``)**:
  - Summons or hides a centered floating terminal (`70% x 70%`) across any workspace tag.

---

## Keybindings & Mouse Controls

*(Note: `Mod` accepts both `Super`/`Logo` and `Alt`.)*

| Shortcut / Click | Action |
| :--- | :--- |
| **Tap `Mod` (Press & Release alone)** | **Toggle `Spotlight` App Launcher** (`/home/mag/Documents/Spotlight`) |
| **`Mod + p`** | **Toggle `Spotlight` App Launcher** (`/home/mag/Documents/Spotlight`) |
| **`Mod + Return`** | **Spawn Terminal** (`alacritty`, `foot`, `kitty`, etc.) |
| **`Mod + Escape` / Click `CPU:` / `MEM:` / `BAT:`** | **Open System Monitor** (`btop` / `top`) |
| **`Mod + w` / Left-Click `WIFI:` Pill** | **Open Floating Wi-Fi Manager** (`purpwm-wifi` / `nmtui`) |
| **`Mod + Shift + w` / Right-Click `WIFI:` Pill** | **Toggle Wi-Fi Radio On/Off** |
| **`Mod + Shift + b` / Left-Click `BT:` Pill** | **Open Floating Bluetooth Manager** (`purpwm-bt`) |
| **`Mod + Ctrl + b` / Right-Click `BT:` Pill** | **Toggle Bluetooth Power On/Off** |
| **`Mod + s` / ``Mod + ` ``** | **Toggle Drop-Down Scratchpad Terminal** |
| **`Mod + Arrow Keys` ($\leftarrow, \rightarrow, \uparrow, \downarrow$)** | **Move focused window** (swaps window in tiled stack; nudges by `40px` in floating mode) |
| **`Mod + Shift + Arrow Keys`** | **Resize focused window** ($\pm 40\text{px}$ width/height; automatically floats window) |
| `Mod + j` / `Mod + k` | Focus next / previous window in stack |
| `Mod + h` / `Mod + l` | Decrease / increase master width factor (`mfact` $\pm 0.05$) |
| `Mod + i` / `Mod + d` | Increment / decrement number of master windows (`nmaster`) |
| `Mod + Shift + Return` | Zoom: swap focused window with the Master slot |
| `Mod + t` / `Mod + m` / `Mod + f` | Switch layout: Tile (`[]=`), Monocle (`[M]`), Floating (`><>`) |
| `Mod + u` / `Mod + g` | Switch layout: CenteredMaster (`\|M\|`), Grid (`###`) |
| `Mod + Space` | Toggle between current and previous layout |
| `Mod + Shift + Space` | Toggle floating mode on focused window |
| `Mod + Shift + f` / `F11` | Toggle true Fullscreen on focused window |
| `Mod + o` | Toggle 100% Opaque mode on focused window |
| `Mod + =` / `Mod + -` | Increase / decrease vanity gaps on the fly (`\pm 2px`) |
| `Mod + Shift + g` | Toggle vanity gaps on/off |
| `Mod + Ctrl + 0` | Reset vanity gaps to default |
| `Mod + b` | Toggle top status bar visibility |
| `Mod + 1..9` | View tag `1..9` (restores per-tag layout & `mfact`) |
| `Mod + Shift + 1..9` | Move focused window to tag `1..9` |
| `Mod + Shift + c` | Close focused window |
| `Mod + Shift + q` | Quit `purpwm` |
| `Mod + Left-Drag` / `Right-Drag` | Interactively float and move / resize window under pointer |
| `Scroll Wheel on Status Bar` | Cycle through workspace tags `1..9` |

---

## Updating Your COSMIC Greeter Sign-In Session

Run the installer script once with `sudo` so `/usr/local/bin/start-purpwm`, `/usr/local/bin/purpwm`, `/usr/local/bin/purpwm-wifi`, `/usr/local/bin/purpwm-bt`, and `/usr/local/bin/purpwm-polkit-agent` are updated:

```bash
sudo /home/mag/Documents/purpwm/install-session.sh
```
*(Note: `/usr/local/bin/start-purpwm` automatically delegates to `/home/mag/Documents/purpwm/start-purpwm` and `/home/mag/Documents/purpwm/target/release/purpwm` after login, so all updates take effect immediately without needing `sudo` every time!)*
