# Keybindings

`Mod` is **Super** (Logo) or **Alt**. All bindings are defined in
[`src/input.rs`](../src/input.rs). Press **`Mod + h`** at any time to display the
built-in keybinding cheat sheet overlay (`Mod + h`, `Escape`, or any click / tap closes it).

## Session

| Keys | Action |
| :--- | :--- |
| `Mod + h` | Show / hide the keybinding cheat sheet overlay (`Escape` or click / tap also closes) |
| `Mod + Shift + l` / `Mod + Ctrl + l` | Lock the screen (built-in PAM lock screen; also `XF86ScreenSaver` or `kill -USR2 $(pidof purpwm)`) |
| `Mod + Shift + q` | Quit purpwm (back to the login screen) |
| `Mod + q` | Close the focused window |

While locked, type your password and press `Enter` to unlock (`BackSpace` deletes one character; `Escape` or `Ctrl + u` clears the input).

## Launching

| Keys | Action |
| :--- | :--- |
| Tap `Mod` alone | Toggle the launcher (Spotlight) |
| `Mod + p` | Toggle the launcher |
| `Mod + Return` | Open a terminal |
| `Mod + s` / ``Mod + ` `` | Show / hide the drop-down scratchpad terminal |
| `Mod + Escape` | Open the system monitor (`btop` / `top`) |
| `Mod + w` | Open the Wi-Fi manager |
| `Mod + Shift + w` | Toggle the Wi-Fi radio |
| `Mod + Shift + b` | Open the Bluetooth manager |
| `Mod + Ctrl + b` | Toggle Bluetooth power |

## Focus and windows

| Keys | Action |
| :--- | :--- |
| `Mod + j` / `Mod + k` | Focus next / previous window |
| `Mod + Shift + Return` | Zoom: swap the focused window with the master (on the master: with the next window) |
| `Mod + Arrow` | Move the window: swap in the tiled stack, or nudge 40 px when floating |
| `Mod + Shift + Arrow` | Resize by 40 px (a tiled window stays tiled: its neighbours give way) |
| `Mod + Shift + Space` | Toggle floating |
| `Mod + Shift + f` / `F11` | Toggle fullscreen |
| `Mod + o` | Toggle 100 % opaque mode (for video, photos) |

Focus follows the mouse when the pointer **enters** another window; a newly opened
window keeps focus until you actually move onto something else. Menus and drags never
move focus.

## Layouts

| Keys | Action |
| :--- | :--- |
| `Mod + t` | Tile `[]=` |
| `Mod + u` | Centered master `\|M\|` |
| `Mod + g` | Grid `###` |
| `Mod + m` | Monocle `[M]` |
| `Mod + f` | Floating `><>` |
| `Mod + Space` | Toggle between the current and previous layout |
| `Mod + Ctrl + Space` | Cycle through all layouts |
| `Mod + [` / `Mod + ]` (or `Mod + l`) | Shrink / grow the master area (`mfact` ± 0.05) |
| `Mod + i` / `Mod + d` | More / fewer master windows (`nmaster`) |

Layout, `mfact` and `nmaster` are remembered per tag.

## Tags (workspaces)

| Keys | Action |
| :--- | :--- |
| `Mod + 1..9` | View tag |
| `Mod + Ctrl + 1..9` | Toggle a tag in the view (view several at once) |
| `Mod + Shift + 1..9` | Move the focused window to a tag |
| `Mod + Ctrl + Shift + 1..9` | Toggle a tag on the focused window |
| `Mod + 0` | View all tags |
| `Mod + Shift + 0` | Show the focused window on all tags |
| `Mod + Tab` | Back to the previously viewed tags |

## Appearance

| Keys | Action |
| :--- | :--- |
| `Mod + Ctrl + =` / `Mod + Ctrl + -` | Bigger / smaller UI scale (0.25 steps, saved) |
| `Mod + =` / `Mod + -` | Bigger / smaller gaps |
| `Mod + Shift + g` | Toggle gaps |
| `Mod + Ctrl + 0` | Reset gaps |
| `Mod + b` | Show / hide the status bar |

## Hardware keys

These work without `Mod`.

| Key | Action |
| :--- | :--- |
| Volume up / down / mute | `wpctl` (falls back to `pactl`) |
| Mic mute | `wpctl` / `pactl` |
| Brightness up / down | `brightnessctl`, or logind when it's not installed |
| Play-pause / next / previous | `playerctl` |
| `XF86ScreenSaver` | Lock the screen |
| `Print` | Screenshot of the whole screen |
| `Shift + Print` / `Mod + Print` | Screenshot of the focused window |

Screenshots are saved to `~/Pictures/Screenshots/purpwm-YYYYmmdd-HHMMSS.png` and placed
on the clipboard as `image/png`. Scripts can trigger one with `kill -USR1 $(pidof purpwm)`.

## Mouse & Touch

| Action | Effect |
| :--- | :--- |
| `Mod + Left-drag` / `Mod + Touch-drag` | Move a window (floats it) |
| `Mod + Right-drag` | Resize a window (a tiled window stays tiled: the splits nearest the pointer follow it) |
| `Mod + Middle-click` | Toggle floating |
| Tap window / overlay / popup | Focus the surface and forward native `wl_touch` events |
| Tap status bar pill / tag | Trigger the corresponding status bar action |
| Click or tap notification toast | Dismiss the notification |

## Status bar

| Target | Left-click / Tap | Right-click | Other |
| :--- | :--- | :--- | :--- |
| Tag `1..9` | View tag (`Shift`: move window there) | Toggle tag in view | Middle: move window there |
| Layout symbol | Next layout | Previous layout | |
| Title area | Focus next window | | Middle: open a terminal |
| `WIFI` pill | Wi-Fi manager | Toggle Wi-Fi radio | |
| `BT` pill | Bluetooth manager | Toggle Bluetooth power | |
| `VOL` pill | Mute / unmute | Open mixer (`pavucontrol`) | Scroll: volume |
| `BAT` / `CPU` / `MEM` / clock | System monitor | | |
| Anywhere else | | | Scroll: switch tags |
