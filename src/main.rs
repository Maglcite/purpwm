//! Entry point and backend runners for `purpwm`.
//!
//! Supports both standalone display manager login (`--standalone`, launched from `cosmic-greeter`
//! at native DRM resolution e.g. `2560x1600`) and nested Wayland/X11 window mode (`--winit`).

pub mod bar;
pub mod config;
pub mod cursor;
pub mod help;
pub mod input;
pub mod lock;
pub mod notify;
pub mod render;
pub mod state;
#[cfg(test)]
mod tests;
pub mod wm;

use std::{
    fs,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Instant,
};

use smithay::{
    backend::{
        allocator::Fourcc,
        input::{
            AbsolutePositionEvent, ButtonState, Event, InputEvent, KeyboardKeyEvent,
            PointerButtonEvent, PointerMotionEvent, TouchEvent,
        },
        renderer::{
            gles::{GlesError, GlesMapping, GlesRenderer, GlesTarget},
            ExportMem, Texture,
        },
        winit::{self, WinitEvent, WinitInput},
    },
    input::pointer::CursorImageStatus,
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        wayland_server::{Display, ListeningSocket},
        winit::{
            dpi::{LogicalSize, PhysicalSize},
            platform::pump_events::PumpStatus,
            window::{Fullscreen, Window as WinitWindow},
        },
    },
    utils::{Buffer, IsAlive, Logical, Physical, Rectangle, Size, Transform},
};

use crate::{
    render::render_output_frame,
    state::{initial_scale, send_frames_surface_tree, ClientState, PurpState, ScreenshotRequest},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt().with_env_filter(env_filter).init();
    } else {
        tracing_subscriber::fmt().with_target(false).init();
    }

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return Ok(());
    }

    let spawn_initial_terminal = args.iter().any(|a| a == "--spawn-terminal");
    let no_autostart = args.iter().any(|a| a == "--no-autostart");
    let is_standalone = args
        .iter()
        .any(|a| a == "--standalone" || a == "--fullscreen")
        || std::env::var_os("PURPWM_STANDALONE").is_some();

    run_winit(spawn_initial_terminal, is_standalone, no_autostart)
}

fn print_usage() {
    println!(
        "purpwm v{} — Ultra-lightweight dwm-inspired Wayland compositor (Translucent Purple Theme)\n\
         \n\
         USAGE:\n\
         \x20 purpwm [OPTIONS]\n\
         \n\
         OPTIONS:\n\
         \x20 --standalone       Run fullscreen at native DRM panel resolution (used by COSMIC greeter)\n\
         \x20 --winit            Run nested inside an existing Wayland/X11 session\n\
         \x20 --spawn-terminal   Automatically launch a terminal on startup\n\
         \x20 --no-autostart     Skip Spotlight daemon, polkit agent, radios & autostart (nested testing)\n\
         \x20 -h, --help         Show this help message\n\
         \n\
         KEYBINDINGS (Mod = Super/Logo or Alt):\n\
         \x20 Mod + h            Show / hide keybinding cheat sheet overlay (Esc / click to close)\n\
         \x20 Tap Mod alone      Toggle Spotlight App Launcher\n\
         \x20 Mod + p            Toggle Spotlight App Launcher\n\
         \x20 Mod + Return       Open Terminal (alacritty / foot / kitty / etc.)\n\
         \x20 Mod + w            Open Wi-Fi Manager (or Left-Click WIFI pill in top bar)\n\
         \x20 Mod + Shift + w    Toggle Wi-Fi Radio On/Off (or Right-Click WIFI pill in top bar)\n\
         \x20 Mod + Shift + b    Open Bluetooth Manager (or Left-Click BT pill in top bar)\n\
         \x20 Mod + Ctrl + b     Toggle Bluetooth Power On/Off (or Right-Click BT pill in top bar)\n\
         \x20 Mod + s / Mod + `  Toggle drop-down Scratchpad Terminal\n\
         \x20 Mod + Shift/Ctrl+l Lock screen with built-in PAM screen locker (also kill -USR2)\n\
         \x20 Mod + Arrow Keys   Move focused window (swap in tiled mode, nudge in floating mode)\n\
         \x20 Mod + Shift+Arrows Resize focused floating window\n\
         \x20 Mod + j / k        Focus next / previous window\n\
         \x20 Mod + [ / ] (or l) Decrease / increase master width factor (mfact)\n\
         \x20 Mod + i / d        Increase / decrease master window count (nmaster)\n\
         \x20 Mod + t / m / f    Switch layout: Tile ([]=), Monocle ([M]), Floating (><>)\n\
         \x20 Mod + u / g        Switch layout: CenteredMaster (|M|), Grid (###)\n\
         \x20 Mod + Shift + f    Toggle true Fullscreen on focused window (also F11)\n\
         \x20 Mod + o            Toggle 100% Opaque mode on focused window (for video/media)\n\
         \x20 Mod + = / -        Increase / decrease vanity gaps on the fly\n\
         \x20 Mod + Shift + g    Toggle vanity gaps on/off\n\
         \x20 Mod + Ctrl + = / - Bigger / smaller UI scale (saved; PURPWM_SCALE env overrides)\n\
         \x20 Mod + Ctrl + 0     Reset vanity gaps to default\n\
         \x20 Mod + Shift+Space  Toggle floating on focused window\n\
         \x20 Mod + b            Toggle top status bar visibility\n\
         \x20 Mod + 1..9         Switch to workspace tag 1..9 (per-tag layouts & mfact)\n\
         \x20 Mod + Shift + 1..9 Move focused window to workspace tag 1..9\n\
         \x20 Mod + Escape       Open System Monitor (or click CPU/MEM area in top bar)\n\
         \x20 Print              Screenshot to ~/Pictures/Screenshots + clipboard (kill -USR1 too)\n\
         \x20 Shift/Mod + Print  Screenshot of the focused window only\n\
         \x20 VOL pill           Click: mute | Scroll: volume | Right-click: mixer\n\
         \x20 Mod + q            Close focused window\n\
         \x20 Mod + Shift + q    Quit purpwm",
        env!("CARGO_PKG_VERSION")
    );
}

