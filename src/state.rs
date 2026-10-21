//! Core compositor state (`PurpState`), Wayland protocol trait implementations,
//! window focus/stack management, `wlr-layer-shell` overlay support (for `Spotlight`),
//! `xdg-decoration` (server-side decorations), `primary-selection`, `wlr-data-control`,
//! `idle-inhibit`, `viewporter`, `single-pixel-buffer`, `fractional-scale`, `xdg-activation`,
//! native touch (`wl_touch`), built-in lock screen, notifications, scratchpad, and command spawning.

use std::{
    collections::HashMap,
    io::Write,
    os::unix::{io::OwnedFd, process::CommandExt},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{mpsc, Arc},
};

use smithay::{
    backend::{
        input::{Keycode, TouchSlot},
        renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state},
    },
    delegate_compositor, delegate_cursor_shape, delegate_data_control, delegate_data_device,
    delegate_fractional_scale, delegate_idle_inhibit, delegate_layer_shell, delegate_output,
    delegate_primary_selection, delegate_seat, delegate_shm, delegate_single_pixel_buffer,
    delegate_viewporter, delegate_xdg_activation, delegate_xdg_decoration, delegate_xdg_shell,
    desktop::{
        find_popup_root_surface, get_popup_toplevel_coords, utils::under_from_surface_tree,
        PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy,
        WindowSurfaceType,
    },
    input::{
        keyboard::{KeyboardHandle, ModifiersState},
        pointer::{CursorImageStatus, Focus, MotionEvent, PointerHandle},
        touch::TouchHandle,
        Seat, SeatHandler, SeatState,
    },
    output::Output,
    reexports::{
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            shell::server::xdg_toplevel,
        },
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{
                wl_buffer,
                wl_data_source::WlDataSource,
                wl_output::WlOutput,
                wl_seat,
                wl_surface::{self, WlSurface},
            },
            Client, DisplayHandle, Resource,
        },
    },
    utils::{IsAlive, Logical, Point, Rectangle, Serial, Size, Transform, SERIAL_COUNTER},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            send_surface_state, with_states, with_surface_tree_downward, CompositorClientState,
            CompositorHandler, CompositorState, SurfaceAttributes, TraversalAction,
        },
        cursor_shape::CursorShapeManagerState,
        fractional_scale::{
            with_fractional_scale, FractionalScaleHandler, FractionalScaleManagerState,
        },
        idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState},
        output::{OutputHandler, OutputManagerState},
        selection::{
            data_device::{
                set_data_device_focus, set_data_device_selection, ClientDndGrabHandler,
                DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
            primary_selection::{
                set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
            },
            wlr_data_control::{DataControlHandler, DataControlState},
            SelectionHandler, SelectionTarget,
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
        tablet_manager::TabletSeatHandler,
        viewporter::ViewporterState,
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
    },
};

use crate::{
    bar::StatusBar,
    config::{
        AUTOSTART_COMMANDS, BAR_HEIGHT, BLUETOOTH_COMMANDS, BORDER_WIDTH, BRIGHTNESS_DOWN_COMMANDS,
        BRIGHTNESS_UP_COMMANDS, DEFAULT_SCALE, LAUNCHER_COMMANDS, MEDIA_NEXT_COMMANDS,
        MEDIA_PLAY_COMMANDS, MEDIA_PREV_COMMANDS, MIC_MUTE_COMMANDS, MIXER_COMMANDS, MOVE_STEP,
        POLKIT_COMMANDS, RESIZE_STEP, SCALE_MAX, SCALE_MIN, SCALE_STEP, SCREENSHOT_DIR,
        SYSTEM_MONITOR_COMMANDS, TAGMASK, TERMINAL_COMMANDS, VOL_DOWN_COMMANDS, VOL_MUTE_COMMANDS,
        VOL_UP_COMMANDS, WIFI_COMMANDS, WINDOW_RULES, window_rules_settled,
    },
    cursor::CursorTheme,
    help::HelpOverlay,
    lock::{LockPoll, LockScreen},
    notify::NotificationCenter,
    wm::{scratchpad_geo, LayoutMode, ManagedWindow, Workspace},
};

/// Which edges of a window a resize grab pulls. A left or top edge moves the
/// origin so the opposite edge stays put.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeEdges {
    pub left: bool,
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
}

impl ResizeEdges {
    pub const BOTTOM_RIGHT: Self = Self {
        left: false,
        top: false,
        right: true,
        bottom: true,
    };
}

/// Apply `dx`/`dy` (pointer movement since the grab started) to `start`.
/// `min_w` / `min_h` are the smallest frame size; the caller may still clamp
/// to the client's size hints afterwards.
pub(crate) fn resize_rect(
    start: Rectangle<i32, Logical>,
    dx: i32,
    dy: i32,
    edges: ResizeEdges,
    min_w: i32,
    min_h: i32,
) -> Rectangle<i32, Logical> {
    let mut x = start.loc.x;
    let mut y = start.loc.y;
    let mut w = start.size.w;
    let mut h = start.size.h;
    if edges.left {
        let new_w = (start.size.w - dx).max(min_w);
        x = start.loc.x + start.size.w - new_w;
        w = new_w;
    } else if edges.right {
        w = (start.size.w + dx).max(min_w);
    }
    if edges.top {
        let new_h = (start.size.h - dy).max(min_h);
        y = start.loc.y + start.size.h - new_h;
        h = new_h;
    } else if edges.bottom {
        h = (start.size.h + dy).max(min_h);
    }
    Rectangle::new(Point::from((x, y)), Size::from((w, h)))
}

/// After swapping the focused window with `target`, which index should stay focused.
/// Zooming the master focuses the window that moved into the master slot (still at
/// `focused`). Zooming any other window focuses it in its new master slot (`target`).
pub(crate) fn focus_after_zoom(master: usize, focused: usize, target: usize) -> usize {
    if master == focused {
        focused
    } else {
        target
    }
}

/// `nmcli -t -f TYPE,STATE device status`: connect only when a wifi device exists
/// and none of them are already connected or connecting. A second, disconnected
/// adapter must not kick `wlan0` off a live network.
pub(crate) fn wifi_needs_autoconnect(nmcli_type_state: &str) -> bool {
    let mut saw_wifi = false;
    let mut busy = false;
    for line in nmcli_type_state.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(3, ':');
        let kind = parts.next().unwrap_or("");
        let state = parts.next().unwrap_or("").trim();
        if kind != "wifi" {
            continue;
        }
        saw_wifi = true;
        if state.starts_with("connect") {
            busy = true;
        }
    }
    saw_wifi && !busy
}

