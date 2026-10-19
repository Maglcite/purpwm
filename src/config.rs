//! Compile-time configuration for `purpwm` (inspired by `dwm`'s `config.h`).
//!
//! Edit this file and recompile (`cargo build --release`) to customize your
//! translucent purple aesthetic, fonts, gaps, tags, window rules, and commands.

use smithay::backend::renderer::Color32F;

/// Default output scale (HiDPI). `1.0` = native panel pixels; `1.5` makes everything 50% bigger
/// while staying sharp (apps render at the higher resolution). Change it live with
/// `Mod + Ctrl + =` / `Mod + Ctrl + -` (remembered in `~/.config/purpwm/scale`), or override a
/// single launch with `PURPWM_SCALE=1.75`.
pub const DEFAULT_SCALE: f64 = 1.5;

/// Step and bounds for the runtime scale keybindings.
pub const SCALE_STEP: f64 = 0.25;
pub const SCALE_MIN: f64 = 1.0;
pub const SCALE_MAX: f64 = 3.0;

/// Height of the built-in top status bar in logical pixels (multiplied by the output scale).
pub const BAR_HEIGHT: i32 = 28;

/// Width of window borders in logical pixels (`dwm`-style `borderpx`).
pub const BORDER_WIDTH: i32 = 2;

/// Inner gap between adjacent tiled windows in logical pixels.
pub const GAP_INNER: i32 = 6;

/// Outer gap between tiled windows and the screen/bar edges in logical pixels.
pub const GAP_OUTER: i32 = 8;

/// Default master area width factor (`0.05..=0.95`, `dwm` `mfact`).
pub const DEFAULT_MFACT: f32 = 0.55;

/// Default number of windows in the master area (`dwm` `nmaster`).
pub const DEFAULT_NMASTER: usize = 1;

/// Step in pixels when moving a floating window with `Mod + Arrow` keys.
pub const MOVE_STEP: i32 = 40;

/// Step in pixels when resizing a window with `Mod + Shift + Arrow` keys.
pub const RESIZE_STEP: i32 = 40;

// ============================================================================
// TRANSLUCENT PURPLE THEME PALETTE
// ============================================================================

/// Deep obsidian-purple compositor background (`#11081F` @ 86% opacity).
pub const COLOR_BG_ROOT: Color32F = Color32F::new(0.067, 0.031, 0.122, 0.86);

/// Active window border: vibrant electric purple (`#A855F7` @ 96% opacity).
pub const COLOR_BORDER_ACTIVE: Color32F = Color32F::new(0.659, 0.333, 0.969, 0.96);

/// Outer 1px glow halo around the active window border (`#C084FC` @ 42% opacity).
pub const COLOR_BORDER_GLOW: Color32F = Color32F::new(0.753, 0.518, 0.988, 0.42);

/// Inactive window border: muted dark plum (`#3B1F5E` @ 72% opacity).
pub const COLOR_BORDER_INACTIVE: Color32F = Color32F::new(0.231, 0.122, 0.369, 0.72);

/// Opacity multiplier applied to the currently focused window (`0.94` for subtle purple glass).
pub const OPACITY_ACTIVE: f32 = 0.94;

/// Opacity multiplier applied to unfocused windows (`0.84` for deeper translucency).
pub const OPACITY_INACTIVE: f32 = 0.84;

// ============================================================================
// STATUS BAR PALETTE (RGBA in 0..=255 for CPU rasterizer)
// ============================================================================

/// Bar background: translucent smoked violet (`#170B29` @ 82% alpha).
pub const BAR_BG_RGBA: [u8; 4] = [0x17, 0x0B, 0x29, 210];

/// Active tag / pill background: rich royal purple (`#7C3AED` @ 92% alpha).
pub const BAR_TAG_ACTIVE_RGBA: [u8; 4] = [0x7C, 0x3A, 0xED, 235];

/// Secondary pill background (layout indicator / status pill): (`#2A1447` @ 88% alpha).
pub const BAR_LAYOUT_BG_RGBA: [u8; 4] = [0x2A, 0x14, 0x47, 224];

