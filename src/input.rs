//! Keyboard, pointer, and touch input handling for `purpwm`, including:
//! - `Mod + h` keybinding cheat sheet overlay (closed with `Mod + h`, `Escape`, or any click / tap)
//! - Solo `Mod` tap detection (press and release `Mod` alone to launch Spotlight)
//! - `Mod + Arrow` keys to move windows around (and `Mod + Shift + Arrow` to resize)
//! - Full `dwm` keybindings (`Mod + 1..9`, `Mod + j/k`, `Mod + [/]`, `Mod + Return`, etc.)
//! - Built-in PAM lock screen (`Mod + Shift + l` / `Mod + Ctrl + l` / `XF86ScreenSaver`)
//! - Native Wayland touch input (`wl_touch`) with status bar taps, window focus, and `Mod + Touch` move
//! - Wi-Fi (`Mod + w`, `Mod + Shift + w`) and Bluetooth (`Mod + Shift + b`) controls + clickable bar pills
//! - System monitor (`Mod + Escape` or clicking CPU/MEM/BAT pill)
//! - Additional layouts (`Mod + u` CenteredMaster `|M|`, `Mod + g` Grid `###`)
//! - Scratchpad (`Mod + s`), Fullscreen (`Mod + Shift + f` / `F11`), Opaque toggle (`Mod + o`), Vanity Gaps (`Mod + =/-`)
//! - Hardware media/volume/brightness/screenshot keys (`XF86Audio*`, `XF86MonBrightness*`, `Print`)
//! - Mouse scroll-wheel (`PointerAxis`) forwarding and status bar wheel tag switching

use smithay::{
    backend::input::{
        Axis, AxisSource, ButtonState, Event, InputBackend, KeyState, Keycode, PointerAxisEvent,
        TouchSlot,
    },
    input::{
        keyboard::{keysyms, FilterResult, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent, RelativeMotionEvent},
        touch::{DownEvent as TouchDown, MotionEvent as TouchMotion, UpEvent as TouchUp},
    },
    utils::{Logical, Point, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        shell::wlr_layer::{KeyboardInteractivity, LayerSurfaceCachedState},
    },
};

use crate::{
    bar::BarClickAction,
    config::{BAR_HEIGHT, SCALE_STEP, TAGMASK, TAG_NAMES},
    lock::LockKey,
    state::{InteractiveGrab, PurpState},
    wm::LayoutMode,
};

/// Scroll distance (one mouse-wheel notch) needed to switch tags from the status bar.
const BAR_SCROLL_STEP: f64 = 15.0;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// Compositor action triggered by a keybinding or solo-Mod release.
#[derive(Debug, Clone, Copy)]
pub enum KeyAction {
    ToggleHelp,
    SpawnLauncher,
    SpawnTerminal,
    SpawnSystemMonitor,
    SpawnWifi,
    ToggleWifi,
    SpawnBluetooth,
    ToggleBluetooth,
    ToggleScratchpad,
    LockScreen,
    CloseWindow,
    Quit,
    FocusDelta(isize),
    MoveWindow(i32, i32),
    ResizeWindow(i32, i32),
    AdjustMfact(f32),
    AdjustNmaster(isize),
    AdjustGaps(i32),
    AdjustScale(f64),
    ResetGaps,
    ToggleGaps,
    Zoom,
    SetLayout(LayoutMode),
    CycleLayout,
    ToggleLastLayout,
    ToggleFloating,
    ToggleFullscreen,
    ToggleOpacity,
    ToggleBar,
    ViewTags(u16),
    ToggleViewTags(u16),
    TagWindow(u16),
    ToggleTagWindow(u16),
    SwapPrevTags,
    VolumeUp,
    VolumeDown,
    VolumeMute,
    MicMute,
    BrightnessUp,
    BrightnessDown,
    MediaPlay,
    MediaNext,
    MediaPrev,
    Screenshot,
    ScreenshotWindow,
}

#[inline]
fn is_mod_keysym(sym: Keysym) -> bool {
    matches!(
        sym.raw(),
        keysyms::KEY_Super_L
            | keysyms::KEY_Super_R
            | keysyms::KEY_Meta_L
            | keysyms::KEY_Meta_R
            | keysyms::KEY_Alt_L
            | keysyms::KEY_Alt_R
    )
}

#[inline]
fn is_mod_active(mods: &ModifiersState) -> bool {
    mods.logo || mods.alt
}

/// Map a keysym (`1..9` or shifted `!..(`) to tag index `0..8`.
fn keysym_to_tag_index(sym: Keysym) -> Option<usize> {
    match sym.raw() {
        keysyms::KEY_1 | keysyms::KEY_exclam => Some(0),
        keysyms::KEY_2 | keysyms::KEY_at => Some(1),
        keysyms::KEY_3 | keysyms::KEY_numbersign => Some(2),
        keysyms::KEY_4 | keysyms::KEY_dollar => Some(3),
        keysyms::KEY_5 | keysyms::KEY_percent => Some(4),
        keysyms::KEY_6 | keysyms::KEY_asciicircum => Some(5),
        keysyms::KEY_7 | keysyms::KEY_ampersand => Some(6),
        keysyms::KEY_8 | keysyms::KEY_asterisk => Some(7),
        keysyms::KEY_9 | keysyms::KEY_parenleft => Some(8),
        _ => None,
    }
}

