# Architecture

purpwm is a single-threaded Smithay compositor (plus a few helper threads) running on
Smithay's **winit** backend: it is a client of a host display server (another
compositor, or `gamescope` when started from a display manager) and composites its own
clients into that window with OpenGL ES.

```
                ┌───────────────── main.rs: event loop ─────────────────┐
 winit events ─▶│ keyboard / pointer / touch ─▶ input.rs ─▶ PurpState   │
 Wayland        │ clients ─▶ Display::dispatch_clients ─▶ state.rs      │
 clients        │ status / PAM / D-Bus threads ─▶ bar / lock / notify   │
                │ (anything changed?) ─▶ render.rs ─▶ submit frame      │
                └───────────────────────────────────────────────────────┘
```

## Source files

| File | Responsibility |
| :--- | :--- |
| `src/main.rs` | CLI, winit window, `wl_output`, main loop, damage-free redraw policy, screenshot capture, scale application |
| `src/state.rs` | `PurpState`: protocol globals and handlers, focus, stacking order, hit-testing, popups, layer shell, touch/lock/notification integration, spawning commands, scale |
| `src/wm.rs` | dwm tiling engine: `ManagedWindow`, `Workspace`, per-tag state and the five layouts |
| `src/input.rs` | Keybindings, solo-`Mod` tap, pointer motion / buttons / scroll, native `wl_touch` routing, status-bar clicks/taps, interactive move / resize |
| `src/lock.rs` | Built-in full-screen purple PAM screen locker (`LockScreen`, worker-thread PAM conversation) |
| `src/notify.rs` | Built-in `org.freedesktop.Notifications` D-Bus listener and software-rasterised toast overlay (`NotificationCenter`) |
| `src/render.rs` | Builds and draws render elements for one frame |
| `src/bar.rs` | Software-rasterised status bar (`fontdue`), background status polling, cursor image |
| `src/config.rs` | Compile-time configuration |
| `src/tests.rs` | Unit tests for layouts, per-tag state, bar hit-testing, touch scaling, lock screen / PAM conversation, and notifications |

## Wayland protocols

`wl_compositor`, `wl_subcompositor`, `wl_shm`, `wl_seat` (keyboard, pointer, and touch),
`wl_output` + `xdg-output`, `xdg-shell` (toplevels, interactive move/resize, and popups with grabs),
`xdg-decoration` (always server-side), `xdg-activation`, `wlr-layer-shell`,
`wl_data_device` (clipboard and drag-and-drop with icon surfaces), `primary-selection`,
`zwlr_data_control_manager_v1`, `zwp_idle_inhibit_manager_v1`, `viewporter`,
`fractional-scale-v1` and `single-pixel-buffer`.

## Main loop

Each iteration:

1. Pump winit events (keyboard, pointer, touch, resize, close).
2. Accept new clients and dispatch their requests.
3. Handle pending `SIGUSR1` (screenshot) and `SIGUSR2` (lock screen) signals, and hand finished screenshots to the clipboard.
4. Poll the background status thread, PAM lock-screen worker, and D-Bus notification queue.
5. Render only if something happened (input, client requests, status/lock/notification change, active drag,
   screenshot). Otherwise sleep 8 ms, so an idle desktop uses almost no CPU.
6. Before rendering, if the window size or scale changed, update the `wl_output` mode and
   scale, recompute the logical output size and relayout.
7. After rendering, send frame callbacks to **visible** surfaces only (and none while the session is locked), so clients on hidden
   tags stop redrawing.
8. Every 30 s, `malloc_trim(0)` returns free heap to the kernel.

## Coordinates, scaling, and touch

Layout, hit-testing and everything clients see are in **logical** pixels.
`output_size = physical window size / scale`.

Touch events from winit report raw physical window coordinates (`event.x()`, `event.y()`), which are
divided by `state.scale` and clamped to `output_size` via `touch_physical_to_logical_coords`
(bypassing Smithay's `WinitTouchStartedEvent::y_transformed`, which divides Y by width).

Rendering converts to physical pixels: client surfaces are drawn with the output scale,
borders are converted rectangle by rectangle, and the bar, lock screen, and notification
toasts are rasterised directly at physical resolution so text stays sharp.

## Windows, focus and stacking

`PurpState::windows` is ordered newest-first. `stacking_order()` is the single source of
truth for "what is on top" and is used by both rendering and pointer/touch hit-testing:

1. The focused window, if it is fullscreen
2. Floating and scratchpad windows (focused one first)
3. Tiled windows (focused one first)

Popups (menus, tooltips) are drawn above all windows and the bar; `Top` / `Overlay`
layer-shell surfaces and desktop notification toasts sit above those. `Background` / `Bottom`
layers sit under the windows.

While `state.lock_screen` is `Some`, all keyboard, pointer, and touch focus is cleared from clients,
and `render_output_frame` draws only the full-screen purple PAM lock screen.
