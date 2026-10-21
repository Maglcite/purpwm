# purpwm

A small, `dwm`-inspired dynamic tiling **Wayland compositor** written in Rust on top of
[Smithay](https://github.com/Smithay/smithay) `0.7`, with a translucent purple theme.

- **dwm workflow**: 9 bitmask tags, master/stack tiling, per-tag layouts (`pertag`), `mfact` / `nmaster`, zoom, scratchpad.
- **5 layouts**: Tile `[]=`, Centered Master `|M|`, Grid `###`, Monocle `[M]`, Floating `><>`.
- **Translucent purple look**: glass-like window opacity, glowing focus borders, built-in status bar, system XCursor theme with purple fallback, and `Mod + h` keybinding cheat sheet overlay.
- **HiDPI**: sharp fractional scaling (default `1.5x`), adjustable live.
- **Native touch support**: full `wl_touch` forwarding, tap-to-focus windows, status-bar touch controls, and `Mod + Touch` window dragging.
- **Built-in PAM lock screen**: `Mod + Shift + l` (or `Mod + Ctrl + l` / `kill -USR2`) locks the session with a full-screen purple PAM locker.
- **Built-in notifications**: implements `org.freedesktop.Notifications` on D-Bus and renders translucent purple toast cards (click or tap to dismiss).
- **Daily-driver bits**: clickable Wi-Fi / Bluetooth / volume pills, system metrics, media / volume / brightness keys, built-in screenshots, clipboard + `wlr-data-control`, `idle-inhibit`, drag-and-drop icons, popup menus, polkit authentication dialog, [OS keyring with login unlock](docs/installation.md#os-keyring).
- **Launcher integration**: tap `Mod` to open the Spotlight launcher (any `wlr-layer-shell` launcher works).
- **Configured in code** like `dwm`'s `config.h`: edit [`src/config.rs`](src/config.rs) and rebuild.

## Quick start

```bash
git clone https://github.com/Maglcite/purpwm.git
cd purpwm
./install.sh
```

The installer asks for user/system installation, directories, dependencies, terminal,
Spotlight path, scale, resolution, refresh rate, and autostart. It builds the release
binary and generates the session entry with your paths. Run it as your normal user;
it uses `sudo` for system files and requested packages. Requires Python 3.9+ and a
current stable Rust toolchain (or install the distro toolchain through setup).

Choose a system installation to make **purpwm** available on the login screen.
Quit with **`Mod + Shift + q`**. To preview setup without changing anything:

```bash
./install.sh --system --dry-run
```

For flags, user installs, and manual builds, see [Installation](docs/installation.md).

`Mod` is **Super** or **Alt**. Press **`Mod + h`** anytime to bring up the keybinding cheat sheet; tapping `Mod` alone opens the launcher; `Mod + Return` opens a terminal.

## Documentation

| Guide | Contents |
| :--- | :--- |
| [Installation](docs/installation.md) | Requirements, building, login-session setup, helper scripts |
| [Keybindings](docs/keybindings.md) | Every keyboard shortcut, touch gesture, and mouse / status-bar action |
| [Configuration](docs/configuration.md) | `src/config.rs`: scale, theme, gaps, rules, commands |
| [Architecture](docs/architecture.md) | How the code is organised, rendering and input pipelines |
| [Troubleshooting](docs/troubleshooting.md) | Common problems, logs, testing safely |

## Status

purpwm is a personal daily-driver project. It runs as a nested winit client, either
inside another compositor or full-screen inside `gamescope` when launched from a
display manager; it does not drive DRM/KMS directly. Installed helpers are found on
`PATH`; session preferences can be changed without rebuilding. Theme, layouts, and
keybindings are configured in `src/config.rs`.

## License

[MIT](LICENSE)