/// Prefer an internal panel over whichever connected connector sorts first.
/// `connectors` is `(sysfs name, first mode line)`.
pub(crate) fn pick_preferred_mode(connectors: &[(&str, &str)]) -> Option<(u32, u32)> {
    fn internal(name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        name.contains("edp") || name.contains("lvds") || name.contains("dsi")
    }
    fn parse(mode: &str) -> Option<(u32, u32)> {
        let (w, h) = mode.trim().split_once('x')?;
        let (w, h) = (w.parse::<u32>().ok()?, h.parse::<u32>().ok()?);
        (w >= 640 && h >= 480).then_some((w, h))
    }
    let mut external = None;
    for (name, mode) in connectors {
        let Some(size) = parse(mode) else {
            continue;
        };
        if internal(name) {
            return Some(size);
        }
        if external.is_none() {
            external = Some(size);
        }
    }
    external
}

/// Detect the primary connected display's native resolution from `/sys/class/drm/card*-*/modes`.
/// Falls back to `(2560, 1600)` if no DRM sysfs mode entry is readable.
pub fn detect_preferred_resolution() -> (u32, u32) {
    let mut connectors = Vec::new();
    if let Ok(entries) = fs::read_dir("/sys/class/drm") {
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let Ok(status) = fs::read_to_string(path.join("status")) else {
                continue;
            };
            if status.trim() != "connected" {
                continue;
            }
            let Ok(modes) = fs::read_to_string(path.join("modes")) else {
                continue;
            };
            let Some(first_mode) = modes.lines().next() else {
                continue;
            };
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            connectors.push((name, first_mode.trim().to_string()));
        }
    }
    let borrowed: Vec<(&str, &str)> = connectors
        .iter()
        .map(|(name, mode)| (name.as_str(), mode.as_str()))
        .collect();
    pick_preferred_mode(&borrowed).unwrap_or((2560, 1600))
}

/// Logical (layout) size of the output for a physical window size at `scale`.
fn logical_output_size(physical: Size<i32, Physical>, scale: f64) -> Size<i32, Logical> {
    let logical = physical.to_f64().to_logical(scale).to_i32_round::<i32>();
    Size::from((logical.w.max(320), logical.h.max(240)))
}

static SCREENSHOT_SIGNAL: AtomicBool = AtomicBool::new(false);
static LOCK_SIGNAL: AtomicBool = AtomicBool::new(false);