/// Translate a key press on the lock screen into a [`LockKey`] edit/submit action.
pub(crate) fn keysym_to_lock_key(sym: Keysym, mods: &ModifiersState) -> Option<LockKey> {
    match sym.raw() {
        keysyms::KEY_Return | keysyms::KEY_KP_Enter => Some(LockKey::Submit),
        keysyms::KEY_BackSpace => Some(LockKey::Backspace),
        keysyms::KEY_Escape => Some(LockKey::Clear),
        keysyms::KEY_u | keysyms::KEY_U if mods.ctrl => Some(LockKey::Clear),
        _ if !mods.ctrl && !mods.alt && !mods.logo => sym
            .key_char()
            .filter(|c| !c.is_control())
            .map(LockKey::Char),
        _ => None,
    }
}

impl PurpState {
    /// Process a physical key press/release event through the Wayland seat keyboard.
    pub fn process_keyboard_event(&mut self, keycode: Keycode, state: KeyState, time: u32) {
        let keyboard = self.keyboard.clone();
        let mut solo_mod_tap = false;

        let intercepted: Option<Option<KeyAction>> = keyboard.input(
            self,
            keycode,
            state,
            SERIAL_COUNTER.next_serial(),
            time,
            |app, mods, handle| {
                app.modifiers = *mods;
                let sym = handle.modified_sym();

                // 0. While locked, swallow every key event so no client can read typing;
                //    route key presses to the PAM lock screen instead.
                if app.is_locked() {
                    app.mod_tap_candidate = false;
                    if state == KeyState::Pressed {
                        if let Some(lock_key) = keysym_to_lock_key(sym, mods) {
                            if let Some(ref mut lock) = app.lock_screen {
                                lock.handle_key(lock_key);
                            }
                        }
                    }
                    return FilterResult::Intercept(None);
                }

                // 1. Solo-Mod tap detection. The Mod release is still forwarded so the
                //    focused client never believes Super/Alt is stuck down.
                if is_mod_keysym(sym) {
                    match state {
                        KeyState::Pressed => {
                            app.mod_tap_candidate = true;
                        }
                        KeyState::Released => {
                            solo_mod_tap = std::mem::take(&mut app.mod_tap_candidate);
                        }
                    }
                    return FilterResult::Forward;
                }

                // Any non-Mod key press cancels the solo-Mod tap candidate.
                if state == KeyState::Pressed {
                    app.mod_tap_candidate = false;
                    // A fresh press decides again whether this key's release is swallowed.
                    app.intercepted_keys.retain(|k| *k != keycode);
                } else {
                    // Swallow the release of keys whose press was a compositor binding.
                    if let Some(pos) = app.intercepted_keys.iter().position(|k| *k == keycode) {
                        app.intercepted_keys.swap_remove(pos);
                        return FilterResult::Intercept(None);
                    }
                    return FilterResult::Forward;
                }

                // Pressing `Escape` while the keybinding cheat sheet is open dismisses it.
                if app.help.visible && sym.raw() == keysyms::KEY_Escape && !is_mod_active(mods) {
                    return FilterResult::Intercept(Some(KeyAction::ToggleHelp));
                }

                // 2. Global hardware media / brightness / screenshot / lock keys (work without Mod)
                match sym.raw() {
                    keysyms::KEY_XF86AudioRaiseVolume => {
                        return FilterResult::Intercept(Some(KeyAction::VolumeUp));
                    }
                    keysyms::KEY_XF86AudioLowerVolume => {
                        return FilterResult::Intercept(Some(KeyAction::VolumeDown));
                    }
                    keysyms::KEY_XF86AudioMute => {
                        return FilterResult::Intercept(Some(KeyAction::VolumeMute));
                    }
                    keysyms::KEY_XF86AudioMicMute => {
                        return FilterResult::Intercept(Some(KeyAction::MicMute));
                    }
                    keysyms::KEY_XF86MonBrightnessUp => {
                        return FilterResult::Intercept(Some(KeyAction::BrightnessUp));
                    }
                    keysyms::KEY_XF86MonBrightnessDown => {
                        return FilterResult::Intercept(Some(KeyAction::BrightnessDown));
                    }
                    keysyms::KEY_XF86AudioPlay | keysyms::KEY_XF86AudioPause => {
                        return FilterResult::Intercept(Some(KeyAction::MediaPlay));
                    }
                    keysyms::KEY_XF86AudioNext => {
                        return FilterResult::Intercept(Some(KeyAction::MediaNext));
                    }
                    keysyms::KEY_XF86AudioPrev => {
                        return FilterResult::Intercept(Some(KeyAction::MediaPrev));
                    }
                    keysyms::KEY_XF86ScreenSaver => {
                        return FilterResult::Intercept(Some(KeyAction::LockScreen));
                    }
                    keysyms::KEY_Print => {
                        let act = if mods.shift || is_mod_active(mods) {
                            KeyAction::ScreenshotWindow
                        } else {
                            KeyAction::Screenshot
                        };
                        return FilterResult::Intercept(Some(act));
                    }
                    _ => {}
                }

                // 3. Check if Mod is held for compositor shortcuts.
                if !is_mod_active(mods) {
                    return FilterResult::Forward;
                }

                // Tag shortcuts (`Mod + [Ctrl] + [Shift] + 1..9`)
                if let Some(tag_idx) = keysym_to_tag_index(sym) {
                    let mask = 1u16 << tag_idx;
                    let act = match (mods.ctrl, mods.shift) {
                        (false, false) => KeyAction::ViewTags(mask),
                        (true, false) => KeyAction::ToggleViewTags(mask),
                        (false, true) => KeyAction::TagWindow(mask),
                        (true, true) => KeyAction::ToggleTagWindow(mask),
                    };
                    return FilterResult::Intercept(Some(act));
                }

                // `Mod + 0` / `Mod + Shift + 0` -> view all tags / sticky window on all tags
                if matches!(sym.raw(), keysyms::KEY_0 | keysyms::KEY_parenright) {
                    let act = if mods.ctrl {
                        KeyAction::ResetGaps
                    } else if mods.shift {
                        KeyAction::TagWindow(TAGMASK)
                    } else {
                        KeyAction::ViewTags(TAGMASK)
                    };
                    return FilterResult::Intercept(Some(act));
                }

                // Arrow keys: `Mod + Arrow` moves window, `Mod + Shift + Arrow` resizes window
                match sym.raw() {
                    keysyms::KEY_Left => {
                        let act = if mods.shift {
                            KeyAction::ResizeWindow(-1, 0)
                        } else {
                            KeyAction::MoveWindow(-1, 0)
                        };
                        return FilterResult::Intercept(Some(act));
                    }
                    keysyms::KEY_Right => {
                        let act = if mods.shift {
                            KeyAction::ResizeWindow(1, 0)
                        } else {
                            KeyAction::MoveWindow(1, 0)
                        };
                        return FilterResult::Intercept(Some(act));
                    }
                    keysyms::KEY_Up => {
                        let act = if mods.shift {
                            KeyAction::ResizeWindow(0, -1)
                        } else {
                            KeyAction::MoveWindow(0, -1)
                        };
                        return FilterResult::Intercept(Some(act));
                    }
                    keysyms::KEY_Down => {
                        let act = if mods.shift {
                            KeyAction::ResizeWindow(0, 1)
                        } else {
                            KeyAction::MoveWindow(0, 1)
                        };
                        return FilterResult::Intercept(Some(act));
                    }
                    _ => {}
                }

                // Standard dwm + daily-driver bindings
                let maybe_act = match sym.raw() {
                    keysyms::KEY_Return | keysyms::KEY_KP_Enter => {
                        if mods.shift {
                            Some(KeyAction::Zoom)
                        } else {
                            Some(KeyAction::SpawnTerminal)
                        }
                    }
                    keysyms::KEY_Escape => Some(KeyAction::SpawnSystemMonitor),
                    keysyms::KEY_p | keysyms::KEY_P => Some(KeyAction::SpawnLauncher),
                    keysyms::KEY_w | keysyms::KEY_W => {
                        if mods.shift {
                            Some(KeyAction::ToggleWifi)
                        } else {
                            Some(KeyAction::SpawnWifi)
                        }
                    }
                    keysyms::KEY_s | keysyms::KEY_S | keysyms::KEY_grave => {
                        Some(KeyAction::ToggleScratchpad)
                    }
                    keysyms::KEY_j | keysyms::KEY_J => Some(KeyAction::FocusDelta(1)),
                    keysyms::KEY_k | keysyms::KEY_K => Some(KeyAction::FocusDelta(-1)),
                    keysyms::KEY_h | keysyms::KEY_H => Some(KeyAction::ToggleHelp),
                    keysyms::KEY_bracketleft => Some(KeyAction::AdjustMfact(-0.05)),
                    keysyms::KEY_bracketright => Some(KeyAction::AdjustMfact(0.05)),
                    keysyms::KEY_l | keysyms::KEY_L => {
                        if mods.shift || mods.ctrl {
                            Some(KeyAction::LockScreen)
                        } else {
                            Some(KeyAction::AdjustMfact(0.05))
                        }
                    }
                    keysyms::KEY_i | keysyms::KEY_I => Some(KeyAction::AdjustNmaster(1)),
                    keysyms::KEY_d | keysyms::KEY_D => Some(KeyAction::AdjustNmaster(-1)),
                    keysyms::KEY_t | keysyms::KEY_T => Some(KeyAction::SetLayout(LayoutMode::Tile)),
                    keysyms::KEY_m | keysyms::KEY_M => {
                        Some(KeyAction::SetLayout(LayoutMode::Monocle))
                    }
                    keysyms::KEY_u | keysyms::KEY_U => {
                        Some(KeyAction::SetLayout(LayoutMode::CenteredMaster))
                    }
                    keysyms::KEY_g | keysyms::KEY_G => {
                        if mods.shift {
                            Some(KeyAction::ToggleGaps)
                        } else {
                            Some(KeyAction::SetLayout(LayoutMode::Grid))
                        }
                    }
                    keysyms::KEY_f | keysyms::KEY_F => {
                        if mods.shift {
                            Some(KeyAction::ToggleFullscreen)
                        } else {
                            Some(KeyAction::SetLayout(LayoutMode::Floating))
                        }
                    }
                    keysyms::KEY_F11 => Some(KeyAction::ToggleFullscreen),
                    keysyms::KEY_o | keysyms::KEY_O => Some(KeyAction::ToggleOpacity),
                    keysyms::KEY_equal | keysyms::KEY_plus if mods.ctrl => {
                        Some(KeyAction::AdjustScale(SCALE_STEP))
                    }
                    keysyms::KEY_minus | keysyms::KEY_underscore if mods.ctrl => {
                        Some(KeyAction::AdjustScale(-SCALE_STEP))
                    }
                    keysyms::KEY_equal | keysyms::KEY_plus => Some(KeyAction::AdjustGaps(2)),
                    keysyms::KEY_minus | keysyms::KEY_underscore => Some(KeyAction::AdjustGaps(-2)),
                    keysyms::KEY_space => {
                        if mods.shift {
                            Some(KeyAction::ToggleFloating)
                        } else if mods.ctrl {
                            Some(KeyAction::CycleLayout)
                        } else {
                            Some(KeyAction::ToggleLastLayout)
                        }
                    }
                    keysyms::KEY_b | keysyms::KEY_B => {
                        if mods.shift {
                            Some(KeyAction::SpawnBluetooth)
                        } else if mods.ctrl {
                            Some(KeyAction::ToggleBluetooth)
                        } else {
                            Some(KeyAction::ToggleBar)
                        }
                    }
                    keysyms::KEY_Tab => Some(KeyAction::SwapPrevTags),
                    keysyms::KEY_q | keysyms::KEY_Q => {
                        if mods.shift {
                            Some(KeyAction::Quit)
                        } else {
                            Some(KeyAction::CloseWindow)
                        }
                    }
                    _ => None,
                };

                if let Some(act) = maybe_act {
                    FilterResult::Intercept(Some(act))
                } else {
                    FilterResult::Forward
                }
            },
        );

        // While locked every key is swallowed above, so there is nothing to
        // remember; recording repeats would leave stale keycodes after unlock.
        if state == KeyState::Pressed && intercepted.is_some() && !self.is_locked() {
            if !self.intercepted_keys.contains(&keycode) {
                self.intercepted_keys.push(keycode);
            }
        }
        if let Some(Some(act)) = intercepted {
            self.execute_key_action(act);
        }
        if solo_mod_tap {
            self.spawn_launcher();
        }
    }

