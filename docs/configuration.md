# Configuration

Like `dwm`, purpwm is configured at compile time. Edit [`src/config.rs`](../src/config.rs)
and rebuild:

```bash
cargo build --release
```

Then log out and back in (or restart the nested instance).

The only runtime settings are the UI scale (below) and the `PURPWM_STANDALONE` /
`PURPWM_SCALE` environment variables.

## Scale (HiDPI)

| Constant | Default | Meaning |
| :--- | :--- | :--- |
| `DEFAULT_SCALE` | `1.5` | Logical-to-physical pixel ratio. `1.0` = native pixels, `2.0` = everything twice as big |
| `SCALE_STEP` | `0.25` | Step for `Mod + Ctrl + =` / `Mod + Ctrl + -` |
| `SCALE_MIN`, `SCALE_MAX` | `1.0`, `3.0` | Bounds for the keybindings |

The scale actually used at startup is, in order:

1. `PURPWM_SCALE` environment variable (e.g. `PURPWM_SCALE=1.75`)
2. `~/.config/purpwm/scale` (written whenever you change the scale with the keybindings)
3. `DEFAULT_SCALE`

Apps that support `wp_fractional_scale_v1` (GTK 4, Qt 6, Firefox, Chromium/Electron, winit
apps such as Alacritty) render at exactly this scale. Others are asked for the next integer
buffer scale and are downsampled, so text stays sharp either way.

All other sizes in this file are in **logical** pixels and get multiplied by the scale.

## Layout and geometry

| Constant | Default | Meaning |
| :--- | :--- | :--- |
| `BAR_HEIGHT` | `28` | Status bar height |
| `BAR_FONT_SIZE` | `13.5` | Status bar font size |
| `BORDER_WIDTH` | `2` | Window border width |
| `GAP_INNER` / `GAP_OUTER` | `6` / `8` | Gaps between windows / to the screen edges |
| `DEFAULT_MFACT` | `0.55` | Master area width (0.1–0.9) |
| `DEFAULT_NMASTER` | `1` | Windows in the master area |
| `MOVE_STEP` / `RESIZE_STEP` | `40` | Pixels per `Mod + Arrow` / `Mod + Shift + Arrow` |
| `TAG_NAMES` | `"1"`…`"9"` | Tag labels shown in the bar |
| `FONT_PATHS` | JetBrains Mono, DejaVu Sans Mono, ... | TTF files tried in order for the bar |

## Theme

Window colours are `Color32F` (RGBA floats, `0.0..=1.0`); bar colours are `[u8; 4]` RGBA.

| Constant | Used for |
| :--- | :--- |
| `COLOR_BG_ROOT` | Desktop background |
| `COLOR_BORDER_ACTIVE` / `COLOR_BORDER_INACTIVE` | Focused / unfocused border |
| `COLOR_BORDER_GLOW` | 1 px halo around the focused window |
| `OPACITY_ACTIVE` / `OPACITY_INACTIVE` | Window opacity when focused / unfocused |
| `BAR_BG_RGBA`, `BAR_ACCENT_LINE_RGBA` | Bar background and bottom line |
| `BAR_TAG_ACTIVE_RGBA`, `BAR_LAYOUT_BG_RGBA` | Selected tag, pills |
| `BAR_ACCENT_RGBA`, `BAR_TEXT_FG_RGBA`, `BAR_TEXT_DIM_RGBA` | Accent, normal and dimmed text |

## Window rules

`WINDOW_RULES` is checked once, as soon as a window sets its `app_id` or title. The first
matching rule wins.

```rust
WindowRule {
    app_id_match: Some("pavucontrol"), // substring of the Wayland app_id (None = any)
    title_match: None,                 // substring of the title (None = any)
    tags: 0,                           // tag bitmask to put it on (0 = current tags)
    is_floating: true,                 // float it, centred
    opacity: None,                     // Some(1.0) = always fully opaque
},
```

A rule matches if **either** the `app_id` or the title substring matches. Windows floated by a
rule open at the size the app asks for (the frame follows the app until you resize it
yourself), and new floating windows are cascaded so they never sit exactly on top of
each other. Find an app's
`app_id` from its `.desktop` file name, or from the bar title while it is focused.

Built-in rules float Spotlight, purpwm's own helper windows, polkit dialogs, file pickers,
`pavucontrol`, `nm-connection-editor`, `blueman-manager` and COSMIC Settings, and make
`mpv`, `imv` and `vlc` fully opaque.

## Commands

Each command list is tried in order; the first program found on `PATH` (or at the given
absolute path) is started. Children get the session's `WAYLAND_DISPLAY`,
`XDG_RUNTIME_DIR` and D-Bus address.

| Constant | Triggered by |
| :--- | :--- |
| `LAUNCHER_COMMANDS` | Tap `Mod`, `Mod + p` |
| `TERMINAL_COMMANDS` | `Mod + Return`, scratchpad |
| `SYSTEM_MONITOR_COMMANDS` | `Mod + Escape`, clicking bar metrics |
| `WIFI_COMMANDS` / `BLUETOOTH_COMMANDS` | Wi-Fi / Bluetooth pills and keys |
| `MIXER_COMMANDS` | Right-click on the `VOL` pill |
| `VOL_*`, `MIC_MUTE_COMMANDS` | Volume keys and the `VOL` pill |
| `BRIGHTNESS_UP/DOWN_COMMANDS` | Brightness keys (logind fallback if none found) |
| `MEDIA_*_COMMANDS` | Media keys |
| `POLKIT_COMMANDS` | Started once at login |
| `AUTOSTART_COMMANDS` | Started once at login if installed (e.g. `mako`, `dunst`) |
| `SCREENSHOT_DIR` | Screenshot folder, relative to `$HOME` |

The launcher daemon itself is pre-started by `PurpState::spawn_spotlight_daemon` in
[`src/state.rs`](../src/state.rs); change the paths there if your launcher lives elsewhere.

## Helper scripts

| Script | What it does |
| :--- | :--- |
| `purpwm-wifi` | Turns Wi-Fi on, rescans, and opens `nmtui` in a floating terminal (falls back to `nm-connection-editor` / COSMIC Settings) |
| `purpwm-bt` | Terminal Bluetooth control centre: list, connect / disconnect, scan, pair & trust, power |
| `purpwm-polkit-agent` | GTK 4 password dialog implementing the polkit authentication-agent D-Bus interface; retries on a wrong password and closes if the requester exits |