/// Bottom 1px highlight line along the bar: glowing amethyst (`#9333EA` @ 90% alpha).
pub const BAR_ACCENT_LINE_RGBA: [u8; 4] = [0x93, 0x33, 0xEA, 230];

/// Bright lavender-purple accent for symbols & active pills (`#C084FC`).
pub const BAR_ACCENT_RGBA: [u8; 4] = [0xC0, 0x84, 0xFC, 255];

/// Primary bar foreground text: crisp lavender-white (`#F3E8FF`).
pub const BAR_TEXT_FG_RGBA: [u8; 4] = [0xF3, 0xE8, 0xFF, 255];

/// Dimmed bar foreground text for unoccupied tags: muted lavender (`#8B7AA8`).
pub const BAR_TEXT_DIM_RGBA: [u8; 4] = [0x8B, 0x7A, 0xA8, 255];

// ============================================================================
// WORKSPACE TAGS & FONTS
// ============================================================================

/// Workspace tags (`dwm` 1..9 bitmask tags).
pub const TAG_NAMES: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// Mask covering all 9 tags (`0b1_1111_1111`).
pub const TAGMASK: u16 = (1 << TAG_NAMES.len()) - 1;

/// Font size of the built-in status bar in logical pixels (multiplied by the output scale).
pub const BAR_FONT_SIZE: f32 = 13.5;

/// Candidate system monospace TTF fonts loaded at startup for the built-in status bar.
pub const FONT_PATHS: &[&str] = &[
    "/usr/share/fonts/TTF/JetBrainsMonoNerdFont-Regular.ttf",
    "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/TTF/DejaVuSansMono.ttf",
    "/usr/share/fonts/TTF/FiraCode-Regular.ttf",
    "/usr/share/fonts/TTF/Hack-Regular.ttf",
    "/usr/share/fonts/noto/NotoSansMono-Regular.ttf",
    "/usr/share/fonts/liberation/LiberationMono-Regular.ttf",
    "/usr/share/fonts/gnu-free/FreeMono.ttf",
    "/usr/share/fonts/Adwaita/AdwaitaMono-Regular.ttf",
];

// ============================================================================
// DWM-STYLE WINDOW RULES
// ============================================================================

/// Compile-time window rule (`dwm` `Rule`), matched when a window maps or updates its `app_id`/`title`.
#[derive(Debug, Clone, Copy)]
pub struct WindowRule {
    /// Substring match against the Wayland `app_id` (`None` matches any `app_id`).
    pub app_id_match: Option<&'static str>,
    /// Substring match against the window title (`None` matches any title).
    pub title_match: Option<&'static str>,
    /// Tag bitmask to assign (`0` keeps the current active tag).
    pub tags: u16,
    /// Whether this window should automatically float centered on screen.
    pub is_floating: bool,
    /// Optional per-window opacity override (`Some(1.0)` forces 100% opaque for video players, etc.).
    pub opacity: Option<f32>,
}