    pub fn execute_key_action(&mut self, action: KeyAction) {
        match action {
            KeyAction::ToggleHelp => {
                self.help.toggle();
                self.sync_pointer_focus(0);
            }
            KeyAction::SpawnLauncher => self.spawn_launcher(),
            KeyAction::SpawnTerminal => self.spawn_terminal(),
            KeyAction::SpawnSystemMonitor => self.spawn_system_monitor(),
            KeyAction::SpawnWifi => self.spawn_wifi_manager(),
            KeyAction::ToggleWifi => self.toggle_wifi_radio(),
            KeyAction::SpawnBluetooth => self.spawn_bluetooth_manager(),
            KeyAction::ToggleBluetooth => self.toggle_bluetooth_power(),
            KeyAction::ToggleScratchpad => self.toggle_scratchpad(),
            KeyAction::LockScreen => self.lock_session(),
            KeyAction::CloseWindow => self.close_focused(),
            KeyAction::Quit => self.should_quit = true,
            KeyAction::FocusDelta(delta) => self.cycle_focus(delta),
            KeyAction::MoveWindow(dx, dy) => self.move_focused_window(dx, dy),
            KeyAction::ResizeWindow(dw, dh) => self.resize_focused_window(dw, dh),
            KeyAction::AdjustMfact(delta) => {
                self.workspace.adjust_mfact(delta);
                self.relayout();
            }
            KeyAction::AdjustNmaster(delta) => {
                self.workspace.adjust_nmaster(delta);
                self.relayout();
            }
            KeyAction::AdjustGaps(delta) => {
                self.workspace.adjust_gaps(delta);
                self.relayout();
            }
            KeyAction::AdjustScale(delta) => self.adjust_scale(delta),
            KeyAction::ResetGaps => {
                self.workspace.reset_gaps();
                self.relayout();
            }
            KeyAction::ToggleGaps => {
                self.workspace.toggle_gaps();
                self.relayout();
            }
            KeyAction::Zoom => self.zoom_focused(),
            KeyAction::SetLayout(mode) => self.set_layout(mode),
            KeyAction::CycleLayout => {
                let next = self.workspace.layout().next();
                self.set_layout(next);
            }
            KeyAction::ToggleLastLayout => {
                let prev = self.workspace.prev_layout();
                self.set_layout(prev);
            }
            KeyAction::ToggleFloating => self.toggle_floating_focused(),
            KeyAction::ToggleFullscreen => self.toggle_fullscreen_focused(),
            KeyAction::ToggleOpacity => self.toggle_opacity_focused(),
            KeyAction::ToggleBar => {
                self.workspace.show_bar = !self.workspace.show_bar;
                self.relayout();
            }
            KeyAction::ViewTags(mask) => self.view_tags(mask),
            KeyAction::ToggleViewTags(mask) => self.toggle_view_tags(mask),
            KeyAction::TagWindow(mask) => self.tag_focused_window(mask),
            KeyAction::ToggleTagWindow(mask) => self.toggle_tag_focused_window(mask),
            KeyAction::SwapPrevTags => {
                let prev = self.workspace.prev_tags;
                self.view_tags(prev);
            }
            KeyAction::VolumeUp => self.spawn_volume_up(),
            KeyAction::VolumeDown => self.spawn_volume_down(),
            KeyAction::VolumeMute => self.spawn_volume_mute(),
            KeyAction::MicMute => self.spawn_mic_mute(),
            KeyAction::BrightnessUp => self.spawn_brightness_up(),
            KeyAction::BrightnessDown => self.spawn_brightness_down(),
            KeyAction::MediaPlay => self.spawn_media_play(),
            KeyAction::MediaNext => self.spawn_media_next(),
            KeyAction::MediaPrev => self.spawn_media_prev(),
            KeyAction::Screenshot => self.request_screenshot(false),
            KeyAction::ScreenshotWindow => self.request_screenshot(true),
        }
    }

