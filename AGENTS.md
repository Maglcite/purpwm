# purpwm agent notes

- GitHub: `Maglcite/purpwm`, **public**. Work on `main`, push directly to `origin/main`.

## Verify

```bash
cargo test --offline
cargo clippy --offline --all-targets   # 3 pre-existing warnings (ingest_line, span, collapsible if)
cargo build --release --offline
git diff --check
```

`cargo fmt --check` already reports diffs in untouched code; only keep your own hunks
rustfmt-clean instead of reformatting whole files.

## Live session

The user's login runs the **installed** `/usr/local/bin/purpwm` (gamescope → purpwm
`--standalone`), so agents usually run inside it. Rebuilding `target/release` does not
reach the login. Deploy with (shows a polkit password dialog; tell the user first):

```bash
pkexec /home/mag/Documents/purpwm/install.sh --system --non-interactive --no-build \
  --binary /home/mag/Documents/purpwm/target/release/purpwm --dependencies none \
  --terminal /usr/bin/alacritty --spotlight /home/mag/.local/bin/spotlight \
  --font /usr/share/fonts/noto/NotoSansMono-Regular.ttf --wallpaper none --scale 1.5 \
  --refresh-rate 120 --resolution auto --autostart yes --session yes --pam yes --keyring-pam no
```

## Nested testing

A nested purpwm opened as a window in the live session stalls in `eglSwapBuffers` while
that window is not visible. Use a headless gamescope host instead (same stack as the login):

```bash
mkdir -p /tmp/pw && env -u PURPWM_STANDALONE -u WAYLAND_DISPLAY -u DISPLAY HOME=/tmp/pw \
  setsid gamescope --backend headless -W 1600 -H 1000 -w 1600 -h 1000 --expose-wayland -- \
  ./target/release/purpwm --no-autostart > /tmp/pw/log 2>&1 < /dev/null & disown
```

- The nested socket is in the log (`listening on WAYLAND_DISPLAY=wayland-N`); start
  clients with `WAYLAND_DISPLAY=wayland-N setsid <app> & disown`.
- `kill -USR1 <nested purpwm pid>` saves a screenshot to `$HOME/Pictures/Screenshots`.
- There are no input-injection tools. To exercise key-driven code, add a temporary
  signal-triggered hook, build it into a separate `--target-dir`, and remove it before
  committing.
- Use `setsid ... & disown`, or the processes die with the tool shell. Kill test processes
  by PID: `pkill -f <pattern>` also matches (and kills) the tool shell's own command line.

## Behaviour notes

- Smithay's winit backend drops key repeats, so held-key behaviour must be driven from the
  main loop (see `src/glide.rs` and `PurpState::tick_key_resize`).
- `Mod + Shift + Arrow` resizing is directional: the edge / tiling split moves the way the
  arrow points, for master and stack windows alike.