/// Built-in daily-driver window rules.
pub const WINDOW_RULES: &[WindowRule] = &[
    // Spotlight launcher & purpwm Wi-Fi / Bluetooth / Polkit control popups always float centered
    WindowRule {
        app_id_match: Some("spotlight"),
        title_match: None,
        tags: 0,
        is_floating: true,
        opacity: Some(0.96),
    },
    WindowRule {
        app_id_match: Some("purpwm-control"),
        title_match: None,
        tags: 0,
        is_floating: true,
        opacity: Some(0.96),
    },
    WindowRule {
        app_id_match: Some("org.purpwm.PolkitAuthenticationAgent"),
        title_match: None,
        tags: 0,
        is_floating: true,
        opacity: Some(0.98),
    },
    WindowRule {
        app_id_match: None,
        title_match: Some("Authentication Required"),
        tags: 0,
        is_floating: true,
        opacity: Some(0.98),
    },
    WindowRule {
        app_id_match: None,
        title_match: Some("purpwm Wi-Fi Manager"),
        tags: 0,
        is_floating: true,
        opacity: Some(0.96),
    },
    WindowRule {
        app_id_match: None,
        title_match: Some("purpwm Bluetooth Manager"),
        tags: 0,
        is_floating: true,
        opacity: Some(0.96),
    },
    WindowRule {
        app_id_match: None,
        title_match: Some("purpwm System Monitor"),
        tags: 0,
        is_floating: true,
        opacity: Some(0.96),
    },
    // Common utility / settings dialogs should float automatically
    WindowRule {
        app_id_match: Some("pavucontrol"),
        title_match: None,
        tags: 0,
        is_floating: true,
        opacity: None,
    },
    WindowRule {
        app_id_match: Some("nm-connection-editor"),
        title_match: None,
        tags: 0,
        is_floating: true,
        opacity: None,
    },
    WindowRule {
        app_id_match: Some("com.system76.CosmicSettings"),
        title_match: None,
        tags: 0,
        is_floating: true,
        opacity: Some(0.98),
    },
    WindowRule {
        app_id_match: Some("blueman-manager"),
        title_match: None,
        tags: 0,
        is_floating: true,
        opacity: None,
    },
    WindowRule {
        app_id_match: Some("xdg-desktop-portal"),
        title_match: None,
        tags: 0,
        is_floating: true,
        opacity: None,
    },
    WindowRule {
        app_id_match: Some("pinentry"),
        title_match: None,
        tags: 0,
        is_floating: true,
        opacity: Some(1.0),
    },
    WindowRule {
        app_id_match: None,
        title_match: Some("Open File"),
        tags: 0,
        is_floating: true,
        opacity: None,
    },
    WindowRule {
        app_id_match: None,
        title_match: Some("Save File"),
        tags: 0,
        is_floating: true,
        opacity: None,
    },
    // Media & video players render at 100% opacity by default
    WindowRule {
        app_id_match: Some("mpv"),
        title_match: None,
        tags: 0,
        is_floating: false,
        opacity: Some(1.0),
    },
    WindowRule {
        app_id_match: Some("imv"),
        title_match: None,
        tags: 0,
        is_floating: true,
        opacity: Some(1.0),
    },
    WindowRule {
        app_id_match: Some("vlc"),
        title_match: None,
        tags: 0,
        is_floating: false,
        opacity: Some(1.0),
    },
];

// ============================================================================
// COMMANDS (Launcher, Terminal, System Monitor, Wi-Fi, Bluetooth, Audio)
// ============================================================================

/// App launcher commands tried in order when `Mod` is tapped alone (or `Mod + p`).
/// Configured to launch the user's `Spotlight` project (`/home/mag/Documents/Spotlight`).
pub const LAUNCHER_COMMANDS: &[&[&str]] = &[
    &[
        "/home/mag/Documents/Spotlight/target/release/spotlight",
        "--toggle",
    ],
    &[
        "/home/mag/Documents/Spotlight/target/debug/spotlight",
        "--toggle",
    ],
    &["/home/mag/.local/bin/spotlight", "--toggle"],
    &["spotlight", "--toggle"],
];

/// Terminal commands tried in order when `Mod + Return` is pressed.
pub const TERMINAL_COMMANDS: &[&[&str]] = &[
    &["alacritty"],
    &["foot"],
    &["kitty"],
    &["wezterm"],
    &["cosmic-term"],
    &["gnome-terminal"],
    &["weston-terminal"],
    &["xterm"],
];

/// System Monitor commands tried in order when clicking the CPU/MEM/BAT area in the status bar or pressing `Mod + Escape`.
pub const SYSTEM_MONITOR_COMMANDS: &[&[&str]] = &[
    &[
        "alacritty",
        "--class",
        "purpwm-control",
        "-t",
        "purpwm System Monitor",
        "-e",
        "btop",
    ],
    &[
        "alacritty",
        "--class",
        "purpwm-control",
        "-t",
        "purpwm System Monitor",
        "-e",
        "top",
    ],
];