/// Active mouse or touch drag operation (`Mod + Left-Drag` / `Mod + Touch-Drag` to move,
/// `Mod + Right-Drag` to resize, or client-initiated `xdg_toplevel.move`/`resize`).
/// Tracks the window by surface because window indices shift when windows map or unmap.
/// `forward_release` is set when the client already received the button/touch down
/// (an `xdg_toplevel` move/resize request) and still needs the matching up event.
#[derive(Debug, Clone)]
pub enum InteractiveGrab {
    Move {
        surface: WlSurface,
        start_pointer: Point<f64, Logical>,
        start_geo: Rectangle<i32, Logical>,
        forward_release: bool,
    },
    Resize {
        surface: WlSurface,
        start_pointer: Point<f64, Logical>,
        start_geo: Rectangle<i32, Logical>,
        edges: ResizeEdges,
        forward_release: bool,
    },
}

impl InteractiveGrab {
    pub(crate) fn forward_release(&self) -> bool {
        match self {
            Self::Move { forward_release, .. } | Self::Resize { forward_release, .. } => {
                *forward_release
            }
        }
    }
}

/// Pending built-in screenshot (`Print` = whole screen, `Shift/Mod + Print` = focused window).
#[derive(Debug, Clone, Copy)]
pub enum ScreenshotRequest {
    Screen,
    Region(Rectangle<i32, Logical>),
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
            *states
                .cached_state
                .get::<LayerSurfaceCachedState>()
                .current()
        });

        // A zero size means "stretch between the anchored edges" (panels, wallpapers);
        // unanchored zero-size surfaces keep the centered launcher fallback.
        let w = if state.size.w > 0 {
            state.size.w.min(output_size.w)
        } else if state.anchor.contains(Anchor::LEFT | Anchor::RIGHT) {
            (output_size.w - state.margin.left - state.margin.right).max(1)
        } else {
            740.min(output_size.w)
        };
        let h = if state.size.h > 0 {
            state.size.h.min(output_size.h)
        } else if state.anchor.contains(Anchor::TOP | Anchor::BOTTOM) {
            (output_size.h - state.margin.top - state.margin.bottom).max(1)
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

        self.layer = state.layer;
        self.geo = Rectangle::new(Point::from((x, y)), Size::from((w, h)));
    }

    /// `OnDemand` surfaces only get focus when clicked (see `layer_focus`), otherwise a
    /// permanent panel would swallow all typing.
    pub fn wants_keyboard_focus(&self) -> bool {
        with_states(self.surface.wl_surface(), |states| {
            let mut binding = states.cached_state.get::<LayerSurfaceCachedState>();
            let cached = binding.current();
            cached.keyboard_interactivity == KeyboardInteractivity::Exclusive
        })
    }

    /// Whether the client has attached a buffer (unmapped surfaces must not take input).
    pub fn is_mapped(&self) -> bool {
        with_renderer_surface_state(self.surface.wl_surface(), |s| s.buffer().is_some())
            .unwrap_or(false)
    }

    /// `Top` / `Overlay` layers render above windows; `Background` / `Bottom` below them.
    pub fn is_above_windows(&self) -> bool {
        matches!(self.layer, Layer::Top | Layer::Overlay)
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
    pub data_control_state: DataControlState,
    pub idle_inhibit_state: IdleInhibitManagerState,
    pub idle_inhibitors: Vec<WlSurface>,
    pub viewporter_state: ViewporterState,
    pub single_pixel_buffer_state: SinglePixelBufferState,
    pub fractional_scale_state: FractionalScaleManagerState,
    pub cursor_shape_state: CursorShapeManagerState,
    pub output_manager_state: OutputManagerState,

    // Input seat & handles
    pub seat: Seat<Self>,
    pub keyboard: KeyboardHandle<Self>,
    pub pointer: PointerHandle<Self>,
    pub touch: TouchHandle<Self>,
    pub pointer_location: Point<f64, Logical>,
    pub touch_locations: HashMap<TouchSlot, Point<f64, Logical>>,
    pub touch_grab_slot: Option<TouchSlot>,
    pub cursor_status: CursorImageStatus,
    /// System XCursor theme used to draw the pointer.
    pub cursor_theme: CursorTheme,
    pub dnd_icon: Option<WlSurface>,
    pub modifiers: ModifiersState,

    /// Solo-Mod tap detection: set `true` when `Mod` is pressed alone, cleared if any
    /// other key or mouse button is pressed while `Mod` is held. On `Mod` release,
    /// if still `true`, launches the `Spotlight` app launcher.
    pub mod_tap_candidate: bool,
    /// Keys whose press triggered a compositor binding; their release is swallowed too.
    pub intercepted_keys: Vec<Keycode>,
    /// Window the pointer was last over (focus follows the pointer only when it enters a new window).
    pub hovered_surface: Option<WlSurface>,
    /// Accumulated scroll on the status bar so touchpads don't flick through every tag.
    pub bar_scroll_accum: f64,

    // Window manager & workspace state
    pub output_size: Size<i32, Logical>,
    pub workspace: Workspace,
    pub windows: Vec<ManagedWindow>,
    pub popup_manager: PopupManager,
    pub layer_surfaces: Vec<ManagedLayerSurface>,
    /// `OnDemand` layer surface the user clicked into (keeps keyboard focus until a window is clicked).
    pub layer_focus: Option<WlSurface>,
    pub focused_idx: Option<usize>,
    pub active_grab: Option<InteractiveGrab>,

    // Built-in screenshots: request is served by the next rendered frame, the encoded PNG
    // comes back from the encoder thread and is offered on the clipboard.
    pub screenshot_request: Option<ScreenshotRequest>,
    pub screenshot_tx: mpsc::Sender<Arc<Vec<u8>>>,
    pub screenshot_rx: mpsc::Receiver<Arc<Vec<u8>>>,

    /// Output scale (logical -> physical pixels); see `config::DEFAULT_SCALE`.
    pub scale: f64,
    /// The single `wl_output`; surfaces are told they are on it (`wl_surface.enter`) so
    /// clients that pick their scale per output (GTK 3, ...) render sharply.
    pub output: Option<Output>,

    // Built-in status bar, lock screen, notification center & keybinding cheat sheet
    pub bar: StatusBar,
    pub lock_screen: Option<LockScreen>,
    pub notifications: NotificationCenter,
    pub help: HelpOverlay,
}

