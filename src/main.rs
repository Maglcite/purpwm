//! Entry point and backend runners for `purpwm`.
//!
//! Supports both standalone display manager login (`--standalone`, launched from `cosmic-greeter`
//! at native DRM resolution e.g. `2560x1600`) and nested Wayland/X11 window mode (`--winit`).

pub mod bar;
pub mod config;
pub mod input;
pub mod render;
pub mod state;
#[cfg(test)]
mod tests;
pub mod wm;

use std::{fs, sync::Arc, time::Instant};

use smithay::{
    backend::{
        input::{
            AbsolutePositionEvent, ButtonState, Event, InputEvent, KeyboardKeyEvent,
            PointerButtonEvent, PointerMotionEvent,
        },
        renderer::gles::GlesRenderer,
        winit::{self, WinitEvent, WinitInput},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::{
        wayland_server::{Display, ListeningSocket},
        winit::{
            dpi::{LogicalSize, PhysicalSize},
            platform::pump_events::PumpStatus,
            window::{Fullscreen, Window as WinitWindow},
        },
    },
    utils::{Rectangle, Size, Transform},
};

use crate::{
    render::render_output_frame,
    state::{send_frames_surface_tree, ClientState, PurpState},
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
    let is_standalone = args.iter().any(|a| a == "--standalone" || a == "--fullscreen")
        || std::env::var_os("PURPWM_STANDALONE").is_some();

    run_winit(spawn_initial_terminal, is_standalone)
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
         \x20 -h, --help         Show this help message\n\
         \n\
         KEYBINDINGS (Mod = Super/Logo or Alt):\n\
         \x20 Tap Mod alone      Toggle Spotlight App Launcher (/home/mag/Documents/Spotlight)\n\
         \x20 Mod + p            Toggle Spotlight App Launcher (/home/mag/Documents/Spotlight)\n\
         \x20 Mod + Return       Open Terminal (alacritty / foot / kitty / etc.)\n\
         \x20 Mod + w            Open Wi-Fi Manager (or Left-Click WIFI pill in top bar)\n\
         \x20 Mod + Shift + w    Toggle Wi-Fi Radio On/Off (or Right-Click WIFI pill in top bar)\n\
         \x20 Mod + Shift + b    Open Bluetooth Manager (or Left-Click BT pill in top bar)\n\
         \x20 Mod + Ctrl + b     Toggle Bluetooth Power On/Off (or Right-Click BT pill in top bar)\n\
         \x20 Mod + s / Mod + `  Toggle drop-down Scratchpad Terminal\n\
         \x20 Mod + Arrow Keys   Move focused window (swap in tiled mode, nudge in floating mode)\n\
         \x20 Mod + Shift+Arrows Resize focused floating window\n\
         \x20 Mod + j / k        Focus next / previous window\n\
         \x20 Mod + h / l        Decrease / increase master width factor (mfact)\n\
         \x20 Mod + i / d        Increase / decrease master window count (nmaster)\n\
         \x20 Mod + t / m / f    Switch layout: Tile ([]=), Monocle ([M]), Floating (><>)\n\
         \x20 Mod + u / g        Switch layout: CenteredMaster (|M|), Grid (###)\n\
         \x20 Mod + Shift + f    Toggle true Fullscreen on focused window (also F11)\n\
         \x20 Mod + o            Toggle 100% Opaque mode on focused window (for video/media)\n\
         \x20 Mod + = / -        Increase / decrease vanity gaps on the fly\n\
         \x20 Mod + Shift + g    Toggle vanity gaps on/off\n\
         \x20 Mod + Ctrl + 0     Reset vanity gaps to default\n\
         \x20 Mod + Shift+Space  Toggle floating on focused window\n\
         \x20 Mod + b            Toggle top status bar visibility\n\
         \x20 Mod + 1..9         Switch to workspace tag 1..9 (per-tag layouts & mfact)\n\
         \x20 Mod + Shift + 1..9 Move focused window to workspace tag 1..9\n\
         \x20 Mod + Shift + c    Close focused window\n\
         \x20 Mod + Shift + q    Quit purpwm",
        env!("CARGO_PKG_VERSION")
    );
}

/// Detect the primary connected display's native resolution from `/sys/class/drm/card*-*/modes`.
/// Falls back to `(2560, 1600)` if no DRM sysfs mode entry is readable.
pub fn detect_preferred_resolution() -> (u32, u32) {
    if let Ok(entries) = fs::read_dir("/sys/class/drm") {
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let status_path = path.join("status");
            let modes_path = path.join("modes");
            if let Ok(status) = fs::read_to_string(&status_path) {
                if status.trim() == "connected" {
                    if let Ok(modes) = fs::read_to_string(&modes_path) {
                        if let Some(first_mode) = modes.lines().next() {
                            if let Some((w_str, h_str)) = first_mode.trim().split_once('x') {
                                if let (Ok(w), Ok(h)) = (w_str.parse::<u32>(), h_str.parse::<u32>())
                                {
                                    if w >= 640 && h >= 480 {
                                        return (w, h);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    (2560, 1600)
}

pub fn run_winit(
    spawn_initial_terminal: bool,
    is_standalone: bool,
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
    let initial_logical = Size::from((
        initial_phys.w.max(native_w as i32 / 2),
        initial_phys.h.max(native_h as i32 / 2),
    ));

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
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);

    let mut state = PurpState::new(dh, socket_name.clone(), initial_logical, !is_standalone);

    tracing::info!(
        "purpwm listening on WAYLAND_DISPLAY={} ({}x{}, standalone={})",
        socket_name,
        initial_logical.w,
        initial_logical.h,
        is_standalone
    );

    // Ensure Wi-Fi and Bluetooth are enabled & connected, pre-warm Spotlight daemon, and run autostart
    state.ensure_network_and_bluetooth();
    state.spawn_spotlight_daemon();
    state.run_autostart();
    state.spawn_polkit_agent();

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
                WinitEvent::Resized { size, .. } => {
                    state.output_size = Size::from((size.w.max(320), size.h.max(240)));
                    output.change_current_state(
                        Some(Mode {
                            size,
                            refresh: 120_000,
                        }),
                        Some(Transform::Flipped180),
                        None,
                        Some((0, 0).into()),
                    );
                    state.relayout();
                }
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
        display.flush_clients()?;

        let status_changed = state.bar.tick_status();

        if had_winit_events
            || accepted_client
            || dispatched > 0
            || status_changed
            || state.bar.is_dirty()
            || state.active_grab.is_some()
        {
            needs_render = true;
        }

        if needs_render {
            let phys_size = backend.window_size();
            if phys_size.w > 0
                && phys_size.h > 0
                && phys_size != state.output_size.to_physical(1)
            {
                state.output_size = Size::from((phys_size.w, phys_size.h));
                state.relayout();
            }
            let full_damage = Rectangle::from_size(phys_size);

            {
                let (renderer, mut framebuffer) = backend.bind()?;
                render_output_frame(
                    &mut state,
                    renderer,
                    &mut framebuffer,
                    phys_size,
                    Transform::Flipped180,
                )?;
            }

            let elapsed_ms = start_time.elapsed().as_millis() as u32;
            for win in &state.windows {
                send_frames_surface_tree(win.wl_surface(), elapsed_ms);
            }
            for popup in &state.popups {
                if popup.alive() {
                    send_frames_surface_tree(popup.wl_surface(), elapsed_ms);
                }
            }
            for layer in &state.layer_surfaces {
                if layer.surface.alive() {
                    send_frames_surface_tree(layer.surface.wl_surface(), elapsed_ms);
                }
            }

            backend.submit(Some(&[full_damage]))?;
            needs_render = false;
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
