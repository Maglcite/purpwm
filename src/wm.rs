//! `dwm`-inspired dynamic tiling engine (`[]=`, `[M]`, `|M|`, `###`, `><>`),
//! per-tag layout persistence (`pertag`), scratchpads, and window metadata.

use smithay::{
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Rectangle, Size},
    wayland::{
        compositor::with_states,
        shell::xdg::{SurfaceCachedState, ToplevelSurface},
    },
};

use crate::config::{
    BAR_HEIGHT, BORDER_WIDTH, DEFAULT_MFACT, DEFAULT_NMASTER, GAP_INNER, GAP_OUTER, TAG_NAMES,
};

/// Available `dwm` layout modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    /// `[]=` — Master on left, stack on right.
    Tile,
    /// `[M]` — Monocle (all tiled windows full-screened below bar).
    Monocle,
    /// `|M|` — Centered Master (Master column centered, stacks on left and right).
    CenteredMaster,
    /// `###` — Equal Grid layout.
    Grid,
    /// `><>` — Floating (manual position & size).
    Floating,
}

impl LayoutMode {
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::Tile => "[]=",
            Self::Monocle => "[M]",
            Self::CenteredMaster => "|M|",
            Self::Grid => "###",
            Self::Floating => "><>",
        }
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Tile => Self::CenteredMaster,
            Self::CenteredMaster => Self::Grid,
            Self::Grid => Self::Monocle,
            Self::Monocle => Self::Floating,
            Self::Floating => Self::Tile,
        }
    }
}

/// Per-window state managed by `purpwm`.
#[derive(Debug, Clone)]
pub struct ManagedWindow {
    pub toplevel: ToplevelSurface,
    /// 9-bit tag mask (`1 << 0` .. `1 << 8`).
    pub tags: u16,
    /// Whether this specific window is floating.
    pub is_floating: bool,
    /// Whether this window is fullscreen (`Mod + F11` or client request).
    pub is_fullscreen: bool,
    /// Whether this window is urgent.
    pub is_urgent: bool,
    /// Whether this window is the drop-down scratchpad terminal (`Mod + s`).
    pub is_scratchpad: bool,
    /// Whether the scratchpad window is currently hidden.
    pub is_hidden: bool,
    /// Optional custom opacity override (`Some(1.0)` for opaque mode `Mod + o`).
    pub opacity_override: Option<f32>,
    /// Whether compile-time `WINDOW_RULES` have already been evaluated once the title/app_id arrived.
    pub rules_applied: bool,
    /// Current outer frame geometry (including borders) in logical coordinates.
    pub geo: Rectangle<i32, Logical>,
    /// Saved floating geometry restored when toggling floating or switching to `><>`.
    pub float_geo: Rectangle<i32, Logical>,
}

impl ManagedWindow {
    pub fn new(toplevel: ToplevelSurface, tags: u16, output_size: Size<i32, Logical>) -> Self {
        let w = (output_size.w * 3 / 5).max(360);
        let h = (output_size.h * 3 / 5).max(240);
        let x = (output_size.w - w) / 2;
        let y = BAR_HEIGHT + (output_size.h - BAR_HEIGHT - h).max(0) / 2;
        let float_geo = Rectangle::new(Point::from((x, y)), Size::from((w, h)));

        Self {
            toplevel,
            tags,
            is_floating: false,
            is_fullscreen: false,
            is_urgent: false,
            is_scratchpad: false,
            is_hidden: false,
            opacity_override: None,
            rules_applied: false,
            geo: float_geo,
            float_geo,
        }
    }

    #[inline]
    pub fn wl_surface(&self) -> &WlSurface {
        self.toplevel.wl_surface()
    }

    #[inline]
    pub fn is_visible_on(&self, selected_tags: u16) -> bool {
        if self.is_hidden {
            return false;
        }
        if self.is_scratchpad {
            return true;
        }
        (self.tags & selected_tags) != 0
    }

