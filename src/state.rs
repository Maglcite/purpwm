//! Core compositor state (`PurpState`), Wayland protocol trait implementations,
//! window focus/stack management, `wlr-layer-shell` overlay support (for `Spotlight`),
//! `xdg-decoration` (server-side decorations), `primary-selection`, `viewporter`,
//! `single-pixel-buffer`, `fractional-scale`, `xdg-activation`, scratchpad, and command spawning.

use std::{
    os::unix::{io::OwnedFd, process::CommandExt},
    path::PathBuf,
    process::{Command, Stdio},
};

use smithay::{
    backend::renderer::utils::on_commit_buffer_handler,
    delegate_compositor, delegate_data_device, delegate_fractional_scale, delegate_layer_shell,
    delegate_output, delegate_primary_selection, delegate_seat, delegate_shm,
    delegate_single_pixel_buffer, delegate_viewporter, delegate_xdg_activation,
    delegate_xdg_decoration, delegate_xdg_shell,
    input::{
        keyboard::{KeyboardHandle, ModifiersState},
        pointer::{CursorImageStatus, PointerHandle},
        Seat, SeatHandler, SeatState,
    },
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            shell::server::xdg_toplevel,
        },
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{
                wl_buffer,
                wl_output::WlOutput,
                wl_seat,
                wl_surface::{self, WlSurface},
            },
            Client, DisplayHandle, Resource,
        },
    },
    utils::{Logical, Point, Rectangle, Serial, Size},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            with_states, with_surface_tree_downward, CompositorClientState, CompositorHandler,
            CompositorState, SurfaceAttributes, TraversalAction,
        },
        fractional_scale::{
            with_fractional_scale, FractionalScaleHandler, FractionalScaleManagerState,
        },
        output::{OutputHandler, OutputManagerState},
        selection::{
            data_device::{
                set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
                ServerDndGrabHandler,
            },
            primary_selection::{
                set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
            },
            SelectionHandler,
        },
        shell::{
            wlr_layer::{
                Anchor, KeyboardInteractivity, Layer, LayerSurface, LayerSurfaceCachedState,
                WlrLayerShellHandler, WlrLayerShellState,
            },
            xdg::{
                decoration::{XdgDecorationHandler, XdgDecorationState},
                PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            },
        },
        shm::{ShmHandler, ShmState},
        single_pixel_buffer::SinglePixelBufferState,
        viewporter::ViewporterState,
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
    },
};

use crate::{
    bar::StatusBar,
    config::{
        AUTOSTART_COMMANDS, BLUETOOTH_COMMANDS, BRIGHTNESS_DOWN_COMMANDS, BRIGHTNESS_UP_COMMANDS,
        LAUNCHER_COMMANDS, MEDIA_NEXT_COMMANDS, MEDIA_PLAY_COMMANDS, MEDIA_PREV_COMMANDS,
        MIC_MUTE_COMMANDS, MOVE_STEP, POLKIT_COMMANDS, RESIZE_STEP, SCREENSHOT_COMMANDS, SYSTEM_MONITOR_COMMANDS, TAGMASK, TERMINAL_COMMANDS,
        VOL_DOWN_COMMANDS, VOL_MUTE_COMMANDS, VOL_UP_COMMANDS, WIFI_COMMANDS, WINDOW_RULES,
    },
    wm::{LayoutMode, ManagedWindow, Workspace},
};

/// Active mouse drag operation (`Mod + Left-Drag` to move, `Mod + Right-Drag` to resize).
#[derive(Debug, Clone, Copy)]
pub enum InteractiveGrab {
    Move {
        window_idx: usize,
        start_pointer: Point<f64, Logical>,
        start_geo: Rectangle<i32, Logical>,
    },
    Resize {
        window_idx: usize,
        start_pointer: Point<f64, Logical>,
        start_geo: Rectangle<i32, Logical>,
    },
}

/// Managed `wlr-layer-shell` surface (used by `Spotlight`'s centered overlay launcher).
#[derive(Debug, Clone)]
pub struct ManagedLayerSurface {
    pub surface: LayerSurface,
    pub layer: Layer,
    pub namespace: String,
    pub geo: Rectangle<i32, Logical>,
}

