//! Keyboard and pointer input handling for `purpwm`, including:
//! - Solo `Mod` tap detection (press and release `Mod` alone to launch `/home/mag/Documents/Spotlight`)
//! - `Mod + Arrow` keys to move windows around (and `Mod + Shift + Arrow` to resize)
//! - Full `dwm` keybindings (`Mod + 1..9`, `Mod + j/k`, `Mod + h/l`, `Mod + Return`, etc.)
//! - Wi-Fi (`Mod + w`, `Mod + Shift + w`) and Bluetooth (`Mod + Shift + b`) controls + clickable bar pills
//! - System monitor (`Mod + Escape` or clicking CPU/MEM/BAT pill)
//! - Additional layouts (`Mod + u` CenteredMaster `|M|`, `Mod + g` Grid `###`)
//! - Scratchpad (`Mod + s`), Fullscreen (`Mod + Shift + f` / `F11`), Opaque toggle (`Mod + o`), Vanity Gaps (`Mod + =/-`)
//! - Hardware media/volume/brightness/screenshot keys (`XF86Audio*`, `XF86MonBrightness*`, `Print`)
//! - Mouse scroll-wheel (`PointerAxis`) forwarding and status bar wheel tag switching

use smithay::{
    backend::input::{
        Axis, AxisSource, ButtonState, Event, InputBackend, KeyState, Keycode, PointerAxisEvent,
    },
    input::{
        keyboard::{keysyms, FilterResult, Keysym, ModifiersState},
        pointer::{AxisFrame, ButtonEvent, MotionEvent, RelativeMotionEvent},
    },
    utils::{Logical, Point, Serial},
};

use crate::{
    bar::BarClickAction,
    config::{BAR_HEIGHT, TAGMASK, TAG_NAMES},
    state::{InteractiveGrab, PurpState},
    wm::LayoutMode,
};