extern "C" fn on_screenshot_signal(_: libc::c_int) {
    SCREENSHOT_SIGNAL.store(true, Ordering::Relaxed);
}

extern "C" fn on_lock_signal(_: libc::c_int) {
    LOCK_SIGNAL.store(true, Ordering::Relaxed);
}

/// Start reading back the finished frame (or the requested window region) into a GPU buffer.
/// Must run before `submit()`; the pixels are fetched afterwards by [`finish_capture`].
fn start_capture(
    renderer: &mut GlesRenderer,
    framebuffer: &GlesTarget<'_>,
    size: Size<i32, Physical>,
    scale: f64,
    request: ScreenshotRequest,
) -> Result<GlesMapping, GlesError> {
    let full = Rectangle::<i32, Physical>::from_size(size);
    let region = match request {
        ScreenshotRequest::Screen => full,
        ScreenshotRequest::Region(geo) => geo
            .to_physical_precise_round(scale)
            .intersection(full)
            .unwrap_or(full),
    };
    // The frame is rendered `Flipped180` into a bottom-up GL framebuffer, so the
    // window's rows sit mirrored vertically in buffer space.
    let buffer_region = Rectangle::<i32, Buffer>::new(
        (region.loc.x, size.h - region.loc.y - region.size.h).into(),
        (region.size.w, region.size.h).into(),
    );
    renderer.copy_framebuffer(framebuffer, buffer_region, Fourcc::Abgr8888)
}

/// Map a capture started by [`start_capture`] into tightly packed, top-down, opaque RGBA rows.
/// Mapping makes the GL context current without the window surface, so this has to run
/// after `submit()` (the next `bind()` restores the surface).
fn finish_capture(
    renderer: &mut GlesRenderer,
    mapping: &GlesMapping,
) -> Result<(u32, u32, Vec<u8>), GlesError> {
    let size = mapping.size();
    let pixels = renderer.map_texture(mapping)?;

    let (w, h) = (size.w as usize, size.h as usize);
    let stride = w * 4;
    let mut rgba = Vec::with_capacity(stride * h);
    for row in (0..h).rev() {
        rgba.extend_from_slice(&pixels[row * stride..(row + 1) * stride]);
    }
    // The purple backdrop is translucent; screenshots should look like the screen does.
    for px in rgba.as_chunks_mut::<4>().0 {
        px[3] = 255;
    }
    Ok((w as u32, h as u32, rgba))
}