impl ManagedLayerSurface {
    pub fn compute_geometry(&mut self, output_size: Size<i32, Logical>) {
        let state = with_states(self.surface.wl_surface(), |states| {
            *states.cached_state.get::<LayerSurfaceCachedState>().current()
        });

        let w = if state.size.w > 0 {
            state.size.w.min(output_size.w)
        } else {
            740.min(output_size.w)
        };
        let h = if state.size.h > 0 {
            state.size.h.min(output_size.h)
        } else {
            620.min(output_size.h)
        };

        let x = if state.anchor.contains(Anchor::LEFT) && !state.anchor.contains(Anchor::RIGHT) {
            state.margin.left
        } else if state.anchor.contains(Anchor::RIGHT) && !state.anchor.contains(Anchor::LEFT) {
            output_size.w - w - state.margin.right
        } else {
            (output_size.w - w) / 2
        };

        let y = if state.anchor.contains(Anchor::TOP) && !state.anchor.contains(Anchor::BOTTOM) {
            state.margin.top
        } else if state.anchor.contains(Anchor::BOTTOM) && !state.anchor.contains(Anchor::TOP) {
            output_size.h - h - state.margin.bottom
        } else {
            (output_size.h - h) / 2
        };

        self.geo = Rectangle::new(Point::from((x, y)), Size::from((w, h)));
    }

    pub fn wants_keyboard_focus(&self) -> bool {
        with_states(self.surface.wl_surface(), |states| {
            let mut binding = states.cached_state.get::<LayerSurfaceCachedState>();
            let cached = binding.current();
            matches!(
                cached.keyboard_interactivity,
                KeyboardInteractivity::Exclusive | KeyboardInteractivity::OnDemand
            )
        })
    }
}

/// Per-client compositor state required by Smithay.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

/// Top-level compositor state for `purpwm`.
pub struct PurpState {
    pub display_handle: DisplayHandle,
    pub socket_name: String,
    /// Real user runtime directory (`/run/user/<uid>`) for D-Bus, PipeWire, Keyring, and Wayland sockets.
    pub runtime_dir: PathBuf,
    pub should_quit: bool,
    pub is_nested: bool,
    /// Set to `true` when the next spawned terminal should become the drop-down scratchpad (`Mod + s`).
    pub pending_scratchpad_spawn: bool,

    // Wayland protocol globals
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub xdg_activation_state: XdgActivationState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: PrimarySelectionState,
    pub viewporter_state: ViewporterState,
    pub single_pixel_buffer_state: SinglePixelBufferState,
    pub fractional_scale_state: FractionalScaleManagerState,
    pub output_manager_state: OutputManagerState,

    // Input seat & handles
    pub seat: Seat<Self>,
    pub keyboard: KeyboardHandle<Self>,
    pub pointer: PointerHandle<Self>,
    pub pointer_location: Point<f64, Logical>,
    pub cursor_status: CursorImageStatus,
    pub modifiers: ModifiersState,

    /// Solo-Mod tap detection: set `true` when `Mod` is pressed alone, cleared if any
    /// other key or mouse button is pressed while `Mod` is held. On `Mod` release,
    /// if still `true`, launches the `Spotlight` app launcher.
    pub mod_tap_candidate: bool,

    // Window manager & workspace state
    pub output_size: Size<i32, Logical>,
    pub workspace: Workspace,
    pub windows: Vec<ManagedWindow>,
    pub popups: Vec<PopupSurface>,
    pub layer_surfaces: Vec<ManagedLayerSurface>,
    pub focused_idx: Option<usize>,
    pub active_grab: Option<InteractiveGrab>,

    // Built-in status bar
    pub bar: StatusBar,
}