    /// Extract `(app_id, title)` from the client's `XdgToplevelSurfaceData`.
    pub fn app_id_and_title(&self) -> (String, String) {
        with_states(self.wl_surface(), |states| {
            let role = states
                .data_map
                .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>();
            if let Some(data) = role {
                let guard = data.lock().unwrap();
                let app_id = guard.app_id.clone().unwrap_or_default();
                let title = guard.title.clone().unwrap_or_default();
                return (app_id, title);
            }
            (String::new(), String::new())
        })
    }

    /// Extract the client's display title (or `app_id` fallback).
    pub fn title(&self) -> String {
        let (app_id, title) = self.app_id_and_title();
        if !title.is_empty() {
            title
        } else if !app_id.is_empty() {
            app_id
        } else {
            String::from("purpwm client")
        }
    }

    /// Where the client's visible window geometry starts (inside the 2px border).
    #[inline]
    pub fn content_loc(&self) -> Point<i32, Logical> {
        if self.is_fullscreen {
            self.geo.loc
        } else {
            Point::from((self.geo.loc.x + BORDER_WIDTH, self.geo.loc.y + BORDER_WIDTH))
        }
    }

    /// Buffer origin of the toplevel surface. Clients drawing client-side shadows (GTK)
    /// set a window geometry offset, so the buffer starts before the visible content.
    #[inline]
    pub fn surface_loc(&self) -> Point<i32, Logical> {
        let geometry_offset = with_states(self.wl_surface(), |states| {
            states
                .cached_state
                .get::<SurfaceCachedState>()
                .current()
                .geometry
                .map(|g| g.loc)
                .unwrap_or_default()
        });
        self.content_loc() - geometry_offset
    }

    /// Inner surface size (excluding 2*BORDER_WIDTH).
    #[inline]
    pub fn surface_size(&self) -> Size<i32, Logical> {
        if self.is_fullscreen {
            self.geo.size
        } else {
            Size::from((
                (self.geo.size.w - 2 * BORDER_WIDTH).max(64),
                (self.geo.size.h - 2 * BORDER_WIDTH).max(48),
            ))
        }
    }

    /// Send an `xdg_toplevel.configure` event matching the window's assigned geometry and focus.
    pub fn configure(&self, is_focused: bool, tiled_layout: bool) {
        let size = self.surface_size();
        let is_tiled =
            !self.is_floating && !self.is_fullscreen && !self.is_scratchpad && tiled_layout;

        self.toplevel.with_pending_state(|state| {
            state.size = Some(size);
            state.states.unset(xdg_toplevel::State::Activated);
            state.states.unset(xdg_toplevel::State::Fullscreen);
            state.states.unset(xdg_toplevel::State::TiledLeft);
            state.states.unset(xdg_toplevel::State::TiledRight);
            state.states.unset(xdg_toplevel::State::TiledTop);
            state.states.unset(xdg_toplevel::State::TiledBottom);

            if is_focused {
                state.states.set(xdg_toplevel::State::Activated);
            }
            if self.is_fullscreen {
                state.states.set(xdg_toplevel::State::Fullscreen);
            } else if is_tiled {
                state.states.set(xdg_toplevel::State::TiledLeft);
                state.states.set(xdg_toplevel::State::TiledRight);
                state.states.set(xdg_toplevel::State::TiledTop);
                state.states.set(xdg_toplevel::State::TiledBottom);
            }
        });
        self.toplevel.send_pending_configure();
    }
}

/// Per-tag configuration (`dwm` `pertag` patch).
#[derive(Debug, Clone, Copy)]
pub struct TagState {
    pub layout: LayoutMode,
    pub prev_layout: LayoutMode,
    pub mfact: f32,
    pub nmaster: usize,
}

