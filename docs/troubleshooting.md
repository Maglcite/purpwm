# Troubleshooting

## Logs

purpwm logs through `tracing`. Set `RUST_LOG` for more detail:

```bash
RUST_LOG=debug ./target/release/purpwm --no-autostart 2> purpwm.log
```

When started from a display manager, output goes wherever the display manager sends
session output (often the systemd journal: `journalctl -b --user` or `journalctl -b`).

## Testing changes safely

If you are running purpwm as your session, test new builds **nested** and with
`--no-autostart`:

```bash
env -u PURPWM_STANDALONE ./target/release/purpwm --no-autostart
```

- `PURPWM_STANDALONE` is set inside a purpwm session; without unsetting it the nested copy
  goes full-screen.
- Without `--no-autostart` the nested copy would restart your launcher daemon on itself,
  start a second polkit agent and toggle your radios.
- Rebuilding `target/release/purpwm` while it is running is safe; the running session
  keeps the old binary until you log in again.
- `kill -USR1 <pid>` makes a specific instance save a screenshot, which is handy for
  checking rendering without touching it.

## Common problems

**The bar shows boxes but no text**
Rebuild and reinstall from the current source, then log in again. Older builds silently drew
no text when none of their hard-coded font paths existed. Current builds try `PURPWM_FONT`,
the configured font paths, fontconfig's monospace font, and finally the bundled DejaVu Sans
Mono. Startup logs either `Loaded status bar font: ...` or `using the bundled DejaVu Sans
Mono`; an old `No TTF font found in FONT_PATHS` message confirms a stale binary. Using
`--no-build` reinstalls the supplied binary unchanged, so do not use it with an old build.

**Everything is too small / too big**
Use `Mod + Ctrl + =` / `Mod + Ctrl + -`; the value is saved in `~/.config/purpwm/scale`.
Delete that file to go back to `DEFAULT_SCALE`.

**An app is blurry**
It supports neither fractional scaling nor `wl_surface` version 6, so it renders at 1x and
gets upscaled. Check whether it has a native Wayland mode (e.g. Electron:
`--ozone-platform=wayland`). XWayland is not supported.

**No password dialog appears for admin actions**
Check that `purpwm-polkit-agent` is running (`pgrep -fa purpwm-polkit-agent`). Run it by
hand inside the session to see errors; it prints
`purpwm Polkit Authentication Agent successfully registered.` on success. Only one agent
can be registered per login session, so another desktop's agent can block it.

**The launcher doesn't open**
Tapping `Mod` runs the first available entry in `LAUNCHER_COMMANDS`. Make sure the
path exists, or set `PURPWM_SPOTLIGHT` to its executable in your `session.conf`.

**Apps cannot save passwords, or the keyring asks for a password every login**
Run `purpwm-keyring status` inside the session, then `purpwm-keyring unlock` to
open the standard keyring dialog. Install `gnome-keyring`, `libsecret` and
`python-gobject` if missing. If no dialog appears, check that
`/usr/lib/gcr-prompter` exists and run `purpwm-keyring start` inside purpwm to refresh
the D-Bus display environment. For automatic unlock on password login, follow
[OS keyring setup](installation.md#os-keyring); starting the daemon alone cannot
unlock an encrypted keyring. Autologin requires a manual unlock.

**A terminal (Alacritty) isn't transparent**
purpwm blends whatever alpha the app draws, so check the app's own setting first
(`[window] opacity = 0.75` for Alacritty). If that is set and the window is still solid,
the config file probably failed to parse: Alacritty then ignores the **whole** file. A
common cause is a tool appending a `[[keyboard.bindings]]` table to a config that already
has `bindings = [ ... ]` under `[keyboard]`. Start `alacritty` from another terminal: it
prints `Unable to load config ... duplicate key` with the offending line. Merge the extra
binding into the existing list.

**Wi-Fi / Bluetooth pill says `N/A`**
`nmcli` or `bluetoothctl` isn't installed, or NetworkManager / `bluetoothd` isn't running
(`systemctl status NetworkManager bluetooth`).

**Brightness keys do nothing**
Install `brightnessctl`, or check that logind accepts
`busctl call org.freedesktop.login1 /org/freedesktop/login1/session/auto org.freedesktop.login1.Session SetBrightness ssu backlight <device> <value>`
(device names are in `/sys/class/backlight`).

**Screenshots aren't on the clipboard after closing purpwm**
The clipboard image is served by the compositor itself, so it lasts only while purpwm
runs. The PNG file in `~/Pictures/Screenshots` stays.

**Flameshot says it cannot capture the screen**
Install `xdg-desktop-portal` and `xdg-desktop-portal-gtk` alongside Flameshot, then
log back into the rebuilt purpwm session. Check `pgrep -fa purpwm-portal` and
`journalctl --user -u xdg-desktop-portal.service`. The frontend must select
`purpwm` for `org.freedesktop.impl.portal.Screenshot`. Run `purpwm-portal --install`
to repair the per-user configuration; `purpwm-portal --session` starts the adapter
and refreshes the frontend for the current display. An old compositor binary
must be restarted before it can serve native portal capture requests.

**The session won't start from the login screen**
Standalone mode needs `gamescope`. Run `start-purpwm` from a TTY to see its errors, and
check the generated paths and settings in `session.conf`. Preview the installation
with `./install.sh --system --dry-run`. If using a user install and the session isn't
listed, install with `--system`; many display managers only read system session directories.