impl PurpState {
    pub fn new(
        display_handle: DisplayHandle,
        socket_name: String,
        output_size: Size<i32, Logical>,
        is_nested: bool,
    ) -> Self {
        let compositor_state = CompositorState::new::<Self>(&display_handle);
        let xdg_shell_state = XdgShellState::new::<Self>(&display_handle);
        let xdg_decoration_state = XdgDecorationState::new::<Self>(&display_handle);
        let xdg_activation_state = XdgActivationState::new::<Self>(&display_handle);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&display_handle);
        let shm_state = ShmState::new::<Self>(&display_handle, vec![]);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(&display_handle);
        let primary_selection_state = PrimarySelectionState::new::<Self>(&display_handle);
        let viewporter_state = ViewporterState::new::<Self>(&display_handle);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(&display_handle);
        let fractional_scale_state = FractionalScaleManagerState::new::<Self>(&display_handle);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);

        let mut seat = seat_state.new_wl_seat(&display_handle, "seat-0");
        let keyboard = seat
            .add_keyboard(Default::default(), 200, 25)
            .expect("Failed to initialize Wayland seat keyboard");
        let pointer = seat.add_pointer();

        let bar = StatusBar::new(output_size.w);

        let uid = unsafe { libc::getuid() };
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", uid)));

        Self {
            display_handle,
            socket_name,
            runtime_dir,
            should_quit: false,
            is_nested,
            pending_scratchpad_spawn: false,
            compositor_state,
            xdg_shell_state,
            xdg_decoration_state,
            xdg_activation_state,
            layer_shell_state,
            shm_state,
            seat_state,
            data_device_state,
            primary_selection_state,
            viewporter_state,
            single_pixel_buffer_state,
            fractional_scale_state,
            output_manager_state,
            seat,
            keyboard,
            pointer,
            pointer_location: Point::from((output_size.w as f64 / 2.0, output_size.h as f64 / 2.0)),
            cursor_status: CursorImageStatus::default_named(),
            modifiers: ModifiersState::default(),
            mod_tap_candidate: false,
            output_size,
            workspace: Workspace::default(),
            windows: Vec::new(),
            popups: Vec::new(),
            layer_surfaces: Vec::new(),
            focused_idx: None,
            active_grab: None,
            bar,
        }
    }

    /// Re-run the `dwm` tiling engine, apply `WINDOW_RULES`, update layer surface geometries,
    /// synchronize keyboard + clipboard focus, and mark the status bar dirty.
    pub fn relayout(&mut self) {
        let prev_len = self.windows.len();
        self.windows.retain(|w| w.toplevel.alive());
        self.popups.retain(|p| p.alive());
        self.layer_surfaces.retain(|l| l.surface.alive());

        for layer in &mut self.layer_surfaces {
            layer.compute_geometry(self.output_size);
        }

        if self.windows.len() != prev_len {
            self.ensure_valid_focus();
            #[cfg(target_os = "linux")]
            unsafe {
                libc::malloc_trim(0);
            }
        }

        // Evaluate compile-time `WINDOW_RULES` once a client sets its `app_id` or `title`
        let out_size = self.output_size;
        for w in &mut self.windows {
            if !w.rules_applied {
                let (app_id, title) = w.app_id_and_title();
                if !app_id.is_empty() || !title.is_empty() {
                    w.rules_applied = true;
                    for rule in WINDOW_RULES {
                        let app_matches = rule
                            .app_id_match
                            .is_some_and(|m| !app_id.is_empty() && app_id.contains(m));
                        let title_matches = rule
                            .title_match
                            .is_some_and(|m| !title.is_empty() && title.contains(m));
                        if app_matches || title_matches {
                            if rule.tags != 0 {
                                w.tags = rule.tags & TAGMASK;
                            }
                            if rule.is_floating {
                                w.is_floating = true;
                                let fw = 920.min(out_size.w - 48);
                                let fh = 680.min(out_size.h - 64);
                                w.float_geo = Rectangle::new(
                                    Point::from(((out_size.w - fw) / 2, (out_size.h - fh) / 2)),
                                    Size::from((fw, fh)),
                                );
                                w.geo = w.float_geo;
                            }
                            if let Some(op) = rule.opacity {
                                w.opacity_override = Some(op);
                            }
                            break;
                        }
                    }
                }
            }
        }

        if let Some(idx) = self.focused_idx {
            if idx >= self.windows.len()
                || !self.windows[idx].is_visible_on(self.workspace.selected_tags)
            {
                self.ensure_valid_focus();
            } else {
                // Clear urgent bit when focused
                self.windows[idx].is_urgent = false;
            }
        } else {
            self.ensure_valid_focus();
        }

        self.workspace
            .arrange(&mut self.windows, self.focused_idx, self.output_size);
        self.sync_keyboard_focus();
        self.bar.mark_dirty();
    }

    fn ensure_valid_focus(&mut self) {
        self.focused_idx = self
            .windows
            .iter()
            .enumerate()
            .find(|(_, w)| w.is_visible_on(self.workspace.selected_tags))
            .map(|(i, _)| i);
    }

    pub fn sync_keyboard_focus(&mut self) {
        if let Some(layer) = self
            .layer_surfaces
            .iter()
            .rev()
            .find(|l| l.surface.alive() && l.wants_keyboard_focus())
        {
            let surf = layer.surface.wl_surface().clone();
            let keyboard = self.keyboard.clone();
            keyboard.set_focus(self, Some(surf), Serial::from(0));
            return;
        }

        let target_surface = self
            .focused_idx
            .and_then(|idx| self.windows.get(idx))
            .filter(|w| w.is_visible_on(self.workspace.selected_tags))
            .map(|w| w.wl_surface().clone());

        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, target_surface, Serial::from(0));
    }

    /// Focus next (`delta = 1`) or previous (`delta = -1`) visible window on current tags.
    pub fn cycle_focus(&mut self, delta: isize) {
        let visible: Vec<usize> = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, w)| w.is_visible_on(self.workspace.selected_tags))
            .map(|(i, _)| i)
            .collect();

        if visible.is_empty() {
            self.focused_idx = None;
            return;
        }

        let current_pos = self
            .focused_idx
            .and_then(|cur| visible.iter().position(|&i| i == cur))
            .unwrap_or(0);

        let len = visible.len() as isize;
        let next_pos = (current_pos as isize + delta).rem_euclid(len) as usize;
        self.focused_idx = Some(visible[next_pos]);
        self.relayout();
    }

    /// Move the focused window with `Mod + Arrow` keys.
    pub fn move_focused_window(&mut self, dx: i32, dy: i32) {
        let Some(focused) = self.focused_idx else {
            return;
        };
        if focused >= self.windows.len() {
            return;
        }

        let is_float = self.windows[focused].is_floating
            || self.windows[focused].is_scratchpad
            || self.workspace.layout() == LayoutMode::Floating;

        if is_float {
            let w = &mut self.windows[focused];
            w.float_geo.loc.x += dx * MOVE_STEP;
            w.float_geo.loc.y += dy * MOVE_STEP;
            w.geo = w.float_geo;
            self.relayout();
            return;
        }

        let visible_tiled: Vec<usize> = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, w)| {
                w.is_visible_on(self.workspace.selected_tags)
                    && !w.is_floating
                    && !w.is_fullscreen
                    && !w.is_scratchpad
            })
            .map(|(i, _)| i)
            .collect();

        if visible_tiled.len() < 2 {
            return;
        }

        let Some(pos) = visible_tiled.iter().position(|&i| i == focused) else {
            return;
        };

        let delta: isize = if dx < 0 || dy < 0 { -1 } else { 1 };
        let len = visible_tiled.len() as isize;
        let target_pos = (pos as isize + delta).rem_euclid(len) as usize;
        let target_idx = visible_tiled[target_pos];

        self.windows.swap(focused, target_idx);
        self.focused_idx = Some(target_idx);
        self.relayout();
    }

    /// Resize the focused window with `Mod + Shift + Arrow` keys (automatically floats if tiled).
    pub fn resize_focused_window(&mut self, dw: i32, dh: i32) {
        let Some(focused) = self.focused_idx else {
            return;
        };
        if focused >= self.windows.len() {
            return;
        }

        let w = &mut self.windows[focused];
        if !w.is_floating && !w.is_scratchpad {
            w.is_floating = true;
            w.float_geo = w.geo;
        }
        w.float_geo.size.w = (w.float_geo.size.w + dw * RESIZE_STEP).max(120);
        w.float_geo.size.h = (w.float_geo.size.h + dh * RESIZE_STEP).max(80);
        w.geo = w.float_geo;
        self.relayout();
    }

    /// Promote the focused window to the master slot (`dwm` `zoom()`).
    pub fn zoom_focused(&mut self) {
        let Some(focused) = self.focused_idx else {
            return;
        };
        let first_tiled = self
            .windows
            .iter()
            .enumerate()
            .find(|(_, w)| {
                w.is_visible_on(self.workspace.selected_tags)
                    && !w.is_floating
                    && !w.is_fullscreen
                    && !w.is_scratchpad
            })
            .map(|(i, _)| i);

        if let Some(master_idx) = first_tiled {
            if master_idx != focused {
                self.windows.swap(master_idx, focused);
                self.focused_idx = Some(master_idx);
                self.relayout();
            }
        }
    }

    /// Switch view to tag bitmask `new_mask` (`Mod + 1..9`).
    pub fn view_tags(&mut self, new_mask: u16) {
        let masked = new_mask & TAGMASK;
        if masked == 0 || masked == self.workspace.selected_tags {
            return;
        }
        self.workspace.prev_tags = self.workspace.selected_tags;
        self.workspace.selected_tags = masked;
        self.workspace.sync_cur_tag();
        self.ensure_valid_focus();
        self.relayout();
    }

    /// Toggle visibility of tag bitmask (`Mod + Ctrl + 1..9`).
    pub fn toggle_view_tags(&mut self, mask: u16) {
        let next = (self.workspace.selected_tags ^ mask) & TAGMASK;
        if next != 0 {
            self.workspace.prev_tags = self.workspace.selected_tags;
            self.workspace.selected_tags = next;
            self.workspace.sync_cur_tag();
            self.ensure_valid_focus();
            self.relayout();
        }
    }

    /// Assign focused window to tag bitmask (`Mod + Shift + 1..9`).
    pub fn tag_focused_window(&mut self, mask: u16) {
        let masked = mask & TAGMASK;
        if masked == 0 {
            return;
        }
        if let Some(idx) = self.focused_idx {
            if let Some(w) = self.windows.get_mut(idx) {
                w.tags = masked;
                self.ensure_valid_focus();
                self.relayout();
            }
        }
    }

    /// Toggle a tag bit on the focused window (`Mod + Ctrl + Shift + 1..9`).
    pub fn toggle_tag_focused_window(&mut self, mask: u16) {
        if let Some(idx) = self.focused_idx {
            if let Some(w) = self.windows.get_mut(idx) {
                let next = (w.tags ^ mask) & TAGMASK;
                if next != 0 {
                    w.tags = next;
                    self.ensure_valid_focus();
                    self.relayout();
                }
            }
        }
    }

    /// Set layout mode (`[]=`, `[M]`, `|M|`, `###`, `><>`).
    pub fn set_layout(&mut self, mode: LayoutMode) {
        self.workspace.set_layout(mode);
        self.relayout();
    }

    /// Toggle floating state on the currently focused window (`Mod + Shift + Space`).
    pub fn toggle_floating_focused(&mut self) {
        if let Some(idx) = self.focused_idx {
            if let Some(w) = self.windows.get_mut(idx) {
                w.is_floating = !w.is_floating;
                if w.is_floating {
                    w.float_geo = w.geo;
                }
                self.relayout();
            }
        }
    }

    /// Toggle fullscreen state on the currently focused window (`Mod + Shift + f` or `Mod + F11`).
    pub fn toggle_fullscreen_focused(&mut self) {
        if let Some(idx) = self.focused_idx {
            if let Some(w) = self.windows.get_mut(idx) {
                w.is_fullscreen = !w.is_fullscreen;
                self.relayout();
            }
        }
    }

    /// Toggle 100% opaque mode on the focused window (`Mod + o`) — great for videos/photos.
    pub fn toggle_opacity_focused(&mut self) {
        if let Some(idx) = self.focused_idx {
            if let Some(w) = self.windows.get_mut(idx) {
                if w.opacity_override.is_some() {
                    w.opacity_override = None;
                } else {
                    w.opacity_override = Some(1.0);
                }
                self.bar.mark_dirty();
            }
        }
    }

    /// Toggle the drop-down floating scratchpad terminal (`Mod + s` or `Mod + grave`).
    /// If no scratchpad window exists yet, spawns a terminal and marks it as the scratchpad.
    pub fn toggle_scratchpad(&mut self) {
        if let Some(idx) = self.windows.iter().position(|w| w.is_scratchpad) {
            let currently_hidden = self.windows[idx].is_hidden;
            self.windows[idx].is_hidden = !currently_hidden;
            if currently_hidden {
                let sw = (self.output_size.w * 7 / 10).max(480);
                let sh = (self.output_size.h * 7 / 10).max(320);
                let sx = (self.output_size.w - sw) / 2;
                let sy = (self.output_size.h - sh) / 2;
                self.windows[idx].float_geo =
                    Rectangle::new(Point::from((sx, sy)), Size::from((sw, sh)));
                self.windows[idx].geo = self.windows[idx].float_geo;
                self.focused_idx = Some(idx);
            } else if self.focused_idx == Some(idx) {
                self.ensure_valid_focus();
            }
            self.relayout();
        } else {
            self.pending_scratchpad_spawn = true;
            self.spawn_terminal();
        }
    }

    /// Send `xdg_toplevel.close()` to the currently focused window (`Mod + Shift + c`).
    pub fn close_focused(&mut self) {
        if let Some(idx) = self.focused_idx {
            if let Some(w) = self.windows.get(idx) {
                w.toplevel.send_close();
            }
        }
    }

    /// Spawn `/home/mag/Documents/Spotlight` (`--toggle`) as the app launcher.
    pub fn spawn_launcher(&self) {
        self.spawn_first_available(LAUNCHER_COMMANDS, "Spotlight launcher");
    }

    /// Pre-warm the `Spotlight --daemon` instance attached to `purpwm`'s Wayland display.
    /// Kills any previous `spotlight` daemon first so `$XDG_RUNTIME_DIR/spotlight.sock`
    /// binds cleanly to `purpwm`'s `WAYLAND_DISPLAY` while preserving the real `XDG_RUNTIME_DIR`
    /// (`/run/user/<uid>`) for D-Bus, PipeWire, and NetworkManager!
    pub fn spawn_spotlight_daemon(&self) {
        let _ = Command::new("pkill").args(["-x", "spotlight"]).status();
        let sock_path = self.runtime_dir.join("spotlight.sock");
        let _ = std::fs::remove_file(&sock_path);

        let daemon_candidates: &[&[&str]] = &[
            &[
                "/home/mag/Documents/Spotlight/target/release/spotlight",
                "--daemon",
            ],
            &[
                "/home/mag/Documents/Spotlight/target/debug/spotlight",
                "--daemon",
            ],
            &["/home/mag/.local/bin/spotlight", "--daemon"],
        ];
        self.spawn_first_available(daemon_candidates, "Spotlight daemon");
    }

    /// Ensure Wi-Fi (`NetworkManager`) and Bluetooth (`bluetoothd`) are powered and connected on session start.
    pub fn ensure_network_and_bluetooth(&mut self) {
        std::thread::spawn(|| {
            // 1. Ensure NetworkManager networking and Wi-Fi radio are enabled
            let _ = Command::new("nmcli").args(["networking", "on"]).status();
            let _ = Command::new("nmcli").args(["radio", "wifi", "on"]).status();

            // Check if wifi is already connected; if not, trigger autoconnect on wlan0
            if let Ok(out) = Command::new("nmcli")
                .args(["-t", "-f", "TYPE,STATE", "device", "status"])
                .output()
            {
                let text = String::from_utf8_lossy(&out.stdout);
                let wifi_disconnected = text
                    .lines()
                    .any(|l| l.starts_with("wifi:") && !l.contains(":connected"));
                if wifi_disconnected {
                    let _ = Command::new("nmcli")
                        .args(["device", "connect", "wlan0"])
                        .status();
                }
            }

            // 2. Ensure Bluetooth is unblocked and powered on
            let _ = Command::new("rfkill").args(["unblock", "bluetooth"]).status();
            let _ = Command::new("bluetoothctl").args(["power", "on"]).status();
        });
        self.bar.request_immediate_status_refresh();
    }

    /// Open the floating Wi-Fi Manager (`Mod + w` or Left-Click on `WIFI:` status bar pill).
    pub fn spawn_wifi_manager(&self) {
        self.spawn_first_available(WIFI_COMMANDS, "Wi-Fi manager");
    }

    /// Toggle Wi-Fi radio On/Off (`Mod + Shift + w` or Right-Click on `WIFI:` status bar pill).
    pub fn toggle_wifi_radio(&mut self) {
        std::thread::spawn(|| {
            if let Ok(out) = Command::new("nmcli").args(["radio", "wifi"]).output() {
                let s = String::from_utf8_lossy(&out.stdout);
                if s.trim().eq_ignore_ascii_case("enabled") {
                    let _ = Command::new("nmcli").args(["radio", "wifi", "off"]).status();
                } else {
                    let _ = Command::new("nmcli").args(["radio", "wifi", "on"]).status();
                    std::thread::sleep(std::time::Duration::from_millis(800));
                    let _ = Command::new("nmcli")
                        .args(["device", "connect", "wlan0"])
                        .status();
                }
            }
        });
        self.bar.request_immediate_status_refresh();
    }

    /// Open the floating Bluetooth Manager (`Mod + Shift + b` or Left-Click on `BT:` status bar pill).
    pub fn spawn_bluetooth_manager(&self) {
        self.spawn_first_available(BLUETOOTH_COMMANDS, "Bluetooth manager");
    }

    /// Toggle Bluetooth power On/Off (`Mod + Ctrl + b` or Right-Click on `BT:` status bar pill).
    pub fn toggle_bluetooth_power(&mut self) {
        std::thread::spawn(|| {
            if let Ok(out) = Command::new("bluetoothctl").arg("show").output() {
                let s = String::from_utf8_lossy(&out.stdout);
                let is_on = s
                    .lines()
                    .any(|l| l.trim().eq_ignore_ascii_case("Powered: yes"));
                if is_on {
                    let _ = Command::new("bluetoothctl")
                        .args(["power", "off"])
                        .status();
                } else {
                    let _ = Command::new("rfkill").args(["unblock", "bluetooth"]).status();
                    let _ = Command::new("bluetoothctl")
                        .args(["power", "on"])
                        .status();
                }
            }
        });
        self.bar.request_immediate_status_refresh();
    }

    /// Spawn the first available terminal emulator from [`TERMINAL_COMMANDS`].
    /// Spawn the polkit authentication agent (`purpwm-polkit-agent`).
    pub fn spawn_polkit_agent(&self) {
        self.spawn_first_available(POLKIT_COMMANDS, "polkit agent");
    }

    /// Open the System Monitor / Task Manager (`Mod + Escape` or Left-Click on `CPU:` / `MEM:` / `BAT:` status bar pill).
    pub fn spawn_system_monitor(&self) {
        self.spawn_first_available(SYSTEM_MONITOR_COMMANDS, "system monitor");
    }

    pub fn spawn_terminal(&self) {
        self.spawn_first_available(TERMINAL_COMMANDS, "terminal");
    }

    /// Run user autostart commands from [`AUTOSTART_COMMANDS`].
    pub fn run_autostart(&self) {
        for cmd in AUTOSTART_COMMANDS {
            if !cmd.is_empty() && command_exists(cmd[0]) {
                self.spawn_detached_cmd(cmd, "autostart");
            }
        }
    }

    pub fn spawn_volume_up(&self) {
        self.spawn_first_available(VOL_UP_COMMANDS, "volume up");
    }

    pub fn spawn_volume_down(&self) {
        self.spawn_first_available(VOL_DOWN_COMMANDS, "volume down");
    }

    pub fn spawn_volume_mute(&self) {
        self.spawn_first_available(VOL_MUTE_COMMANDS, "volume mute");
    }

    pub fn spawn_mic_mute(&self) {
        self.spawn_first_available(MIC_MUTE_COMMANDS, "mic mute");
    }

    pub fn spawn_brightness_up(&self) {
        self.spawn_first_available(BRIGHTNESS_UP_COMMANDS, "brightness up");
    }

    pub fn spawn_brightness_down(&self) {
        self.spawn_first_available(BRIGHTNESS_DOWN_COMMANDS, "brightness down");
    }

    pub fn spawn_media_play(&self) {
        self.spawn_first_available(MEDIA_PLAY_COMMANDS, "media play/pause");
    }

    pub fn spawn_media_next(&self) {
        self.spawn_first_available(MEDIA_NEXT_COMMANDS, "media next");
    }

    pub fn spawn_media_prev(&self) {
        self.spawn_first_available(MEDIA_PREV_COMMANDS, "media prev");
    }

    pub fn spawn_screenshot(&self) {
        self.spawn_first_available(SCREENSHOT_COMMANDS, "screenshot");
    }

    fn spawn_first_available(&self, candidates: &[&[&str]], label: &str) {
        for cmd in candidates {
            if cmd.is_empty() {
                continue;
            }
            if !command_exists(cmd[0]) {
                continue;
            }
            if self.spawn_detached_cmd(cmd, label) {
                return;
            }
        }
        tracing::warn!("No available {} binary found in config candidates", label);
    }

    /// Spawn a child process in its own process group (`process_group(0)`) with a background reaper
    /// thread and full Wayland + systemd D-Bus session environment variables.
    fn spawn_detached_cmd(&self, cmd: &[&str], label: &str) -> bool {
        let prog = cmd[0];
        let mut command = Command::new(prog);
        command
            .args(&cmd[1..])
            .stdin(Stdio::null())
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env("XDG_RUNTIME_DIR", &self.runtime_dir)
            .env("XDG_SESSION_TYPE", "wayland")
            .env("XDG_CURRENT_DESKTOP", "purpwm")
            .env("QT_QPA_PLATFORM", "wayland")
            .env("GDK_BACKEND", "wayland")
            .env("MOZ_ENABLE_WAYLAND", "1")
            .env("ELECTRON_OZONE_PLATFORM_HINT", "auto")
            .process_group(0);

        // Ensure DBUS_SESSION_BUS_ADDRESS points to the real systemd user bus (`/run/user/<uid>/bus`)
        let bus_socket = self.runtime_dir.join("bus");
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() && bus_socket.exists() {
            command.env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", bus_socket.display()),
            );
        }

        match command.spawn() {
            Ok(mut child) => {
                tracing::info!(
                    "Spawned {} on WAYLAND_DISPLAY={}: {:?}",
                    label,
                    self.socket_name,
                    cmd
                );
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                true
            }
            Err(err) => {
                tracing::warn!("Failed to spawn {:?}: {}", cmd, err);
                false
            }
        }
    }

    /// Check if `point` hits an active layer-shell surface (such as `Spotlight`) first.
    pub fn layer_surface_at(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<i32, Logical>)> {
        let p = point.to_i32_round();
        for layer in self.layer_surfaces.iter().rev() {
            if layer.surface.alive() && layer.geo.contains(p) {
                return Some((layer.surface.wl_surface().clone(), layer.geo.loc));
            }
        }
        None
    }

    /// Find the topmost visible window at `point` (in logical coordinates).
    pub fn window_at(&self, point: Point<f64, Logical>) -> Option<(usize, Point<i32, Logical>)> {
        let p = point.to_i32_round();
        for pass_floating in [true, false] {
            for (idx, w) in self.windows.iter().enumerate().rev() {
                if !w.is_visible_on(self.workspace.selected_tags) {
                    continue;
                }
                let is_float_like = w.is_floating || w.is_scratchpad;
                if is_float_like != pass_floating {
                    continue;
                }
                if w.geo.contains(p) {
                    return Some((idx, w.surface_loc()));
                }
            }
        }
        None
    }
}

