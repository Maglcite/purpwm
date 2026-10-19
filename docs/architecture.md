# Architecture

purpwm is a single-threaded Smithay compositor (plus a few helper threads) running on
Smithay's **winit** backend: it is a client of a host display server (another
compositor, or `gamescope` when started from a display manager) and composites its own
clients into that window with OpenGL ES.

```
                ┌──────────────── main.rs: event loop ─────────────────┐
 winit events ─▶│ keyboard / pointer ─▶ input.rs ─▶ PurpState actions  │
 Wayland        │ clients ─▶ Display::dispatch_clients ─▶ state.rs     │
 clients        │ status thread ─▶ bar.rs snapshot                     │
                │ (anything changed?) ─▶ render.rs ─▶ submit frame     │
                └──────────────────────────────────────────────────────┘
```

## Source files

| File | Responsibility |
| :--- | :--- |
| `src/main.rs` | CLI, winit window, `wl_output`, main loop, damage-free redraw policy, screenshot capture, scale application |
| `src/state.rs` | `PurpState`: protocol globals and handlers, focus, stacking order, hit-testing, popups, layer shell, spawning commands, scale |
| `src/wm.rs` | dwm tiling engine: `ManagedWindow`, `Workspace`, per-tag state and the five layouts |
| `src/input.rs` | Keybindings, solo-`Mod` tap, pointer motion / buttons / scroll, status-bar clicks, interactive move / resize |
| `src/render.rs` | Builds and draws render elements for one frame |
| `src/bar.rs` | Software-rasterised status bar (`fontdue`), background status polling, cursor image |
| `src/config.rs` | Compile-time configuration |
| `src/tests.rs` | Unit tests for layouts, per-tag state and bar hit-testing |

## Wayland protocols

`wl_compositor`, `wl_subcompositor`, `wl_shm`, `wl_seat`, `wl_output` + `xdg-output`,
`xdg-shell` (toplevels and popups with grabs), `xdg-decoration` (always server-side),
`xdg-activation`, `wlr-layer-shell`, `wl_data_device` (clipboard and drag-and-drop),
`primary-selection`, `viewporter`, `fractional-scale-v1` and `single-pixel-buffer`.

## Main loop

Each iteration:

1. Pump winit events (input, resize, close).
2. Accept new clients and dispatch their requests.
3. Handle a pending `SIGUSR1` screenshot request and hand finished screenshots to the clipboard.
4. Pull the latest status snapshot from the background thread.
5. Render only if something happened (input, client requests, status change, active drag,
   screenshot). Otherwise sleep 8 ms, so an idle desktop uses almost no CPU.
6. Before rendering, if the window size or scale changed, update the `wl_output` mode and
   scale, recompute the logical output size and relayout.
7. After rendering, send frame callbacks to **visible** surfaces only, so clients on hidden
   tags stop redrawing.
8. Every 30 s, `malloc_trim(0)` returns free heap to the kernel.

## Coordinates and scaling

Layout, hit-testing and everything clients see are in **logical** pixels.
`output_size = physical window size / scale`.

Rendering converts to physical pixels: client surfaces are drawn with the output scale,
borders are converted rectangle by rectangle, and the bar is rasterised directly at
physical resolution so its text is sharp. Every surface is told the preferred fractional
scale (and the next integer buffer scale for older clients) on commit, and again whenever
the scale changes.

## Windows, focus and stacking

`PurpState::windows` is ordered newest-first. `stacking_order()` is the single source of
truth for "what is on top" and is used by both rendering and pointer hit-testing:

1. The focused window, if it is fullscreen
2. Floating and scratchpad windows (focused one first)
3. Tiled windows (focused one first)

Popups (menus, tooltips) are drawn above all windows and the bar; `Top` / `Overlay`
layer-shell surfaces above those. `Background` / `Bottom` layers sit under the windows.

Keyboard focus is: an `Exclusive` layer surface (e.g. the launcher), else a clicked
`OnDemand` layer surface, else the focused window. Pointer focus follows the exact surface
under the cursor, including subsurfaces and popups.

`Workspace::arrange` computes geometries for the current tags and sends `xdg_toplevel`
configures (size, activated, tiled and fullscreen states). Hidden windows are deactivated.

## Status bar

The bar is drawn on the CPU into a `MemoryRenderBuffer` and only re-rasterised when its
inputs change (tags, layout, title, status snapshot, width or scale), not on every
client frame. Glyphs are cached.

A background thread refreshes battery, CPU usage (`/proc/stat` deltas), memory, volume and
the clock every 2 s, and Wi-Fi / Bluetooth (`nmcli`, `bluetoothctl`, killed after 3 s if they
hang) every 6 s. Clicking a pill triggers an immediate refresh.

## Screenshots

A request (key or `SIGUSR1`) makes the next frame render without the cursor. Right after
rendering, the framebuffer region is copied into a GPU buffer; after the frame is
submitted the buffer is mapped, since mapping detaches the GL context from the window
surface. The pixels are then flipped (the GL framebuffer is bottom-up), made opaque,
PNG-encoded on a worker thread, saved, and offered on the clipboard through a
compositor-owned `wl_data_device` selection.