    /// Apply an active interactive window move/resize grab for the given pointer/touch location.
    fn update_interactive_grab(&mut self, position: Point<f64, Logical>) -> bool {
        let Some(grab) = self.active_grab.clone() else {
            return false;
        };
        let (surface, start_pointer, start_geo, edges) = match grab {
            InteractiveGrab::Move {
                surface,
                start_pointer,
                start_geo,
                ..
            } => (surface, start_pointer, start_geo, None),
            InteractiveGrab::Resize {
                surface,
                start_pointer,
                start_geo,
                edges,
                ..
            } => (surface, start_pointer, start_geo, Some(edges)),
        };
        let Some(w) = self.windows.iter_mut().find(|w| w.wl_surface() == &surface) else {
            self.active_grab = None;
            self.touch_grab_slot = None;
            return true;
        };
        let dx = (position.x - start_pointer.x).round() as i32;
        let dy = (position.y - start_pointer.y).round() as i32;
        if let Some(edges) = edges {
            // The user picked a size now; keep it within the app's min/max size hints.
            w.size_from_client = false;
            let wanted = crate::state::resize_rect(start_geo, dx, dy, edges, 120, 80);
            let clamped = w.clamp_frame_to_client_limits(wanted.size);
            w.float_geo.loc = wanted.loc;
            if edges.left {
                w.float_geo.loc.x = start_geo.loc.x + start_geo.size.w - clamped.w;
            }
            if edges.top {
                w.float_geo.loc.y = start_geo.loc.y + start_geo.size.h - clamped.h;
            }
            w.float_geo.size = clamped;
        } else {
            w.float_geo.loc.x = start_geo.loc.x + dx;
            w.float_geo.loc.y = start_geo.loc.y + dy;
        }
        w.geo = w.float_geo;
        self.relayout();
        true
    }