/// Convert physical window touch coordinates `(phys_x, phys_y)` into logical output coordinates
/// at `scale`, clamped to `output_size`. Uses raw physical coordinates directly rather than
/// Smithay's `WinitTouchStartedEvent::y_transformed`, which divides Y by width instead of height.
pub fn touch_physical_to_logical_coords(
    phys_x: f64,
    phys_y: f64,
    scale: f64,
    output_size: Size<i32, Logical>,
) -> Point<f64, Logical> {
    let s = scale.max(0.1);
    let max_x = (output_size.w - 1).max(0) as f64;
    let max_y = (output_size.h - 1).max(0) as f64;
    Point::from((
        (phys_x / s).clamp(0.0, max_x),
        (phys_y / s).clamp(0.0, max_y),
    ))
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
        let data_control_state = DataControlState::new::<Self, _>(
            &display_handle,
            Some(&primary_selection_state),
            |_| true,
        );
        let idle_inhibit_state = IdleInhibitManagerState::new::<Self>(&display_handle);
        let viewporter_state = ViewporterState::new::<Self>(&display_handle);
        let single_pixel_buffer_state = SinglePixelBufferState::new::<Self>(&display_handle);
        let fractional_scale_state = FractionalScaleManagerState::new::<Self>(&display_handle);
        let cursor_shape_state = CursorShapeManagerState::new::<Self>(&display_handle);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&display_handle);

        let mut seat = seat_state.new_wl_seat(&display_handle, "seat-0");
        let keyboard = seat
            .add_keyboard(Default::default(), 200, 25)
            .expect("Failed to initialize Wayland seat keyboard");
        let pointer = seat.add_pointer();
        let touch = seat.add_touch();

        let bar = StatusBar::new(output_size.w);
        let notifications = NotificationCenter::new();

        let uid = unsafe { libc::getuid() };
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", uid)));
        let (screenshot_tx, screenshot_rx) = mpsc::channel();

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
            data_control_state,
            idle_inhibit_state,
            idle_inhibitors: Vec::new(),
            viewporter_state,
            single_pixel_buffer_state,
            fractional_scale_state,
            cursor_shape_state,
            output_manager_state,
            seat,
            keyboard,
            pointer,
            touch,
            pointer_location: Point::from((output_size.w as f64 / 2.0, output_size.h as f64 / 2.0)),
            touch_locations: HashMap::new(),
            touch_grab_slot: None,
            cursor_status: CursorImageStatus::default_named(),
            cursor_theme: CursorTheme::load(),
            dnd_icon: None,
            modifiers: ModifiersState::default(),
            mod_tap_candidate: false,
            intercepted_keys: Vec::new(),
            hovered_surface: None,
            bar_scroll_accum: 0.0,
            output_size,
            workspace: Workspace::default(),
            windows: Vec::new(),
            popup_manager: PopupManager::default(),
            layer_surfaces: Vec::new(),
            layer_focus: None,
            focused_idx: None,
            active_grab: None,
            screenshot_request: None,
            screenshot_tx,
            screenshot_rx,
            scale: 1.0,
            output: None,
            bar,
            lock_screen: None,
            notifications,
            help: HelpOverlay::default(),
        }
    }

    /// Convert physical touch coordinates `(phys_x, phys_y)` into logical output coordinates.
    pub fn touch_physical_to_logical(&self, phys_x: f64, phys_y: f64) -> Point<f64, Logical> {
        touch_physical_to_logical_coords(phys_x, phys_y, self.scale, self.output_size)
    }

    /// Whether the built-in PAM screen locker is currently active.
    pub fn is_locked(&self) -> bool {
        self.lock_screen.is_some()
    }

    /// Engage the built-in purple lock screen (`Mod + Shift + l`, `Mod + Ctrl + l`, `XF86ScreenSaver`, or `SIGUSR2`).
    pub fn lock_session(&mut self) {
        if self.lock_screen.is_some() {
            return;
        }
        self.help.visible = false;
        self.active_grab = None;
        self.touch_grab_slot = None;
        self.touch_locations.clear();
        self.mod_tap_candidate = false;

        let touch = self.touch.clone();
        touch.cancel(self);

        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());

        let pointer = self.pointer.clone();
        pointer.motion(
            self,
            None,
            &MotionEvent {
                location: self.pointer_location,
                serial: SERIAL_COUNTER.next_serial(),
                time: 0,
            },
        );
        pointer.frame(self);

        self.lock_screen = Some(LockScreen::new());
    }

    /// Poll the lock screen's background PAM worker thread.
    /// Returns `true` if the lock screen state changed (unlocked or failed attempt) and needs a redraw.
    pub fn poll_lock_screen(&mut self) -> bool {
        let Some(ref mut lock) = self.lock_screen else {
            return false;
        };
        match lock.poll_result() {
            LockPoll::Unlocked => {
                self.lock_screen = None;
                self.sync_keyboard_focus();
                self.sync_pointer_focus(0);
                true
            }
            LockPoll::Failed => true,
            LockPoll::Pending => false,
        }
    }

    /// Whether any visible client surface has requested `zwp_idle_inhibit_manager_v1`.
    pub fn is_idle_inhibited(&self) -> bool {
        let tags = self.workspace.selected_tags;
        self.idle_inhibitors.iter().any(|surf| {
            surf.alive()
                && self
                    .windows
                    .iter()
                    .any(|w| w.wl_surface() == surf && w.is_visible_on(tags))
        })
    }

    /// Re-run the `dwm` tiling engine, apply `WINDOW_RULES`, update layer surface geometries,
    /// synchronize keyboard + pointer + clipboard focus, and mark the status bar dirty.
    pub fn relayout(&mut self) {
        let prev_len = self.windows.len();
        self.windows.retain(|w| w.toplevel.alive());
        self.popup_manager.cleanup();
        self.layer_surfaces.retain(|l| l.surface.alive());
        self.idle_inhibitors.retain(|s| s.alive());
        if self.dnd_icon.as_ref().is_some_and(|s| !s.alive()) {
            self.dnd_icon = None;
        }
        if self.layer_focus.as_ref().is_some_and(|s| !s.alive()) {
            self.layer_focus = None;
        }

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
        let mut newly_floated = Vec::new();
        for (idx, w) in self.windows.iter_mut().enumerate() {
            if !w.rules_applied {
                let (app_id, title) = w.app_id_and_title();
                if app_id.is_empty() && title.is_empty() {
                    continue;
                }
                w.rule_seen = (app_id.clone(), title.clone());
                let mut matched = false;
                for rule in WINDOW_RULES {
                    if !rule.matches(&app_id, &title) {
                        continue;
                    }
                    matched = true;
                    if rule.tags != 0 {
                        w.tags = rule.tags & TAGMASK;
                    }
                    if rule.is_floating {
                        // Dialogs open at the size they ask for; until they draw,
                        // reserve a centred default box.
                        w.is_floating = true;
                        w.size_from_client = true;
                        let fw = 920
                            .min(out_size.w.saturating_sub(48))
                            .clamp(1, out_size.w.max(1));
                        let fh = 680
                            .min(out_size.h.saturating_sub(64))
                            .clamp(1, out_size.h.max(1));
                        w.float_geo.size = Size::from((fw, fh));
                        newly_floated.push(idx);
                    }
                    if let Some(op) = rule.opacity {
                        w.opacity_override = Some(op);
                    }
                    break;
                }
                if window_rules_settled(matched, &app_id, &title) {
                    w.rules_applied = true;
                }
            }
        }

        for idx in newly_floated {
            let size = self.windows[idx].float_geo.size;
            let loc = self.cascade_loc(size, Some(idx));
            let w = &mut self.windows[idx];
            w.float_geo.loc = loc;
            w.geo = w.float_geo;
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
        self.sync_pointer_focus(0);
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
        if self.is_locked() {
            let keyboard = self.keyboard.clone();
            keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
            return;
        }

        let layer_target = self.keyboard_layer_surface();

        let target_surface = layer_target.or_else(|| {
            self.focused_idx
                .and_then(|idx| self.windows.get(idx))
                .filter(|w| w.is_visible_on(self.workspace.selected_tags))
                .map(|w| w.wl_surface().clone())
        });

        let keyboard = self.keyboard.clone();
        keyboard.set_focus(self, target_surface, SERIAL_COUNTER.next_serial());
    }

    /// Synchronize `wl_pointer` focus (`wl_pointer.enter` / `leave` / `motion`) with the
    /// surface currently under `self.pointer_location`, even when the mouse is stationary.
    /// Needed whenever windows, popups, subsurfaces, or overlays map, unmap, or move under the cursor.
    pub fn sync_pointer_focus(&mut self, time: u32) {
        let under = if self.is_locked() || self.help.visible {
            None
        } else {
            self.surface_under(self.pointer_location)
        };
        let pointer = self.pointer.clone();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: self.pointer_location,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
    }

    /// Change the output scale by `delta` (`Mod + Ctrl + =` / `Mod + Ctrl + -`) and remember it.
    /// The main loop picks up the new scale, updates the `wl_output` and relayouts.
    pub fn adjust_scale(&mut self, delta: f64) {
        let next =
            (((self.scale + delta) / SCALE_STEP).round() * SCALE_STEP).clamp(SCALE_MIN, SCALE_MAX);
        if (next - self.scale).abs() < f64::EPSILON {
            return;
        }
        self.set_scale(next);
        if let Some(path) = scale_file_path() {
            let _ = path
                .parent()
                .map(std::fs::create_dir_all)
                .unwrap_or(Ok(()))
                .and_then(|_| std::fs::write(&path, format!("{}\n", next)));
        }
    }

    /// Apply `scale` to the bar and every existing surface.
    pub fn set_scale(&mut self, scale: f64) {
        self.scale = scale;
        self.bar.set_scale(scale);
        let mut roots: Vec<WlSurface> = self
            .windows
            .iter()
            .map(|w| w.wl_surface().clone())
            .collect();
        roots.extend(
            self.layer_surfaces
                .iter()
                .map(|l| l.surface.wl_surface().clone()),
        );
        for w in &self.windows {
            roots.extend(
                PopupManager::popups_for_surface(w.wl_surface())
                    .map(|(p, _)| p.wl_surface().clone()),
            );
        }
        for root in roots {
            with_surface_tree_downward(
                &root,
                (),
                |_, _, &()| TraversalAction::DoChildren(()),
                |surface, _, &()| apply_preferred_scale(surface, scale),
                |_, _, &()| true,
            );
        }
    }

    /// Whether the status bar is drawn and clickable (hidden under a focused fullscreen window).
    pub fn bar_visible(&self) -> bool {
        self.workspace.show_bar
            && !self
                .focused_idx
                .and_then(|idx| self.windows.get(idx))
                .is_some_and(|w| w.is_fullscreen && w.is_visible_on(self.workspace.selected_tags))
    }

    /// Visible windows from topmost to bottommost. Rendering and hit-testing both use this,
    /// so the window you see under the pointer is the one that gets the input:
    /// fullscreen windows (focused first), then floating windows, then tiled ones
    /// (focused first in each group).
    pub fn stacking_order(&self) -> Vec<usize> {
        let tags = self.workspace.selected_tags;
        let focused = self
            .focused_idx
            .filter(|&i| self.windows.get(i).is_some_and(|w| w.is_visible_on(tags)));
        let floats = |w: &ManagedWindow| (w.is_floating || w.is_scratchpad) && !w.is_fullscreen;

        let mut order = Vec::with_capacity(self.windows.len());
        // Every fullscreen window covers ordinary windows, focused or not.
        if let Some(f) = focused.filter(|&i| self.windows[i].is_fullscreen) {
            order.push(f);
        }
        for (i, w) in self.windows.iter().enumerate() {
            if Some(i) != focused && w.is_visible_on(tags) && w.is_fullscreen {
                order.push(i);
            }
        }
        for pass_floating in [true, false] {
            if let Some(f) = focused
                .filter(|&i| !order.contains(&i) && floats(&self.windows[i]) == pass_floating)
            {
                order.push(f);
            }
            for (i, w) in self.windows.iter().enumerate() {
                if Some(i) != focused
                    && w.is_visible_on(tags)
                    && !w.is_fullscreen
                    && floats(w) == pass_floating
                {
                    order.push(i);
                }
            }
        }
        order
    }

    /// Exclusive layer, else the OnDemand layer the user clicked, if it is still mapped.
    /// An unmapped launcher must not keep the keyboard after it hides.
    fn keyboard_layer_surface(&self) -> Option<WlSurface> {
        self.layer_surfaces
            .iter()
            .rev()
            .find(|l| l.surface.alive() && l.is_mapped() && l.wants_keyboard_focus())
            .map(|l| l.surface.wl_surface().clone())
            .or_else(|| {
                let focus = self.layer_focus.as_ref()?;
                self.layer_surfaces
                    .iter()
                    .any(|l| l.surface.wl_surface() == focus && l.surface.alive() && l.is_mapped())
                    .then(|| focus.clone())
            })
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
            self.sync_keyboard_focus();
            self.sync_pointer_focus(0);
            return;
        }

        let len = visible.len() as isize;
        // No current focus (or focus on a hidden window) should land on the
        // first or last visible window, not skip past it.
        let next_pos = match self
            .focused_idx
            .and_then(|cur| visible.iter().position(|&i| i == cur))
        {
            Some(current_pos) => (current_pos as isize + delta).rem_euclid(len) as usize,
            None if delta < 0 => visible.len() - 1,
            None => 0,
        };
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
        w.size_from_client = false;
        let wanted = Size::from((
            (w.float_geo.size.w + dw * RESIZE_STEP).max(120),
            (w.float_geo.size.h + dh * RESIZE_STEP).max(80),
        ));
        w.float_geo.size = w.clamp_frame_to_client_limits(wanted);
        w.geo = w.float_geo;
        self.relayout();
    }

    /// Promote the focused window to the master slot (`dwm` `zoom()`); zooming the master
    /// swaps it with the next tiled window. Floating windows are not part of the tile order.
    pub fn zoom_focused(&mut self) {
        let Some(focused) = self.focused_idx else {
            return;
        };
        let tags = self.workspace.selected_tags;
        let is_tiled = |w: &ManagedWindow| {
            w.is_visible_on(tags) && !w.is_floating && !w.is_fullscreen && !w.is_scratchpad
        };
        if !self.windows.get(focused).is_some_and(is_tiled) {
            return;
        }

        let mut tiled = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, w)| is_tiled(w))
            .map(|(i, _)| i);
        let master = tiled.next();
        let target = if master == Some(focused) {
            tiled.next()
        } else {
            master
        };

        if let (Some(master), Some(target)) = (master, target) {
            self.windows.swap(target, focused);
            self.focused_idx = Some(focus_after_zoom(master, focused, target));
            self.relayout();
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
        // Switching to `><>` leaves tiled windows where they are (like dwm) instead of
        // piling them all onto their old default floating spot.
        if mode == LayoutMode::Floating && self.workspace.layout() != LayoutMode::Floating {
            let tags = self.workspace.selected_tags;
            for w in &mut self.windows {
                if w.is_visible_on(tags) && !w.is_floating && !w.is_scratchpad && !w.is_fullscreen {
                    w.float_geo = w.geo;
                }
            }
        }
        self.workspace.set_layout(mode);
        self.relayout();
    }

    /// Top-left corner for a new floating window of `size`: centred below the bar, stepped
    /// diagonally while another visible floating window already sits there, so floating
    /// windows never hide each other completely.
    pub fn cascade_loc(&self, size: Size<i32, Logical>, skip: Option<usize>) -> Point<i32, Logical> {
        const STEP: i32 = 32;
        let top = if self.workspace.show_bar { BAR_HEIGHT } else { 0 };
        let out = self.output_size;
        let start = Point::<i32, Logical>::from((
            ((out.w - size.w) / 2).max(0),
            (top + (out.h - top - size.h) / 2).max(top),
        ));
        let tags = self.workspace.selected_tags;
        let taken = |p: Point<i32, Logical>| {
            self.windows.iter().enumerate().any(|(i, w)| {
                Some(i) != skip
                    && (w.is_floating || w.is_scratchpad)
                    && w.is_visible_on(tags)
                    && (w.float_geo.loc.x - p.x).abs() < STEP / 2
                    && (w.float_geo.loc.y - p.y).abs() < STEP / 2
            })
        };
        let clamp = |p: Point<i32, Logical>| {
            Point::<i32, Logical>::from((
                p.x.clamp(0, (out.w - size.w).max(0)),
                p.y.clamp(top, (out.h - size.h).max(top)),
            ))
        };
        let mut loc = start;
        for step in 1..=12 {
            if !taken(loc) {
                return clamp(loc);
            }
            loc = Point::from((start.x + step * STEP, start.y + step * STEP));
            if loc.x + size.w > out.w || loc.y + size.h > out.h {
                // Ran off the screen: continue the cascade from the top-left instead.
                loc = Point::from((STEP * step, top + STEP * step));
            }
        }
        clamp(loc)
    }

    /// For self-sizing floating windows, make the frame match what the client actually drew
    /// (first time: also centre it). Runs on every commit of a toplevel.
    fn adopt_client_size(&mut self, surface: &WlSurface) {
        let Some(idx) = self.windows.iter().position(|w| w.wl_surface() == surface) else {
            return;
        };
        let w = &self.windows[idx];
        // Only once the client has acted on our "pick your own size" configure.
        if !w.size_from_client
            || !(w.is_floating || w.is_scratchpad)
            || w.is_fullscreen
            || w.toplevel.current_state().size.is_some()
        {
            return;
        }
        let Some(client) = w.client_geometry_size() else {
            return;
        };
        let frame = Size::from((client.w + 2 * BORDER_WIDTH, client.h + 2 * BORDER_WIDTH));
        if frame == w.float_geo.size && w.client_sized_once {
            return;
        }
        let top = if self.workspace.show_bar { BAR_HEIGHT } else { 0 };
        let limit: Size<i32, Logical> =
            Size::from((self.output_size.w - 32, self.output_size.h - top - 32));
        let clamped = Size::from((frame.w.min(limit.w), frame.h.min(limit.h)));
        let first = !w.client_sized_once;
        let loc = if first {
            self.cascade_loc(clamped, Some(idx))
        } else {
            w.float_geo.loc
        };

        let w = &mut self.windows[idx];
        w.client_sized_once = true;
        // Too big for the screen: from now on we pick the (clamped) size.
        if clamped != frame {
            w.size_from_client = false;
        }
        w.float_geo = Rectangle::new(loc, clamped);
        w.geo = w.float_geo;
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
                self.windows[idx].float_geo =
                    scratchpad_geo(self.output_size, self.workspace.show_bar);
                self.windows[idx].geo = self.windows[idx].float_geo;
                self.focused_idx = Some(idx);
            } else if self.focused_idx == Some(idx) {
                self.ensure_valid_focus();
            }
            self.relayout();
        } else {
            // Only arm the flag if a terminal actually launched, otherwise the next
            // unrelated window would be swallowed as the scratchpad.
            self.pending_scratchpad_spawn =
                self.spawn_first_available(TERMINAL_COMMANDS, "scratchpad terminal");
        }
    }

    /// Send `xdg_toplevel.close()` to the currently focused window (`Mod + q`).
    pub fn close_focused(&mut self) {
        if let Some(idx) = self.focused_idx {
            if let Some(w) = self.windows.get(idx) {
                w.toplevel.send_close();
            }
        }
    }

    /// Toggle `/home/mag/Documents/Spotlight` as the app launcher.
    /// Fast path: write `"Toggle"\n` directly to the daemon's Unix domain socket (`$XDG_RUNTIME_DIR/spotlight.sock`),
    /// taking ~0.03ms with zero fork/exec overhead.
    /// Fallback: if the socket connection fails (daemon not running), spawn the binary via [`LAUNCHER_COMMANDS`].
    pub fn spawn_launcher(&self) {
        let sock_path = self.runtime_dir.join("spotlight.sock");
        if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&sock_path) {
            if stream.write_all(b"\"Toggle\"\n").is_ok() && stream.flush().is_ok() {
                return;
            }
        }
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
                if wifi_needs_autoconnect(&text) {
                    let _ = Command::new("nmcli")
                        .args(["device", "connect", "wlan0"])
                        .status();
                }
            }

            // 2. Ensure Bluetooth is unblocked and powered on
            let _ = Command::new("rfkill")
                .args(["unblock", "bluetooth"])
                .status();
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
                    let _ = Command::new("nmcli")
                        .args(["radio", "wifi", "off"])
                        .status();
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
                    let _ = Command::new("bluetoothctl").args(["power", "off"]).status();
                } else {
                    let _ = Command::new("rfkill")
                        .args(["unblock", "bluetooth"])
                        .status();
                    let _ = Command::new("bluetoothctl").args(["power", "on"]).status();
                }
            }
        });
        self.bar.request_immediate_status_refresh();
    }

    /// Spawn the polkit authentication agent (`purpwm-polkit-agent`).
    pub fn spawn_polkit_agent(&self) {
        self.spawn_first_available(POLKIT_COMMANDS, "polkit agent");
    }

    /// Start the built-in D-Bus notification daemon (`org.freedesktop.Notifications`).
    pub fn spawn_notification_daemon(&self) {
        self.notifications.start_dbus_listener(&self.runtime_dir);
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

    /// Open the audio mixer (Right-Click on the `VOL` status bar pill).
    pub fn spawn_mixer(&self) {
        self.spawn_first_available(MIXER_COMMANDS, "audio mixer");
    }

    pub fn spawn_brightness_up(&self) {
        if !self.spawn_first_available(BRIGHTNESS_UP_COMMANDS, "brightness up") {
            adjust_backlight_via_logind(5);
        }
    }

    pub fn spawn_brightness_down(&self) {
        if !self.spawn_first_available(BRIGHTNESS_DOWN_COMMANDS, "brightness down") {
            adjust_backlight_via_logind(-5);
        }
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

    /// Capture the whole screen (`Print`) or just the focused window (`Shift/Mod + Print`)
    /// on the next rendered frame. No `wlr-screencopy` / `grim` needed.
    pub fn request_screenshot(&mut self, focused_window_only: bool) {
        if self.is_locked() {
            return;
        }
        let window_geo = self
            .focused_idx
            .and_then(|idx| self.windows.get(idx))
            .filter(|w| w.is_visible_on(self.workspace.selected_tags))
            .map(|w| w.geo);
        self.screenshot_request = match (focused_window_only, window_geo) {
            (true, Some(geo)) => Some(ScreenshotRequest::Region(geo)),
            _ => Some(ScreenshotRequest::Screen),
        };
    }

    /// Encode captured RGBA pixels to `~/Pictures/Screenshots/purpwm-<timestamp>.png` on a
    /// background thread; the PNG is then handed back to be offered on the clipboard.
    pub fn save_screenshot(&self, width: u32, height: u32, rgba: Vec<u8>) {
        let dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(SCREENSHOT_DIR);
        let tx = self.screenshot_tx.clone();
        std::thread::spawn(move || {
            let mut png_bytes = Vec::new();
            let encoded = {
                let mut encoder = png::Encoder::new(&mut png_bytes, width, height);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder
                    .write_header()
                    .and_then(|mut writer| writer.write_image_data(&rgba))
            };
            if let Err(err) = encoded {
                tracing::warn!("Failed to encode screenshot: {}", err);
                return;
            }
            let path = dir.join(format!("purpwm-{}.png", screenshot_timestamp()));
            match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &png_bytes)) {
                Ok(()) => tracing::info!("Saved screenshot to {}", path.display()),
                Err(err) => tracing::warn!("Failed to save screenshot {}: {}", path.display(), err),
            }
            let _ = tx.send(Arc::new(png_bytes));
        });
    }

    /// Offer the latest screenshot PNG as the clipboard selection (`image/png`).
    pub fn offer_screenshot_clipboard(&mut self, png_bytes: Arc<Vec<u8>>) {
        set_data_device_selection(
            &self.display_handle,
            &self.seat,
            vec!["image/png".to_string()],
            png_bytes,
        );
    }

    /// Returns `true` if one of the candidate commands was spawned.
    fn spawn_first_available(&self, candidates: &[&[&str]], label: &str) -> bool {
        for cmd in candidates {
            if cmd.is_empty() {
                continue;
            }
            if !command_exists(cmd[0]) {
                continue;
            }
            if self.spawn_detached_cmd(cmd, label) {
                return true;
            }
        }
        tracing::warn!("No available {} binary found in config candidates", label);
        false
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
            .env("WGPU_BACKEND", "gl")
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

    /// Check if `point` hits a mapped `Top`/`Overlay` layer-shell surface (such as `Spotlight`),
    /// respecting its surface tree and input region.
    pub fn layer_surface_at(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<i32, Logical>)> {
        for layer in self.layer_surfaces.iter().rev() {
            if layer.surface.alive()
                && layer.namespace != "Spotlight-keepalive"
                && layer.is_above_windows()
                && layer.is_mapped()
                && layer.geo.to_f64().contains(point)
                && under_from_surface_tree(
                    layer.surface.wl_surface(),
                    point,
                    layer.geo.loc,
                    WindowSurfaceType::ALL,
                )
                .is_some()
            {
                return Some((layer.surface.wl_surface().clone(), layer.geo.loc));
            }
        }
        None
    }

    /// Find the topmost visible window at `point` (in logical coordinates).
    pub fn window_at(&self, point: Point<f64, Logical>) -> Option<(usize, Point<i32, Logical>)> {
        self.stacking_order()
            .into_iter()
            .find(|&idx| self.windows[idx].geo.to_f64().contains(point))
            .map(|idx| (idx, self.windows[idx].surface_loc()))
    }

    /// Popups of every visible overlay and window (topmost first) with their buffer origin in global coordinates.
    pub fn visible_popups(&self) -> Vec<(PopupKind, Point<i32, Logical>)> {
        let mut out = Vec::new();
        for layer in self.layer_surfaces.iter().rev() {
            if layer.surface.alive()
                && layer.namespace != "Spotlight-keepalive"
                && layer.is_mapped()
            {
                for (popup, offset) in PopupManager::popups_for_surface(layer.surface.wl_surface())
                {
                    let loc = layer.geo.loc + offset - popup.geometry().loc;
                    out.push((popup, loc));
                }
            }
        }
        for idx in self.stacking_order() {
            let w = &self.windows[idx];
            let content_loc = w.content_loc();
            for (popup, offset) in PopupManager::popups_for_surface(w.wl_surface()) {
                let loc = content_loc + offset - popup.geometry().loc;
                out.push((popup, loc));
            }
        }
        out
    }

    /// Whether `point` is over a visible popup (menu, dropdown, tooltip).
    pub fn popup_at(&self, point: Point<f64, Logical>) -> bool {
        self.visible_popups().into_iter().any(|(popup, loc)| {
            under_from_surface_tree(popup.wl_surface(), point, loc, WindowSurfaceType::ALL)
                .is_some()
        })
    }

    /// Exact surface under `point` for pointer/touch focus: overlays, then popup menus, then the
    /// window's surface tree (subsurfaces respected), falling back to the toplevel itself
    /// when the pointer is over its border.
    pub fn surface_under(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if let Some((surf, loc)) = self.layer_surface_at(point) {
            return Some(
                under_from_surface_tree(&surf, point, loc, WindowSurfaceType::ALL)
                    .map(|(s, l)| (s, l.to_f64()))
                    .unwrap_or_else(|| (surf, loc.to_f64())),
            );
        }
        for (popup, loc) in self.visible_popups() {
            if let Some((surf, surf_loc)) =
                under_from_surface_tree(popup.wl_surface(), point, loc, WindowSurfaceType::ALL)
            {
                return Some((surf, surf_loc.to_f64()));
            }
        }
        self.window_at(point).map(|(idx, surface_loc)| {
            let root = self.windows[idx].wl_surface();
            under_from_surface_tree(root, point, surface_loc, WindowSurfaceType::ALL)
                .map(|(surf, loc)| (surf, loc.to_f64()))
                .unwrap_or_else(|| (root.clone(), surface_loc.to_f64()))
        })
    }

    /// Start a client-initiated interactive window move or resize (`xdg_toplevel.move` / `resize`).
    /// `edges` is `None` for a move. The button or touch that started the grab was
    /// already delivered to the client, so its release is forwarded too.
    fn begin_client_interactive_grab(
        &mut self,
        surface: &ToplevelSurface,
        edges: Option<xdg_toplevel::ResizeEdge>,
    ) {
        if self.is_locked() {
            return;
        }
        let Some(idx) = self.windows.iter().position(|w| &w.toplevel == surface) else {
            return;
        };
        if !self.windows[idx].is_visible_on(self.workspace.selected_tags) {
            return;
        }
        self.focused_idx = Some(idx);
        let active_touch = self
            .touch_locations
            .iter()
            .next()
            .map(|(&slot, &pos)| (slot, pos));
        let start_pointer = active_touch
            .map(|(_, pos)| pos)
            .unwrap_or(self.pointer_location);
        self.touch_grab_slot = active_touch.map(|(slot, _)| slot);

        let w = &mut self.windows[idx];
        if !w.is_floating && !w.is_scratchpad {
            w.is_floating = true;
            w.float_geo = w.geo;
        }
        let start_geo = w.float_geo;
        let wl_surface = w.wl_surface().clone();
        self.active_grab = Some(if let Some(edge) = edges {
            InteractiveGrab::Resize {
                surface: wl_surface,
                start_pointer,
                start_geo,
                edges: resize_edges_from_request(edge),
                forward_release: true,
            }
        } else {
            InteractiveGrab::Move {
                surface: wl_surface,
                start_pointer,
                start_geo,
                forward_release: true,
            }
        });
        self.relayout();
    }

    /// Clamp a new or repositioned popup to the visible output area below the status bar.
    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let kind = PopupKind::Xdg(popup.clone());
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let root_loc = if let Some(w) = self.windows.iter().find(|w| w.wl_surface() == &root) {
            w.content_loc()
        } else if let Some(l) = self
            .layer_surfaces
            .iter()
            .find(|l| l.surface.wl_surface() == &root)
        {
            l.geo.loc
        } else {
            return;
        };

        let top = if self.workspace.show_bar {
            BAR_HEIGHT
        } else {
            0
        };
        let mut target = Rectangle::new(
            Point::from((0, top)),
            Size::from((self.output_size.w.max(1), (self.output_size.h - top).max(1))),
        );
        // The positioner works relative to the parent's window geometry.
        target.loc -= get_popup_toplevel_coords(&kind);
        target.loc -= root_loc;
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

/// Tell a surface the scale to render at: exact fractional scale for clients supporting
/// `wp_fractional_scale_v1`, the next integer buffer scale for the rest (downsampled crisply).
fn apply_preferred_scale(surface: &WlSurface, scale: f64) {
    with_states(surface, |states| {
        with_fractional_scale(states, |fractional_scale| {
            fractional_scale.set_preferred_scale(scale);
        });
        send_surface_state(surface, states, scale.ceil() as i32, Transform::Normal);
    });
}

/// Startup scale: `PURPWM_SCALE`, else the value saved by the scale keybindings, else the default.
pub fn initial_scale() -> f64 {
    std::env::var("PURPWM_SCALE")
        .ok()
        .or_else(|| std::fs::read_to_string(scale_file_path()?).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite())
        .unwrap_or(DEFAULT_SCALE)
        .clamp(SCALE_MIN, SCALE_MAX)
}

fn scale_file_path() -> Option<PathBuf> {
    let config_dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(config_dir.join("purpwm").join("scale"))
}

/// Built-in brightness fallback when `brightnessctl` is missing: ask logind (no root needed
/// for the active session) to step the first backlight by `percent_delta`.
fn adjust_backlight_via_logind(percent_delta: i32) {
    std::thread::spawn(move || {
        let Some(device) = std::fs::read_dir("/sys/class/backlight")
            .ok()
            .and_then(|mut entries| entries.next())
            .and_then(|e| e.ok())
        else {
            tracing::warn!("No backlight device found in /sys/class/backlight");
            return;
        };
        let read = |name: &str| -> Option<i64> {
            std::fs::read_to_string(device.path().join(name))
                .ok()?
                .trim()
                .parse()
                .ok()
        };
        let (Some(cur), Some(max)) = (read("brightness"), read("max_brightness")) else {
            return;
        };
        let step = (max * percent_delta.abs() as i64 / 100).max(1);
        let next = (if percent_delta >= 0 {
            cur + step
        } else {
            cur - step
        })
        .clamp(max / 100, max);
        let name = device.file_name().to_string_lossy().into_owned();
        let _ = Command::new("busctl")
            .args([
                "call",
                "org.freedesktop.login1",
                "/org/freedesktop/login1/session/auto",
                "org.freedesktop.login1.Session",
                "SetBrightness",
                "ssu",
                "backlight",
                &name,
                &next.to_string(),
            ])
            .status();
    });
}

/// Local `YYYYmmdd-HHMMSS` timestamp for screenshot file names.
fn screenshot_timestamp() -> String {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return "00000000-000000".to_string();
        }
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
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
        // Tell new surfaces which output they are on and which scale to render at
        // (both are no-ops when already sent).
        if let Some(output) = &self.output {
            output.enter(surface);
        }
        apply_preferred_scale(surface, self.scale);

        // Popups get their initial configure in response to their first commit.
        self.popup_manager.commit(surface);
        if let Some(PopupKind::Xdg(ref popup)) = self.popup_manager.find_popup(surface) {
            if !popup.is_initial_configure_sent() {
                let _ = popup.send_configure();
            }
        }

        let needs_rule_check = self.windows.iter().any(|w| {
            if w.rules_applied || w.wl_surface() != surface {
                return false;
            }
            let (app_id, title) = w.app_id_and_title();
            (!app_id.is_empty() || !title.is_empty()) && (app_id, title) != w.rule_seen
        });
        if needs_rule_check {
            self.relayout();
        }
        self.adopt_client_size(surface);

        let mut is_layer = false;
        for layer in &mut self.layer_surfaces {
            if layer.surface.wl_surface() == surface {
                is_layer = true;
                let old_geo = layer.geo;
                layer.compute_geometry(self.output_size);
                if layer.geo != old_geo {
                    let size = layer.geo.size;
                    layer.surface.with_pending_state(|pending| {
                        pending.size = Some(size);
                    });
                    let _ = layer.surface.send_pending_configure();
                }
            }
        }
        if is_layer {
            self.sync_keyboard_focus();
        }
        // A newly mapped or unmapped popup, subsurface, or updated input_region under the
        // stationary pointer must immediately update `wl_pointer` focus.
        self.sync_pointer_focus(0);
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
    /// PNG bytes of the last screenshot, offered by the compositor on the clipboard.
    type SelectionUserData = Arc<Vec<u8>>;

    fn send_selection(
        &mut self,
        _ty: SelectionTarget,
        _mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        user_data: &Self::SelectionUserData,
    ) {
        // Write on a thread: a slow reader must not stall the compositor.
        let png_bytes = Arc::clone(user_data);
        std::thread::spawn(move || {
            let _ = std::fs::File::from(fd).write_all(&png_bytes);
        });
    }
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

impl DataControlHandler for PurpState {
    fn data_control_state(&self) -> &DataControlState {
        &self.data_control_state
    }
}

impl IdleInhibitHandler for PurpState {
    fn inhibit(&mut self, surface: WlSurface) {
        if !self.idle_inhibitors.contains(&surface) {
            self.idle_inhibitors.push(surface);
        }
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle_inhibitors.retain(|s| s != &surface);
    }
}

impl ClientDndGrabHandler for PurpState {
    fn started(
        &mut self,
        _source: Option<WlDataSource>,
        icon: Option<WlSurface>,
        _seat: Seat<Self>,
    ) {
        self.dnd_icon = icon;
    }

    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        self.dnd_icon = None;
    }
}