/// Compositor action triggered by a keybinding or solo-Mod release.
#[derive(Debug, Clone, Copy)]
pub enum KeyAction {
    SpawnLauncher,
    SpawnTerminal,
    SpawnSystemMonitor,
    SpawnWifi,
    ToggleWifi,
    SpawnBluetooth,
    ToggleBluetooth,
    ToggleScratchpad,
    CloseWindow,
    Quit,
    FocusDelta(isize),
    MoveWindow(i32, i32),
    ResizeWindow(i32, i32),
    AdjustMfact(f32),
    AdjustNmaster(isize),
    AdjustGaps(i32),
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
fn is_mod_active(mods: &ModifiersState, is_nested: bool) -> bool {
    if is_nested {
        mods.alt || mods.logo
    } else {
        mods.logo || mods.alt
    }
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

impl PurpState {
    /// Process a physical key press/release event through the Wayland seat keyboard.
    pub fn process_keyboard_event(&mut self, keycode: Keycode, state: KeyState, time: u32) {
        let keyboard = self.keyboard.clone();
        let is_nested = self.is_nested;

        let action: Option<KeyAction> = keyboard
            .input(
                self,
                keycode,
                state,
                Serial::from(0),
                time,
                |app, mods, handle| {
                    app.modifiers = *mods;
                    let sym = handle.modified_sym();

                    // 1. Solo-Mod tap detection:
                    if is_mod_keysym(sym) {
                        match state {
                            KeyState::Pressed => {
                                app.mod_tap_candidate = true;
                            }
                            KeyState::Released => {
                                if app.mod_tap_candidate {
                                    app.mod_tap_candidate = false;
                                    return FilterResult::Intercept(Some(KeyAction::SpawnLauncher));
                                }
                            }
                        }
                        return FilterResult::Forward;
                    }

                    // Any non-Mod key press cancels the solo-Mod tap candidate.
                    if state == KeyState::Pressed {
                        app.mod_tap_candidate = false;
                    } else {
                        return FilterResult::Forward;
                    }

                    // 2. Global hardware media / brightness / screenshot keys (work without Mod)
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
                        keysyms::KEY_Print => {
                            return FilterResult::Intercept(Some(KeyAction::Screenshot));
                        }
                        _ => {}
                    }

                    // 3. Check if Mod is held for compositor shortcuts.
                    if !is_mod_active(mods, is_nested) {
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
                        keysyms::KEY_h | keysyms::KEY_H => Some(KeyAction::AdjustMfact(-0.05)),
                        keysyms::KEY_l | keysyms::KEY_L => Some(KeyAction::AdjustMfact(0.05)),
                        keysyms::KEY_i | keysyms::KEY_I => Some(KeyAction::AdjustNmaster(1)),
                        keysyms::KEY_d | keysyms::KEY_D => Some(KeyAction::AdjustNmaster(-1)),
                        keysyms::KEY_t | keysyms::KEY_T => {
                            Some(KeyAction::SetLayout(LayoutMode::Tile))
                        }
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
                        keysyms::KEY_equal | keysyms::KEY_plus => Some(KeyAction::AdjustGaps(2)),
                        keysyms::KEY_minus | keysyms::KEY_underscore => {
                            Some(KeyAction::AdjustGaps(-2))
                        }
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
                        keysyms::KEY_c | keysyms::KEY_C if mods.shift => {
                            Some(KeyAction::CloseWindow)
                        }
                        keysyms::KEY_q | keysyms::KEY_Q if mods.shift => Some(KeyAction::Quit),
                        _ => None,
                    };

                    if let Some(act) = maybe_act {
                        FilterResult::Intercept(Some(act))
                    } else {
                        FilterResult::Forward
                    }
                },
            )
            .flatten();

        if let Some(act) = action {
            self.execute_key_action(act);
        }
    }

    pub fn execute_key_action(&mut self, action: KeyAction) {
        match action {
            KeyAction::SpawnLauncher => self.spawn_launcher(),
            KeyAction::SpawnTerminal => self.spawn_terminal(),
            KeyAction::SpawnSystemMonitor => self.spawn_system_monitor(),
            KeyAction::SpawnWifi => self.spawn_wifi_manager(),
            KeyAction::ToggleWifi => self.toggle_wifi_radio(),
            KeyAction::SpawnBluetooth => self.spawn_bluetooth_manager(),
            KeyAction::ToggleBluetooth => self.toggle_bluetooth_power(),
            KeyAction::ToggleScratchpad => self.toggle_scratchpad(),
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
            KeyAction::Screenshot => self.spawn_screenshot(),
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

        let under = if let Some((surf, loc)) = self.layer_surface_at(new_pos) {
            Some((surf, loc.to_f64()))
        } else {
            self.window_at(new_pos).map(|(idx, surface_loc)| {
                let surf = self.windows[idx].wl_surface().clone();
                (surf, surface_loc.to_f64())
            })
        };

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

        if let Some(grab) = self.active_grab {
            match grab {
                InteractiveGrab::Move {
                    window_idx,
                    start_pointer,
                    start_geo,
                } => {
                    if let Some(w) = self.windows.get_mut(window_idx) {
                        let dx = (position.x - start_pointer.x).round() as i32;
                        let dy = (position.y - start_pointer.y).round() as i32;
                        w.float_geo.loc.x = start_geo.loc.x + dx;
                        w.float_geo.loc.y = start_geo.loc.y + dy;
                        w.geo = w.float_geo;
                        self.relayout();
                    }
                }
                InteractiveGrab::Resize {
                    window_idx,
                    start_pointer,
                    start_geo,
                } => {
                    if let Some(w) = self.windows.get_mut(window_idx) {
                        let dw = (position.x - start_pointer.x).round() as i32;
                        let dh = (position.y - start_pointer.y).round() as i32;
                        w.float_geo.size.w = (start_geo.size.w + dw).max(120);
                        w.float_geo.size.h = (start_geo.size.h + dh).max(80);
                        w.geo = w.float_geo;
                        self.relayout();
                    }
                }
            }
            return;
        }

        let under = if let Some((surf, loc)) = self.layer_surface_at(position) {
            Some((surf, loc.to_f64()))
        } else {
            self.window_at(position).map(|(idx, surface_loc)| {
                if self.focused_idx != Some(idx) {
                    self.focused_idx = Some(idx);
                    self.relayout();
                }
                let surf = self.windows[idx].wl_surface().clone();
                (surf, surface_loc.to_f64())
            })
        };

        let pointer = self.pointer.clone();
        pointer.motion(
            self,
            under,
            &MotionEvent {
                location: position,
                serial: Serial::from(0),
                time,
            },
        );
        pointer.frame(self);
    }

    /// Handle mouse wheel / touchpad two-finger scroll (`InputEvent::PointerAxis`).
    pub fn process_pointer_axis<B: InputBackend>(&mut self, event: B::PointerAxisEvent) {
        let pos = self.pointer_location;

        // Scrolling on the top status bar switches workspace tags!
        if self.workspace.show_bar
            && pos.y >= 0.0
            && pos.y < BAR_HEIGHT as f64
            && self.layer_surface_at(pos).is_none()
        {
            let v = event
                .amount(Axis::Vertical)
                .or_else(|| event.amount_v120(Axis::Vertical))
                .unwrap_or(0.0);
            if v.abs() > 0.01 {
                let ntags = TAG_NAMES.len() as isize;
                let dir: isize = if v > 0.0 { 1 } else { -1 };
                let next_tag = (self.workspace.cur_tag as isize + dir).rem_euclid(ntags) as usize;
                self.view_tags(1u16 << next_tag);
            }
            return;
        }

        let mut frame = AxisFrame::new(Event::time_msec(&event)).source(event.source());
        for axis in [Axis::Horizontal, Axis::Vertical] {
            if let Some(amount) = event.amount(axis) {
                if amount != 0.0 {
                    frame = frame.value(axis, amount);
                } else if matches!(event.source(), AxisSource::Finger | AxisSource::Continuous) {
                    frame = frame.stop(axis);
                }
            }
            if let Some(v120) = event.amount_v120(axis) {
                frame = frame.v120(axis, v120 as i32);
            }
            frame = frame.relative_direction(axis, event.relative_direction(axis));
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

        if state == ButtonState::Released {
            if self.active_grab.take().is_some() {
                self.relayout();
                return;
            }
        }

        let pos = self.pointer_location;

        // 0. If clicking on a layer-shell overlay (Spotlight), forward directly to it
        if self.layer_surface_at(pos).is_some() {
            let pointer = self.pointer.clone();
            pointer.button(
                self,
                &ButtonEvent {
                    button,
                    state,
                    serial: Serial::from(0),
                    time,
                },
            );
            pointer.frame(self);
            return;
        }

        const BTN_LEFT: u32 = 0x110;
        const BTN_RIGHT: u32 = 0x111;
        const BTN_MIDDLE: u32 = 0x112;

        // 1. Check if clicking on the top status bar
        if state == ButtonState::Pressed
            && self.workspace.show_bar
            && pos.y >= 0.0
            && pos.y < BAR_HEIGHT as f64
        {
            match self.bar.hit_test(pos.x as i32) {
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
            return;
        }

        // 2. Check for `Mod + Left/Right/Middle Click` interactive window move/resize/toggle-float
        if state == ButtonState::Pressed && is_mod_active(&self.modifiers, self.is_nested) {
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
                if button == BTN_LEFT {
                    self.active_grab = Some(InteractiveGrab::Move {
                        window_idx: idx,
                        start_pointer: pos,
                        start_geo,
                    });
                    self.relayout();
                    return;
                } else if button == BTN_RIGHT {
                    self.active_grab = Some(InteractiveGrab::Resize {
                        window_idx: idx,
                        start_pointer: pos,
                        start_geo,
                    });
                    self.relayout();
                    return;
                }
            }
        }

        // 3. Focus clicked window & forward button event to the Wayland client
        if state == ButtonState::Pressed {
            if let Some((idx, _)) = self.window_at(pos) {
                if self.focused_idx != Some(idx) {
                    self.focused_idx = Some(idx);
                    self.relayout();
                }
            }
        }

        let pointer = self.pointer.clone();
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state,
                serial: Serial::from(0),
                time,
            },
        );
        pointer.frame(self);
    }
}