    /// Execute a top status bar click/tap action at logical `x` with `button`.
    fn handle_bar_click(&mut self, x: i32, button: u32) {
        match self.bar.hit_test(x) {
            BarClickAction::Tag(idx) => {
                let mask = 1u16 << idx;
                if button == BTN_RIGHT || self.modifiers.ctrl {
                    self.toggle_view_tags(mask);
                } else if button == BTN_MIDDLE || self.modifiers.shift {
                    self.tag_focused_window(mask);
                } else {
                    self.view_tags(mask);
                }
            }
            BarClickAction::CycleLayout => {
                if button == BTN_RIGHT {
                    let prev = self.workspace.prev_layout();
                    self.set_layout(prev);
                } else {
                    let next = self.workspace.layout().next();
                    self.set_layout(next);
                }
            }
            BarClickAction::Wifi => {
                if button == BTN_RIGHT {
                    self.toggle_wifi_radio();
                } else {
                    self.spawn_wifi_manager();
                }
            }
            BarClickAction::Bluetooth => {
                if button == BTN_RIGHT {
                    self.toggle_bluetooth_power();
                } else {
                    self.spawn_bluetooth_manager();
                }
            }
            BarClickAction::Volume => {
                if button == BTN_RIGHT {
                    self.spawn_mixer();
                } else {
                    self.spawn_volume_mute();
                    self.bar.request_immediate_status_refresh();
                }
            }
            BarClickAction::SystemMonitor => {
                self.spawn_system_monitor();
            }
            BarClickAction::FocusNext => {
                if button == BTN_MIDDLE {
                    self.spawn_terminal();
                } else {
                    self.cycle_focus(1);
                }
            }
        }
    }