impl Default for TagState {
    fn default() -> Self {
        Self {
            layout: LayoutMode::Tile,
            prev_layout: LayoutMode::Floating,
            mfact: DEFAULT_MFACT,
            nmaster: DEFAULT_NMASTER,
        }
    }
}

/// Workspace and layout state (equivalent to `Monitor` + `Pertag` in `dwm`).
#[derive(Debug)]
pub struct Workspace {
    /// Bitmask of currently viewed tags (`1 << 0` by default).
    pub selected_tags: u16,
    /// Previous tag bitmask for `Mod + Tab` quick toggle.
    pub prev_tags: u16,
    /// Active primary tag index (`0..8`) for `pertag` state lookup.
    pub cur_tag: usize,
    /// Per-tag layout, `mfact`, and `nmaster` state (`0..9`).
    pub pertag: [TagState; TAG_NAMES.len()],
    /// Runtime inner gap between tiled windows (`Mod + =` / `Mod + -`).
    pub gap_inner: i32,
    /// Runtime outer gap between screen edges/bar and tiled windows.
    pub gap_outer: i32,
    /// Whether gaps are currently enabled (`Mod + Shift + 0` or `Mod + g` toggles).
    pub gaps_enabled: bool,
    /// Whether the top status bar is visible (`Mod + b`).
    pub show_bar: bool,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            selected_tags: 1 << 0,
            prev_tags: 1 << 0,
            cur_tag: 0,
            pertag: [TagState::default(); TAG_NAMES.len()],
            gap_inner: GAP_INNER,
            gap_outer: GAP_OUTER,
            gaps_enabled: true,
            show_bar: true,
        }
    }
}

impl Workspace {
    #[inline]
    pub fn layout(&self) -> LayoutMode {
        self.pertag[self.cur_tag].layout
    }

    #[inline]
    pub fn prev_layout(&self) -> LayoutMode {
        self.pertag[self.cur_tag].prev_layout
    }

    #[inline]
    pub fn mfact(&self) -> f32 {
        self.pertag[self.cur_tag].mfact
    }

    #[inline]
    pub fn nmaster(&self) -> usize {
        self.pertag[self.cur_tag].nmaster
    }

    pub fn set_layout(&mut self, mode: LayoutMode) {
        let cur = &mut self.pertag[self.cur_tag];
        if cur.layout != mode {
            cur.prev_layout = cur.layout;
            cur.layout = mode;
        }
    }

    pub fn adjust_mfact(&mut self, delta: f32) {
        let cur = &mut self.pertag[self.cur_tag];
        cur.mfact = (cur.mfact + delta).clamp(0.10, 0.90);
    }

    pub fn adjust_nmaster(&mut self, delta: isize) {
        let cur = &mut self.pertag[self.cur_tag];
        cur.nmaster = (cur.nmaster as isize + delta).max(0) as usize;
    }

    pub fn adjust_gaps(&mut self, delta: i32) {
        self.gaps_enabled = true;
        self.gap_inner = (self.gap_inner + delta).clamp(0, 48);
        self.gap_outer = (self.gap_outer + delta).clamp(0, 64);
    }

    pub fn reset_gaps(&mut self) {
        self.gaps_enabled = true;
        self.gap_inner = GAP_INNER;
        self.gap_outer = GAP_OUTER;
    }

    pub fn toggle_gaps(&mut self) {
        self.gaps_enabled = !self.gaps_enabled;
    }

    /// Update `cur_tag` whenever `selected_tags` changes.
    pub fn sync_cur_tag(&mut self) {
        for i in 0..TAG_NAMES.len() {
            if (self.selected_tags & (1 << i)) != 0 {
                self.cur_tag = i;
                break;
            }
        }
    }