impl ServerDndGrabHandler for PurpState {
    fn send(&mut self, _mime_type: String, _fd: OwnedFd, _seat: Seat<Self>) {}
}

impl OutputHandler for PurpState {}

/// Needed by `wp_cursor_shape_v1` (cursor shapes requested for tablet tools); no tablets here.
impl TabletSeatHandler for PurpState {}

impl FractionalScaleHandler for PurpState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        apply_preferred_scale(&surface, self.scale);
    }
}

/// Always answer with server-side decorations. The configure is forced: the toplevel was
/// usually configured (with `ServerSide` already pending) before the client created its
/// decoration object, so a "pending changes only" configure would never tell the client,
/// and it would fall back to drawing its own titlebar.
fn force_server_side_decorations(toplevel: &ToplevelSurface) {
    toplevel.with_pending_state(|state| {
        state.decoration_mode = Some(DecorationMode::ServerSide);
    });
    toplevel.send_configure();
}

impl XdgDecorationHandler for PurpState {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        force_server_side_decorations(&toplevel);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        force_server_side_decorations(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        force_server_side_decorations(&toplevel);
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
            *states
                .cached_state
                .get::<LayerSurfaceCachedState>()
                .current()
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

    fn new_popup(&mut self, _parent: LayerSurface, popup: PopupSurface) {
        self.unconstrain_popup(&popup);
        if let Err(err) = self.popup_manager.track_popup(PopupKind::Xdg(popup)) {
            tracing::warn!("Failed to track layer-shell popup: {}", err);
        }
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
        win.float_geo.loc = self.cascade_loc(win.float_geo.size, None);
        win.geo = win.float_geo;
        if self.pending_scratchpad_spawn {
            self.pending_scratchpad_spawn = false;
            win.is_scratchpad = true;
            win.is_floating = true;
            win.float_geo = scratchpad_geo(self.output_size, self.workspace.show_bar);
            win.geo = win.float_geo;
        }
        self.windows.insert(0, win);
        self.focused_idx = Some(0);
        self.relayout();
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        self.windows.retain(|w| w.toplevel != surface);
        if self
            .hovered_surface
            .as_ref()
            .is_some_and(|s| s == surface.wl_surface())
        {
            self.hovered_surface = None;
        }
        self.ensure_valid_focus();
        self.relayout();
        #[cfg(target_os = "linux")]
        unsafe {
            libc::malloc_trim(0);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        // The initial configure is sent on the popup's first commit (see `commit`).
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
        });
        self.unconstrain_popup(&surface);
        if let Err(err) = self.popup_manager.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!("Failed to track popup: {}", err);
        }
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        self.popup_manager.cleanup();
        self.sync_keyboard_focus();
        self.sync_pointer_focus(0);
    }

    /// Explicit popup grab (menus): keyboard goes to the menu and clicking outside dismisses it.
    fn grab(&mut self, surface: PopupSurface, _seat: wl_seat::WlSeat, serial: Serial) {
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else {
            return;
        };
        let seat = self.seat.clone();
        let Ok(mut grab) = self.popup_manager.grab_popup(root, kind, &seat, serial) else {
            return;
        };

        let keyboard = self.keyboard.clone();
        let pointer = self.pointer.clone();
        if keyboard.is_grabbed()
            && !(keyboard.has_grab(serial)
                || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
        {
            grab.ungrab(PopupUngrabStrategy::All);
            return;
        }
        keyboard.set_focus(self, grab.current_grab(), serial);
        keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);

        if pointer.is_grabbed()
            && !(pointer.has_grab(serial)
                || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
        {
            grab.ungrab(PopupUngrabStrategy::All);
            return;
        }
        pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        // `send_repositioned` already includes the configure event.
        surface.send_repositioned(token);
        self.sync_pointer_focus(0);
    }

    fn move_request(&mut self, surface: ToplevelSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        self.begin_client_interactive_grab(&surface, None);
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        self.begin_client_interactive_grab(&surface, Some(edges));
    }

    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<WlOutput>) {
        if let Some(idx) = self.windows.iter().position(|w| w.toplevel == surface) {
            self.windows[idx].is_fullscreen = true;
            if self.windows[idx].is_visible_on(self.workspace.selected_tags) {
                self.focused_idx = Some(idx);
            }
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
        // Tiling has no maximize state. Reconfigure without the bit so the
        // client is not left maximized at its tiled size.
        let _ = surface;
        self.relayout();
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        let _ = surface;
        self.relayout();
    }
}