    /// Handle relative pointer motion (used by `--tty-udev` libinput mouse/touchpad movement).
    pub fn process_pointer_move_relative(
        &mut self,
        delta: Point<f64, Logical>,
        delta_unaccel: Point<f64, Logical>,
        time: u32,
    ) {
        let new_x = (self.pointer_location.x + delta.x).clamp(0.0, (self.output_size.w - 1) as f64);
        let new_y = (self.pointer_location.y + delta.y).clamp(0.0, (self.output_size.h - 1) as f64);
        let new_pos = Point::from((new_x, new_y));

        if self.is_locked() {
            self.pointer_location = new_pos;
            return;
        }

        let under = self.surface_under(new_pos);

        let pointer = self.pointer.clone();
        pointer.relative_motion(
            self,
            under,
            &RelativeMotionEvent {
                delta,
                delta_unaccel,
                utime: (time as u64) * 1000,
            },
        );
        self.process_pointer_motion(new_pos, time);
    }

    /// Handle pointer motion in logical coordinates (layer-shell overlays first, then sloppy focus-follows-mouse + interactive move/resize).
    pub fn process_pointer_motion(&mut self, position: Point<f64, Logical>, time: u32) {
        self.pointer_location = position;
        if self.is_locked() {
            return;
        }

        if self.touch_grab_slot.is_none() && self.update_interactive_grab(position) {
            return;
        }

        // Sloppy focus with dwm "enter" semantics: focus changes only when the pointer
        // crosses into a different window, so a freshly opened window keeps focus until
        // the mouse actually moves onto something else. Menus (popups) and button drags
        // never move focus, otherwise a menu hanging over a neighbour would close.
        if self.layer_surface_at(position).is_none()
            && !self.pointer.is_grabbed()
            && !self.popup_at(position)
        {
            let hovered = self.window_at(position).map(|(idx, _)| idx);
            let hovered_surface = hovered.map(|idx| self.windows[idx].wl_surface().clone());
            if hovered_surface != self.hovered_surface {
                self.hovered_surface = hovered_surface;
                if let Some(idx) = hovered {
                    if self.focused_idx != Some(idx) {
                        self.focused_idx = Some(idx);
                        self.relayout();
                    }
                }
            }
        }

        let under = self.surface_under(position);
        let pointer = self.pointer.clone();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: position,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
    }