    /// Compute geometries for all visible windows on the current tags, just like `arrange()` in `dwm`.
    pub fn arrange(
        &self,
        windows: &mut [ManagedWindow],
        focused_idx: Option<usize>,
        output_size: Size<i32, Logical>,
    ) {
        let bar_y = if self.show_bar { BAR_HEIGHT } else { 0 };
        let g_outer = if self.gaps_enabled { self.gap_outer } else { 0 };
        let g_inner = if self.gaps_enabled { self.gap_inner } else { 0 };

        let usable = Rectangle::<i32, Logical>::new(
            Point::from((g_outer, bar_y + g_outer)),
            Size::from((
                (output_size.w - 2 * g_outer).max(100),
                (output_size.h - bar_y - 2 * g_outer).max(100),
            )),
        );

        let tiled_indices: Vec<usize> = windows
            .iter()
            .enumerate()
            .filter(|(_, w)| {
                w.is_visible_on(self.selected_tags)
                    && !w.is_floating
                    && !w.is_fullscreen
                    && !w.is_scratchpad
            })
            .map(|(i, _)| i)
            .collect();

        let n = tiled_indices.len();
        let layout = self.layout();
        let mfact = self.mfact();
        let nmaster_cfg = self.nmaster();

        match layout {
            LayoutMode::Floating => {
                for w in windows.iter_mut() {
                    if w.is_visible_on(self.selected_tags) && !w.is_fullscreen {
                        w.geo = w.float_geo;
                    }
                }
            }
            LayoutMode::Monocle => {
                for &idx in &tiled_indices {
                    windows[idx].geo = usable;
                }
            }
            LayoutMode::Tile => {
                if n > 0 {
                    let nmaster = nmaster_cfg.min(n);
                    let master_w = if n > nmaster && nmaster > 0 {
                        ((usable.size.w - g_inner) as f32 * mfact).round() as i32
                    } else {
                        usable.size.w
                    };

                    if nmaster > 0 {
                        Self::arrange_column(
                            windows,
                            &tiled_indices[..nmaster],
                            usable.loc.x,
                            usable.loc.y,
                            master_w,
                            usable.size.h,
                            g_inner,
                        );
                    }

                    let nstack = n.saturating_sub(nmaster);
                    if nstack > 0 {
                        let stack_x = if nmaster > 0 {
                            usable.loc.x + master_w + g_inner
                        } else {
                            usable.loc.x
                        };
                        let stack_w = if nmaster > 0 {
                            (usable.size.w - master_w - g_inner).max(64)
                        } else {
                            usable.size.w
                        };
                        Self::arrange_column(
                            windows,
                            &tiled_indices[nmaster..],
                            stack_x,
                            usable.loc.y,
                            stack_w,
                            usable.size.h,
                            g_inner,
                        );
                    }
                }
            }
            LayoutMode::CenteredMaster => {
                if n > 0 {
                    let nmaster = nmaster_cfg.min(n);
                    if n <= nmaster || nmaster == 0 {
                        Self::arrange_column(
                            windows,
                            &tiled_indices,
                            usable.loc.x,
                            usable.loc.y,
                            usable.size.w,
                            usable.size.h,
                            g_inner,
                        );
                    } else {
                        let stack_slice = &tiled_indices[nmaster..];
                        let nstack = stack_slice.len();
                        let master_w = (usable.size.w as f32 * mfact).round() as i32;

                        if nstack == 1 {
                            let mw = ((usable.size.w - g_inner) as f32 * mfact).round() as i32;
                            let sw = (usable.size.w - mw - g_inner).max(64);
                            Self::arrange_column(
                                windows,
                                &tiled_indices[..nmaster],
                                usable.loc.x,
                                usable.loc.y,
                                mw,
                                usable.size.h,
                                g_inner,
                            );
                            Self::arrange_column(
                                windows,
                                stack_slice,
                                usable.loc.x + mw + g_inner,
                                usable.loc.y,
                                sw,
                                usable.size.h,
                                g_inner,
                            );
                        } else {
                            let rem_w = (usable.size.w - master_w - 2 * g_inner).max(128);
                            let left_w = rem_w / 2;
                            let right_w = rem_w - left_w;
                            let master_x = usable.loc.x + left_w + g_inner;
                            let right_x = master_x + master_w + g_inner;

                            Self::arrange_column(
                                windows,
                                &tiled_indices[..nmaster],
                                master_x,
                                usable.loc.y,
                                master_w,
                                usable.size.h,
                                g_inner,
                            );

                            let left_indices: Vec<usize> = stack_slice
                                .iter()
                                .enumerate()
                                .filter(|(i, _)| i % 2 == 0)
                                .map(|(_, &idx)| idx)
                                .collect();
                            let right_indices: Vec<usize> = stack_slice
                                .iter()
                                .enumerate()
                                .filter(|(i, _)| i % 2 == 1)
                                .map(|(_, &idx)| idx)
                                .collect();

                            Self::arrange_column(
                                windows,
                                &left_indices,
                                usable.loc.x,
                                usable.loc.y,
                                left_w,
                                usable.size.h,
                                g_inner,
                            );
                            Self::arrange_column(
                                windows,
                                &right_indices,
                                right_x,
                                usable.loc.y,
                                right_w,
                                usable.size.h,
                                g_inner,
                            );
                        }
                    }
                }
            }
            LayoutMode::Grid => {
                if n > 0 {
                    let cols = (n as f64).sqrt().ceil() as usize;
                    let rows = n.div_ceil(cols);
                    let total_col_gaps = g_inner * (cols as i32 - 1).max(0);
                    let cell_w = ((usable.size.w - total_col_gaps) / cols as i32).max(64);

                    for c in 0..cols {
                        let start_idx = c * rows;
                        if start_idx >= n {
                            break;
                        }
                        let end_idx = ((c + 1) * rows).min(n);
                        let col_slice = &tiled_indices[start_idx..end_idx];
                        let cx = usable.loc.x + (c as i32) * (cell_w + g_inner);
                        let cw = if c + 1 == cols {
                            (usable.loc.x + usable.size.w - cx).max(64)
                        } else {
                            cell_w
                        };
                        Self::arrange_column(
                            windows,
                            col_slice,
                            cx,
                            usable.loc.y,
                            cw,
                            usable.size.h,
                            g_inner,
                        );
                    }
                }
            }
        }

        // Apply floating/fullscreen/scratchpad geometries and send XDG configures.
        let tiled_layout = layout != LayoutMode::Floating;
        for (idx, w) in windows.iter_mut().enumerate() {
            if !w.is_visible_on(self.selected_tags) {
                // Windows on hidden tags must not keep looking focused (blinking cursors, etc.)
                w.toplevel.with_pending_state(|state| {
                    state.states.unset(xdg_toplevel::State::Activated);
                });
                w.toplevel.send_pending_configure();
                continue;
            }
            if w.is_fullscreen {
                w.geo = Rectangle::from_size(output_size);
            } else if w.is_floating || w.is_scratchpad {
                w.geo = w.float_geo;
            }
            let is_focused = focused_idx == Some(idx);
            w.configure(is_focused, tiled_layout);
        }
    }

    fn arrange_column(
        windows: &mut [ManagedWindow],
        indices: &[usize],
        x: i32,
        y: i32,
        w: i32,
        total_h: i32,
        gap_inner: i32,
    ) {
        let count = indices.len();
        if count == 0 {
            return;
        }
        let total_gaps = gap_inner * (count as i32 - 1).max(0);
        let each_h = ((total_h - total_gaps) / count as i32).max(48);
        let mut cur_y = y;

        for (pos, &win_idx) in indices.iter().enumerate() {
            let h = if pos + 1 == count {
                (y + total_h - cur_y).max(48)
            } else {
                each_h
            };
            windows[win_idx].geo = Rectangle::new(Point::from((x, cur_y)), Size::from((w, h)));
            cur_y += h + gap_inner;
        }
    }
}
