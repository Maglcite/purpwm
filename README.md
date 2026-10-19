# purpwm

A small, `dwm`-inspired dynamic tiling **Wayland compositor** written in Rust on top of
[Smithay](https://github.com/Smithay/smithay) `0.7`, with a translucent purple theme.

- **dwm workflow**: 9 bitmask tags, master/stack tiling, per-tag layouts (`pertag`), `mfact` / `nmaster`, zoom, scratchpad.
- **5 layouts**: Tile `[]=`, Centered Master `|M|`, Grid `###`, Monocle `[M]`, Floating `><>`.
- **Translucent purple look**: glass-like window opacity, glowing focus borders, built-in status bar, high-contrast cursor.
- **HiDPI**: sharp fractional scaling (default `1.5x`), adjustable live.
- **Daily-driver bits**: clickable Wi-Fi / Bluetooth / volume pills, system metrics, media / volume / brightness keys, built-in screenshots, clipboard, popup menus, polkit authentication dialog.
- **Launcher integration**: tap `Mod` to open the Spotlight launcher (any `wlr-layer-shell` launcher works).
- **Configured in code** like `dwm`'s `config.h`: edit [`src/config.rs`](src/config.rs) and rebuild.

## Quick start

```bash
cargo build --release

# Try it nested inside your current Wayland/X11 session
./target/release/purpwm --no-autostart

# Install as a login session for your display manager (COSMIC greeter, GDM, SDDM, ...)
sudo ./install-session.sh
```

Then pick **purpwm** on the login screen. Quit with **`Mod + Shift + q`**.

`Mod` is **Super** or **Alt**. Tapping `Mod` alone opens the launcher; `Mod + Return` opens a terminal.

## Documentation

| Guide | Contents |
| :--- | :--- |
| [Installation](docs/installation.md) | Requirements, building, login-session setup, helper scripts |
| [Keybindings](docs/keybindings.md) | Every keyboard shortcut and mouse / status-bar action |
| [Configuration](docs/configuration.md) | `src/config.rs`: scale, theme, gaps, rules, commands |
| [Architecture](docs/architecture.md) | How the code is organised, rendering and input pipelines |
| [Troubleshooting](docs/troubleshooting.md) | Common problems, logs, testing safely |

## Status

purpwm is a personal daily-driver project. It runs as a nested winit client, either
inside another compositor or full-screen inside `gamescope` when launched from a
display manager; it does not drive DRM/KMS directly. Several defaults (Spotlight
paths, helper script locations) point at the author's home directory and are meant
to be edited in `src/config.rs`.

## License

[MIT](LICENSE)