/// Wi-Fi manager commands tried in order when clicking the `WIFI:` status bar pill or pressing `Mod + w`.
pub const WIFI_COMMANDS: &[&[&str]] = &[
    &["/usr/local/bin/purpwm-wifi"],
    &["/home/mag/.local/bin/purpwm-wifi"],
    &["/home/mag/Documents/purpwm/purpwm-wifi"],
    &[
        "alacritty",
        "--class",
        "purpwm-control",
        "-t",
        "purpwm Wi-Fi Manager",
        "-e",
        "nmtui",
    ],
    &["nm-connection-editor"],
    &["cosmic-settings", "wireless"],
];

/// Bluetooth manager commands tried in order when clicking the `BT:` status bar pill or pressing `Mod + Shift + b`.
pub const BLUETOOTH_COMMANDS: &[&[&str]] = &[
    &["/usr/local/bin/purpwm-bt"],
    &["/home/mag/.local/bin/purpwm-bt"],
    &["/home/mag/Documents/purpwm/purpwm-bt"],
    &["cosmic-settings", "bluetooth"],
];

/// Audio volume up commands (`XF86AudioRaiseVolume`).
pub const VOL_UP_COMMANDS: &[&[&str]] = &[
    &[
        "wpctl",
        "set-volume",
        "-l",
        "1.5",
        "@DEFAULT_AUDIO_SINK@",
        "5%+",
    ],
    &["pactl", "set-sink-volume", "@DEFAULT_SINK@", "+5%"],
];

/// Audio volume down commands (`XF86AudioLowerVolume`).
pub const VOL_DOWN_COMMANDS: &[&[&str]] = &[
    &["wpctl", "set-volume", "@DEFAULT_AUDIO_SINK@", "5%-"],
    &["pactl", "set-sink-volume", "@DEFAULT_SINK@", "-5%"],
];

/// Audio mute toggle commands (`XF86AudioMute`).
pub const VOL_MUTE_COMMANDS: &[&[&str]] = &[
    &["wpctl", "set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"],
    &["pactl", "set-sink-mute", "@DEFAULT_SINK@", "toggle"],
];

/// Microphone mute toggle commands (`XF86AudioMicMute`).
pub const MIC_MUTE_COMMANDS: &[&[&str]] = &[
    &["wpctl", "set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"],
    &["pactl", "set-source-mute", "@DEFAULT_SOURCE@", "toggle"],
];

/// Screen brightness up commands (`XF86MonBrightnessUp`).
pub const BRIGHTNESS_UP_COMMANDS: &[&[&str]] = &[&["brightnessctl", "set", "+5%"]];

/// Screen brightness down commands (`XF86MonBrightnessDown`).
pub const BRIGHTNESS_DOWN_COMMANDS: &[&[&str]] = &[&["brightnessctl", "set", "5%-"]];

/// Media play/pause commands (`XF86AudioPlay`).
pub const MEDIA_PLAY_COMMANDS: &[&[&str]] = &[&["playerctl", "play-pause"]];

/// Media next track commands (`XF86AudioNext`).
pub const MEDIA_NEXT_COMMANDS: &[&[&str]] = &[&["playerctl", "next"]];

/// Media previous track commands (`XF86AudioPrev`).
pub const MEDIA_PREV_COMMANDS: &[&[&str]] = &[&["playerctl", "previous"]];

/// Built-in screenshots (`Print` = whole screen, `Shift/Mod + Print` = focused window) are saved
/// under `$HOME/<SCREENSHOT_DIR>` and also offered on the clipboard as `image/png`.
pub const SCREENSHOT_DIR: &str = "Pictures/Screenshots";

/// Audio mixer commands tried in order when right-clicking the `VOL` status bar pill.
pub const MIXER_COMMANDS: &[&[&str]] = &[&["pavucontrol"], &["cosmic-settings", "sound"]];

/// Polkit authentication agent commands tried in order.
pub const POLKIT_COMMANDS: &[&[&str]] = &[
    &["/usr/local/bin/purpwm-polkit-agent"],
    &["/home/mag/.local/bin/purpwm-polkit-agent"],
    &["/home/mag/Documents/purpwm/purpwm-polkit-agent"],
];

/// Optional background daemons to autostart when `purpwm` launches (notification daemons, wallpaper, etc.).
pub const AUTOSTART_COMMANDS: &[&[&str]] = &[&["mako"], &["dunst"]];