fn command_exists(prog: &str) -> bool {
    if prog.contains('/') {
        return std::path::Path::new(prog).is_file();
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let full = dir.join(prog);
            if full.is_file() {
                return true;
            }
        }
    }
    false
}

/// Send Wayland frame callbacks to all surfaces in a surface tree.
pub fn send_frames_surface_tree(surface: &wl_surface::WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

// ============================================================================
// SMITHAY PROTOCOL HANDLER IMPLEMENTATIONS
// ============================================================================

impl BufferHandler for PurpState {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl CompositorHandler for PurpState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);

        let needs_rule_check = self
            .windows
            .iter()
            .any(|w| w.wl_surface() == surface && !w.rules_applied);
        if needs_rule_check {
            self.relayout();
        }

        let mut is_layer = false;
        for layer in &mut self.layer_surfaces {
            if layer.surface.wl_surface() == surface {
                is_layer = true;
                let state = with_states(surface, |states| {
                    *states.cached_state.get::<LayerSurfaceCachedState>().current()
                });
                let target_w = if state.size.w > 0 {
                    state.size.w
                } else {
                    740
                };
                let target_h = if state.size.h > 0 {
                    state.size.h
                } else {
                    620
                };
                layer.surface.with_pending_state(|pending| {
                    pending.size = Some(Size::from((target_w, target_h)));
                });
                let _ = layer.surface.send_pending_configure();
                layer.compute_geometry(self.output_size);
            }
        }
        if is_layer {
            self.sync_keyboard_focus();
        }
        self.bar.mark_dirty();
    }
}