    /// Handle mouse wheel / touchpad two-finger scroll (`InputEvent::PointerAxis`).
    pub fn process_pointer_axis<B: InputBackend>(&mut self, event: B::PointerAxisEvent) {
        if self.is_locked() {
            return;
        }
        let pos = self.pointer_location;
        // Mouse wheels only report discrete v120 steps; clients still need a continuous
        // value (15 per notch, like libinput) or many of them ignore the scroll entirely.
        let continuous = |axis| {
            event
                .amount(axis)
                .unwrap_or_else(|| event.amount_v120(axis).unwrap_or(0.0) * 15.0 / 120.0)
        };

        // Scrolling on the top status bar switches workspace tags (or volume over the VOL pill).
        if self.bar_visible()
            && pos.y >= 0.0
            && pos.y < BAR_HEIGHT as f64
            && self.layer_surface_at(pos).is_none()
        {
            self.bar_scroll_accum += continuous(Axis::Vertical);
            while self.bar_scroll_accum.abs() >= BAR_SCROLL_STEP {
                let dir: isize = if self.bar_scroll_accum > 0.0 { 1 } else { -1 };
                self.bar_scroll_accum -= dir as f64 * BAR_SCROLL_STEP;
                if self.bar.hit_test(pos.x as i32) == BarClickAction::Volume {
                    if dir > 0 {
                        self.spawn_volume_down();
                    } else {
                        self.spawn_volume_up();
                    }
                    self.bar.request_immediate_status_refresh();
                } else {
                    let ntags = TAG_NAMES.len() as isize;
                    let next_tag =
                        (self.workspace.cur_tag as isize + dir).rem_euclid(ntags) as usize;
                    self.view_tags(1u16 << next_tag);
                }
            }
            return;
        }
        self.bar_scroll_accum = 0.0;

        let time = Event::time_msec(&event);
        if !self.pointer.is_grabbed() {
            self.sync_pointer_focus(time);
        }

        let mut frame = AxisFrame::new(time).source(event.source());
        for axis in [Axis::Horizontal, Axis::Vertical] {
            let amount = continuous(axis);
            if amount != 0.0 {
                frame = frame
                    .relative_direction(axis, event.relative_direction(axis))
                    .value(axis, amount);
                if let Some(v120) = event.amount_v120(axis) {
                    frame = frame.v120(axis, v120 as i32);
                }
            } else if event.amount(axis) == Some(0.0)
                && matches!(event.source(), AxisSource::Finger | AxisSource::Continuous)
            {
                frame = frame.stop(axis);
            }
        }

        let pointer = self.pointer.clone();
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// Handle pointer button presses (layer-shell overlays, bar clicks, Mod+Drag move/resize, and client click forwarding).
    pub fn process_pointer_button(&mut self, button: u32, state: ButtonState, time: u32) {
        if state == ButtonState::Pressed {
            self.mod_tap_candidate = false;
        }
        if self.is_locked() {
            return;
        }
        if state == ButtonState::Pressed && self.help.visible {
            self.help.visible = false;
            self.sync_pointer_focus(time);
            return;
        }

        if state == ButtonState::Released && self.touch_grab_slot.is_none() {
            if let Some(grab) = self.active_grab.take() {
                let forward = grab.forward_release();
                self.relayout();
                // Client-initiated move/resize already delivered the button
                // press; dropping the release leaves the client stuck down.
                if forward {
                    self.forward_pointer_button(button, state, time);
                }
                return;
            }
        }

        let pos = self.pointer_location;

        // Dismiss a notification toast if clicked
        if state == ButtonState::Pressed
            && self
                .notifications
                .dismiss_at(pos, self.output_size.w, self.bar_visible())
        {
            self.sync_pointer_focus(time);
            return;
        }

        // 0. If clicking on a layer-shell overlay (Spotlight), forward directly to it
        if let Some((layer_surface, _)) = self.layer_surface_at(pos) {
            if state == ButtonState::Pressed {
                let on_demand = with_states(&layer_surface, |states| {
                    states
                        .cached_state
                        .get::<LayerSurfaceCachedState>()
                        .current()
                        .keyboard_interactivity
                        == KeyboardInteractivity::OnDemand
                });
                if on_demand {
                    self.layer_focus = Some(layer_surface);
                    self.sync_keyboard_focus();
                }
            }
            self.forward_pointer_button(button, state, time);
            return;
        }

        // Menus sit above everything: clicks on them go straight to the client.
        if self.popup_at(pos) {
            self.forward_pointer_button(button, state, time);
            return;
        }

        // 1. Check if clicking on the top status bar
        if state == ButtonState::Pressed
            && self.bar_visible()
            && pos.y >= 0.0
            && pos.y < BAR_HEIGHT as f64
        {
            self.handle_bar_click(pos.x as i32, button);
            return;
        }

        // 2. Check for `Mod + Left/Right/Middle Click` interactive window move/resize/toggle-float
        if state == ButtonState::Pressed && is_mod_active(&self.modifiers) {
            if let Some((idx, _)) = self.window_at(pos) {
                self.focused_idx = Some(idx);
                if button == BTN_MIDDLE {
                    self.toggle_floating_focused();
                    return;
                }
                let w = &mut self.windows[idx];
                if !w.is_floating && !w.is_scratchpad {
                    w.is_floating = true;
                    w.float_geo = w.geo;
                }
                let start_geo = w.float_geo;
                let surface = w.wl_surface().clone();
                if button == BTN_LEFT {
                    self.touch_grab_slot = None;
                    self.active_grab = Some(InteractiveGrab::Move {
                        surface,
                        start_pointer: pos,
                        start_geo,
                        forward_release: false,
                    });
                    self.relayout();
                    return;
                } else if button == BTN_RIGHT {
                    self.touch_grab_slot = None;
                    self.active_grab = Some(InteractiveGrab::Resize {
                        surface,
                        start_pointer: pos,
                        start_geo,
                        edges: crate::state::ResizeEdges::BOTTOM_RIGHT,
                        forward_release: false,
                    });
                    self.relayout();
                    return;
                }
            }
        }

        // 3. Focus clicked window & forward button event to the Wayland client
        if state == ButtonState::Pressed {
            self.layer_focus = None;
            if let Some((idx, _)) = self.window_at(pos) {
                if self.focused_idx != Some(idx) {
                    self.focused_idx = Some(idx);
                    self.relayout();
                }
            }
        }

        self.forward_pointer_button(button, state, time);
    }

    fn forward_pointer_button(&mut self, button: u32, state: ButtonState, time: u32) {
        // Ensure `self.pointer` has up-to-date focus and surface-local coordinates before
        // delivering a button press (e.g. if a window/popup appeared or moved under a
        // stationary cursor, or if clicking an unfocused window just triggered `relayout()`).
        if state == ButtonState::Pressed || !self.pointer.is_grabbed() {
            self.sync_pointer_focus(time);
        }
        let pointer = self.pointer.clone();
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state,
                serial: SERIAL_COUNTER.next_serial(),
                time,
            },
        );
        pointer.frame(self);
        // A click outside an open menu ends its grab; hand keyboard and pointer focus
        // back to the window now under the pointer.
        if state == ButtonState::Pressed {
            self.sync_keyboard_focus();
        }
        if !self.pointer.is_grabbed() {
            self.sync_pointer_focus(time);
        }
    }

    // ========================================================================
    // NATIVE TOUCH INPUT (`wl_touch`)
    // ========================================================================

    /// Handle a touch-down event in logical coordinates: notification dismissal, layer overlays,
    /// popups, top status bar taps, `Mod + Touch` interactive window dragging, window focus, and
    /// `wl_touch::down` forwarding to Wayland clients.
    pub fn process_touch_down(&mut self, slot: TouchSlot, pos: Point<f64, Logical>, time: u32) {
        self.mod_tap_candidate = false;
        self.touch_locations.insert(slot, pos);
        self.pointer_location = pos;

        if self.is_locked() {
            return;
        }
        if self.help.visible {
            self.help.visible = false;
            self.sync_pointer_focus(time);
            return;
        }

        if self
            .notifications
            .dismiss_at(pos, self.output_size.w, self.bar_visible())
        {
            self.sync_pointer_focus(time);
            return;
        }

        // 0. Layer-shell overlay (Spotlight)
        if let Some((layer_surface, _)) = self.layer_surface_at(pos) {
            let on_demand = with_states(&layer_surface, |states| {
                states
                    .cached_state
                    .get::<LayerSurfaceCachedState>()
                    .current()
                    .keyboard_interactivity
                    == KeyboardInteractivity::OnDemand
            });
            if on_demand {
                self.layer_focus = Some(layer_surface);
                self.sync_keyboard_focus();
            }
            let under = self.surface_under(pos);
            let serial = SERIAL_COUNTER.next_serial();
            let touch = self.touch.clone();
            touch.down(
                self,
                under,
                &TouchDown {
                    slot,
                    location: pos,
                    serial,
                    time,
                },
            );
            touch.frame(self);
            return;
        }

        // Popups sit above windows and the bar
        if self.popup_at(pos) {
            let under = self.surface_under(pos);
            let serial = SERIAL_COUNTER.next_serial();
            let touch = self.touch.clone();
            touch.down(
                self,
                under,
                &TouchDown {
                    slot,
                    location: pos,
                    serial,
                    time,
                },
            );
            touch.frame(self);
            return;
        }

        // 1. Tapping on the top status bar activates tags, layouts, Wi-Fi, BT, volume, or monitor
        if self.bar_visible() && pos.y >= 0.0 && pos.y < BAR_HEIGHT as f64 {
            self.handle_bar_click(pos.x as i32, BTN_LEFT);
            return;
        }

        // 2. `Mod + Touch` interactive window move
        if is_mod_active(&self.modifiers) {
            if let Some((idx, _)) = self.window_at(pos) {
                self.focused_idx = Some(idx);
                let w = &mut self.windows[idx];
                if !w.is_floating && !w.is_scratchpad {
                    w.is_floating = true;
                    w.float_geo = w.geo;
                }
                let start_geo = w.float_geo;
                let surface = w.wl_surface().clone();
                self.touch_grab_slot = Some(slot);
                self.active_grab = Some(InteractiveGrab::Move {
                    surface,
                    start_pointer: pos,
                    start_geo,
                    forward_release: false,
                });
                self.relayout();
                return;
            }
        }

        // 3. Focus touched window and forward `wl_touch::down` to the client surface
        self.layer_focus = None;
        if let Some((idx, _)) = self.window_at(pos) {
            if self.focused_idx != Some(idx) {
                self.focused_idx = Some(idx);
                self.relayout();
            }
        }

        let under = self.surface_under(pos);
        let serial = SERIAL_COUNTER.next_serial();
        let touch = self.touch.clone();
        touch.down(
            self,
            under,
            &TouchDown {
                slot,
                location: pos,
                serial,
                time,
            },
        );
        touch.frame(self);
        self.sync_keyboard_focus();
    }

    /// Handle touch point motion in logical coordinates.
    pub fn process_touch_motion(&mut self, slot: TouchSlot, pos: Point<f64, Logical>, time: u32) {
        self.touch_locations.insert(slot, pos);
        self.pointer_location = pos;

        if self.is_locked() {
            return;
        }

        if self.touch_grab_slot == Some(slot) && self.update_interactive_grab(pos) {
            return;
        }

        let under = self.surface_under(pos);
        let touch = self.touch.clone();
        touch.motion(
            self,
            under,
            &TouchMotion {
                slot,
                location: pos,
                time,
            },
        );
        touch.frame(self);
    }

    /// Handle touch point release (`wl_touch::up`).
    pub fn process_touch_up(&mut self, slot: TouchSlot, time: u32) {
        self.touch_locations.remove(&slot);

        if self.touch_grab_slot == Some(slot) {
            self.touch_grab_slot = None;
            if let Some(grab) = self.active_grab.take() {
                let forward = grab.forward_release();
                self.relayout();
                if !forward {
                    return;
                }
            }
        }

        if self.is_locked() {
            return;
        }

        let serial = SERIAL_COUNTER.next_serial();
        let touch = self.touch.clone();
        touch.up(self, &TouchUp { slot, serial, time });
        touch.frame(self);
    }

    /// Handle touch session cancellation (`wl_touch::cancel`).
    pub fn process_touch_cancel(&mut self, slot: TouchSlot) {
        self.touch_locations.remove(&slot);
        if self.touch_grab_slot == Some(slot) {
            self.touch_grab_slot = None;
            self.active_grab = None;
        }
        let touch = self.touch.clone();
        touch.cancel(self);
    }

    /// Handle explicit touch frame boundary (`wl_touch::frame`).
    pub fn process_touch_frame(&mut self) {
        if self.is_locked() {
            return;
        }
        let touch = self.touch.clone();
        touch.frame(self);
    }
}