pub fn run_winit(
    spawn_initial_terminal: bool,
    is_standalone: bool,
    no_autostart: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut display: Display<PurpState> = Display::new()?;
    let dh = display.handle();

    // Bind an available Wayland socket (`wayland-1` .. `wayland-32`) in the real $XDG_RUNTIME_DIR
    let listener = ListeningSocket::bind_auto("wayland", 1..33)?;
    let socket_name = listener
        .socket_name()
        .and_then(|s| s.to_str())
        .unwrap_or("wayland-5")
        .to_string();

    let (native_w, native_h) = detect_preferred_resolution();
    let window_attrs = if is_standalone {
        WinitWindow::default_attributes()
            .with_inner_size(PhysicalSize::new(native_w, native_h))
            .with_fullscreen(Some(Fullscreen::Borderless(None)))
            .with_decorations(false)
            .with_title("purpwm")
            .with_visible(true)
    } else {
        let nested_w = (native_w * 3 / 4).max(1440);
        let nested_h = (native_h * 3 / 4).max(900);
        WinitWindow::default_attributes()
            .with_inner_size(LogicalSize::new(nested_w as f64, nested_h as f64))
            .with_title("purpwm")
            .with_visible(true)
    };

    let (mut backend, mut winit_evt_loop) =
        winit::init_from_attributes::<GlesRenderer>(window_attrs)?;

    // Hide the host window cursor so our crisp software cursor is the single authoritative cursor
    backend.window().set_cursor_visible(false);

    let initial_phys = backend.window_size();
    let scale = initial_scale();
    let initial_logical = logical_output_size(initial_phys, scale);

    // Register a Wayland wl_output global so clients (including Spotlight's zxdg_output_v1 probe)
    // know our exact resolution & refresh rate.
    let output = Output::new(
        "purpwm-eDP-1".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "purpwm".into(),
            model: "display".into(),
        },
    );
    let _output_global = output.create_global::<PurpState>(&dh);
    let mode = Mode {
        size: initial_phys,
        refresh: 120_000,
    };
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        Some(Scale::Fractional(scale)),
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    let mut state = PurpState::new(dh, socket_name.clone(), initial_logical, !is_standalone);
    state.set_scale(scale);
    state.output = Some(output.clone());
    // Physical size and scale the wl_output / layout were last configured for.
    let mut applied_output = (initial_phys, scale);

    tracing::info!(
        "purpwm listening on WAYLAND_DISPLAY={} ({}x{} @ {}x scale, standalone={})",
        socket_name,
        initial_phys.w,
        initial_phys.h,
        scale,
        is_standalone
    );

    // Ensure Wi-Fi and Bluetooth are enabled & connected, pre-warm Spotlight daemon,
    // start built-in notification daemon, and run autostart
    if !no_autostart {
        state.ensure_network_and_bluetooth();
        state.spawn_spotlight_daemon();
        state.spawn_notification_daemon();
        state.run_autostart();
        state.spawn_polkit_agent();
    } else if std::env::var_os("PURPWM_NOTIFY_DBUS").is_some() {
        state.spawn_notification_daemon();
    }

    // `kill -USR1 <pid>` takes a screenshot; `kill -USR2 <pid>` locks the session
    unsafe {
        libc::signal(
            libc::SIGUSR1,
            on_screenshot_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGUSR2,
            on_lock_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }

    if spawn_initial_terminal {
        state.spawn_terminal();
    }

    let start_time = Instant::now();
    let mut last_trim = Instant::now();
    let mut needs_render = true;

    while !state.should_quit {
        let mut had_winit_events = false;
        let pump_status = winit_evt_loop.dispatch_new_events(|event| {
            had_winit_events = true;
            match event {
                // Size changes are applied before the next frame (see `applied_output`).
                WinitEvent::Resized { .. } => {}
                WinitEvent::CloseRequested => {
                    state.should_quit = true;
                }
                WinitEvent::Input(input_ev) => match input_ev {
                    InputEvent::Keyboard { event } => {
                        state.process_keyboard_event(
                            event.key_code(),
                            event.state(),
                            Event::time_msec(&event),
                        );
                    }
                    InputEvent::PointerMotion { event } => {
                        let delta = PointerMotionEvent::<WinitInput>::delta(&event);
                        let delta_unaccel = PointerMotionEvent::<WinitInput>::delta_unaccel(&event);
                        state.process_pointer_move_relative(
                            delta,
                            delta_unaccel,
                            Event::<WinitInput>::time_msec(&event),
                        );
                    }
                    InputEvent::PointerMotionAbsolute { event } => {
                        let out_geo = Size::from((state.output_size.w, state.output_size.h));
                        let pos = event.position_transformed(out_geo);
                        state.process_pointer_motion(pos, Event::time_msec(&event));
                    }
                    InputEvent::PointerButton { event } => {
                        let btn = event.button_code();
                        let btn_state: ButtonState = event.state();
                        state.process_pointer_button(btn, btn_state, Event::time_msec(&event));
                    }
                    InputEvent::PointerAxis { event } => {
                        state.process_pointer_axis::<WinitInput>(event);
                    }
                    InputEvent::TouchDown { event } => {
                        let pos = state.touch_physical_to_logical(event.x(), event.y());
                        state.process_touch_down(event.slot(), pos, Event::time_msec(&event));
                    }
                    InputEvent::TouchMotion { event } => {
                        let pos = state.touch_physical_to_logical(event.x(), event.y());
                        state.process_touch_motion(event.slot(), pos, Event::time_msec(&event));
                    }
                    InputEvent::TouchUp { event } => {
                        state.process_touch_up(event.slot(), Event::time_msec(&event));
                    }
                    InputEvent::TouchCancel { event } => {
                        state.process_touch_cancel(event.slot());
                    }
                    InputEvent::TouchFrame { .. } => {
                        state.process_touch_frame();
                    }
                    _ => {}
                },
                _ => {}
            }
        });

        if matches!(pump_status, PumpStatus::Exit(_)) || state.should_quit {
            break;
        }

        let mut accepted_client = false;
        while let Some(stream) = listener.accept()? {
            let _ = display
                .handle()
                .insert_client(stream, Arc::new(ClientState::default()));
            accepted_client = true;
        }

        let dispatched = display.dispatch_clients(&mut state)?;

        if LOCK_SIGNAL.swap(false, Ordering::Relaxed) {
            state.lock_session();
            needs_render = true;
        }
        if SCREENSHOT_SIGNAL.swap(false, Ordering::Relaxed) {
            state.request_screenshot(false);
        }
        while let Ok(png_bytes) = state.screenshot_rx.try_recv() {
            state.offer_screenshot_clipboard(png_bytes);
        }
        display.flush_clients()?;

        let status_changed = state.bar.tick_status();
        let lock_changed = state.poll_lock_screen();
        let notif_changed = state.notifications.tick();

        if had_winit_events
            || accepted_client
            || dispatched > 0
            || status_changed
            || lock_changed
            || notif_changed
            || state.bar.is_dirty()
            || state.active_grab.is_some()
            || state.screenshot_request.is_some()
        {
            needs_render = true;
        }

        if needs_render {
            let phys_size = backend.window_size();
            if phys_size.w > 0 && phys_size.h > 0 && (phys_size, state.scale) != applied_output {
                applied_output = (phys_size, state.scale);
                let mode = Mode {
                    size: phys_size,
                    refresh: 120_000,
                };
                output.change_current_state(
                    Some(mode),
                    Some(Transform::Flipped180),
                    Some(Scale::Fractional(state.scale)),
                    Some((0, 0).into()),
                );
                output.set_preferred(mode);
                state.output_size = logical_output_size(phys_size, state.scale);
                state.relayout();
            }
            let full_damage = Rectangle::from_size(phys_size);
            let mut pending_capture = None;

            {
                let (renderer, mut framebuffer) = backend.bind()?;
                render_output_frame(
                    &mut state,
                    renderer,
                    &mut framebuffer,
                    phys_size,
                    Transform::Flipped180,
                )?;
                if let Some(request) = state.screenshot_request.take() {
                    match start_capture(renderer, &framebuffer, phys_size, state.scale, request) {
                        Ok(mapping) => pending_capture = Some(mapping),
                        Err(err) => tracing::warn!("Screenshot capture failed: {}", err),
                    }
                }
            }

            // Only visible surfaces get frame callbacks, so clients on hidden tags (or while
            // the session is locked) stop redrawing until they are shown again.
            if !state.is_locked() {
                let elapsed_ms = start_time.elapsed().as_millis() as u32;
                let tags = state.workspace.selected_tags;
                for win in state.windows.iter().filter(|w| w.is_visible_on(tags)) {
                    send_frames_surface_tree(win.wl_surface(), elapsed_ms);
                }
                for (popup, _) in state.visible_popups() {
                    send_frames_surface_tree(popup.wl_surface(), elapsed_ms);
                }
                if let CursorImageStatus::Surface(ref cursor) = state.cursor_status {
                    send_frames_surface_tree(cursor, elapsed_ms);
                }
                for layer in &state.layer_surfaces {
                    if layer.surface.alive() {
                        send_frames_surface_tree(layer.surface.wl_surface(), elapsed_ms);
                    }
                }
                if let Some(ref icon) = state.dnd_icon {
                    if icon.alive() {
                        send_frames_surface_tree(icon, elapsed_ms);
                    }
                }
            }

            backend.submit(Some(&[full_damage]))?;
            needs_render = false;

            if let Some(mapping) = pending_capture.take() {
                match finish_capture(backend.renderer(), &mapping) {
                    Ok((w, h, rgba)) => state.save_screenshot(w, h, rgba),
                    Err(err) => tracing::warn!("Screenshot capture failed: {}", err),
                }
            }
        } else {
            // Idle throttling: sleep 8ms (~120Hz polling) when nothing changed,
            // dropping CPU usage to ~0% and preventing allocator churn.
            std::thread::sleep(std::time::Duration::from_millis(8));
        }

        // Periodically reclaim free glibc heap pages back to the Linux kernel every 30s
        if last_trim.elapsed().as_secs() >= 30 {
            last_trim = Instant::now();
            #[cfg(target_os = "linux")]
            unsafe {
                libc::malloc_trim(0);
            }
        }
    }

    Ok(())
}