impl ShmHandler for PurpState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for PurpState {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        set_primary_focus(dh, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }
}

impl SelectionHandler for PurpState {
    type SelectionUserData = ();
}

impl DataDeviceHandler for PurpState {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl PrimarySelectionHandler for PurpState {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

impl ClientDndGrabHandler for PurpState {}
impl ServerDndGrabHandler for PurpState {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

impl OutputHandler for PurpState {}

impl FractionalScaleHandler for PurpState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional_scale| {
                fractional_scale.set_preferred_scale(1.0);
            });
        });
    }
}

impl XdgDecorationHandler for PurpState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_pending_configure();
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_pending_configure();
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_pending_configure();
    }
}

impl XdgActivationHandler for PurpState {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation_state
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        if let Some(idx) = self.windows.iter().position(|w| w.wl_surface() == &surface) {
            if self.windows[idx].is_visible_on(self.workspace.selected_tags) {
                self.focused_idx = Some(idx);
            } else {
                self.windows[idx].is_urgent = true;
            }
            self.relayout();
        }
    }
}

impl WlrLayerShellHandler for PurpState {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        _output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        let state = with_states(surface.wl_surface(), |states| {
            *states.cached_state.get::<LayerSurfaceCachedState>().current()
        });
        let w = if state.size.w > 0 { state.size.w } else { 740 };
        let h = if state.size.h > 0 { state.size.h } else { 620 };