fn resize_edges_from_request(edge: xdg_toplevel::ResizeEdge) -> ResizeEdges {
    use xdg_toplevel::ResizeEdge::{
        Bottom, BottomLeft, BottomRight, Left, Right, Top, TopLeft, TopRight,
    };
    let (left, top, right, bottom) = match edge {
        Left => (true, false, false, false),
        Right => (false, false, true, false),
        Top => (false, true, false, false),
        Bottom => (false, false, false, true),
        TopLeft => (true, true, false, false),
        TopRight => (false, true, true, false),
        BottomLeft => (true, false, false, true),
        BottomRight => (false, false, true, true),
        _ => (false, false, true, true),
    };
    ResizeEdges {
        left,
        top,
        right,
        bottom,
    }
}

delegate_compositor!(PurpState);
delegate_shm!(PurpState);
delegate_seat!(PurpState);
delegate_data_device!(PurpState);
delegate_primary_selection!(PurpState);
delegate_data_control!(PurpState);
delegate_idle_inhibit!(PurpState);
delegate_viewporter!(PurpState);
delegate_single_pixel_buffer!(PurpState);
delegate_fractional_scale!(PurpState);
delegate_cursor_shape!(PurpState);
delegate_output!(PurpState);
delegate_xdg_shell!(PurpState);
delegate_xdg_decoration!(PurpState);
delegate_xdg_activation!(PurpState);
delegate_layer_shell!(PurpState);