        surface.with_pending_state(|pending| {
            pending.size = Some(Size::from((w, h)));
        });
        surface.send_configure();

        let mut managed = ManagedLayerSurface {
            surface,
            layer,
            namespace,
            geo: Rectangle::new(
                Point::from(((self.output_size.w - w) / 2, (self.output_size.h - h) / 2)),
                Size::from((w, h)),
            ),
        };
        managed.compute_geometry(self.output_size);
        self.layer_surfaces.push(managed);
        self.relayout();
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        self.layer_surfaces.retain(|l| l.surface != surface);
        self.relayout();
    }
}

impl XdgShellHandler for PurpState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        let mut win = ManagedWindow::new(surface, self.workspace.selected_tags, self.output_size);
        if self.pending_scratchpad_spawn {
            self.pending_scratchpad_spawn = false;
            win.is_scratchpad = true;
            win.is_floating = true;
            let sw = (self.output_size.w * 7 / 10).max(480);
            let sh = (self.output_size.h * 7 / 10).max(320);
            let sx = (self.output_size.w - sw) / 2;
            let sy = (self.output_size.h - sh) / 2;
            win.float_geo = Rectangle::new(Point::from((sx, sy)), Size::from((sw, sh)));
            win.geo = win.float_geo;
        }
        self.windows.insert(0, win);
        self.focused_idx = Some(0);
        self.relayout();
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.windows.retain(|w| w.toplevel != surface);
        self.ensure_valid_focus();
        self.relayout();
        #[cfg(target_os = "linux")]
        unsafe {
            libc::malloc_trim(0);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        let _ = surface.send_configure();
        self.popups.push(surface);
    }

    fn popup_destroyed(&mut self, surface: PopupSurface) {
        self.popups.retain(|p| p != &surface);
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        _positioner: PositionerState,
        token: u32,
    ) {
        let _ = surface.send_repositioned(token);
        let _ = surface.send_configure();
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<WlOutput>) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.toplevel == surface) {
            w.is_fullscreen = true;
            self.relayout();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(w) = self.windows.iter_mut().find(|w| w.toplevel == surface) {
            w.is_fullscreen = false;
            self.relayout();
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Maximized);
        });
        surface.send_pending_configure();
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        surface.with_pending_state(|state| {
            state.states.unset(xdg_toplevel::State::Maximized);
        });
        surface.send_pending_configure();
    }
}

delegate_compositor!(PurpState);
delegate_shm!(PurpState);
delegate_seat!(PurpState);
delegate_data_device!(PurpState);
delegate_primary_selection!(PurpState);
delegate_viewporter!(PurpState);
delegate_single_pixel_buffer!(PurpState);
delegate_fractional_scale!(PurpState);
delegate_output!(PurpState);
delegate_xdg_shell!(PurpState);
delegate_xdg_decoration!(PurpState);
delegate_xdg_activation!(PurpState);
delegate_layer_shell!(PurpState);
