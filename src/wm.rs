//! `dwm`-inspired dynamic tiling engine (`[]=`, `[M]`, `|M|`, `###`, `><>`),
//! per-tag layout persistence (`pertag`), scratchpads, and window metadata.

use smithay::{
    backend::renderer::{
        element::Id,
        utils::{with_renderer_surface_state, CommitCounter},
        Color32F,
    },
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
    /// Floating window that picks its own size (dialogs floated by a rule): it is configured
    /// without a size and the frame follows what the client draws, until the user resizes it.
    pub size_from_client: bool,
    /// Whether the client's own size has been adopted at least once (first time also centres it).
    pub client_sized_once: bool,
    /// Last `(app_id, title)` seen by the rule matcher. Rules stay open until a
    /// rule matches or both strings are known, so a title that arrives after
    /// `app_id` can still match.
    pub rule_seen: (String, String),
    /// Height weight inside its tiled column (`dwm` `cfacts`); resizing a tiled
    /// window moves the split between it and its neighbour by changing these.
    pub cfact: f32,
    pub border_render_id: Id,
    pub glow_render_id: Id,
    pub backdrop_render_id: Id,
    pub border_commit: CommitCounter,
    pub border_color: Option<Color32F>,
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
            size_from_client: false,
            client_sized_once: false,
            rule_seen: (String::new(), String::new()),
            cfact: 1.0,
            border_render_id: Id::new(),
            glow_render_id: Id::new(),
            backdrop_render_id: Id::new(),
            border_commit: CommitCounter::default(),
            border_color: None,
        }
    }

    /// Size of the client's visible window (xdg window geometry, else its buffer), once mapped.
    pub fn client_geometry_size(&self) -> Option<Size<i32, Logical>> {
        let geometry = with_states(self.wl_surface(), |states| {
            states.cached_state.get::<SurfaceCachedState>().current().geometry
        });
        geometry
            .map(|g| g.size)
            .or_else(|| with_renderer_surface_state(self.wl_surface(), |s| s.surface_size()).flatten())
            .filter(|s| s.w > 0 && s.h > 0)
    }

    /// Clamp an outer frame size so the client area respects the client's min/max size hints.
    pub fn clamp_frame_to_client_limits(&self, frame: Size<i32, Logical>) -> Size<i32, Logical> {
        let (min, max) = with_states(self.wl_surface(), |states| {
            let mut cached = states.cached_state.get::<SurfaceCachedState>();
            let current = cached.current();
            (current.min_size, current.max_size)
        });
        let clamp_axis = |v: i32, lo: i32, hi: i32| {
            let inner = v - 2 * BORDER_WIDTH;
            let inner = if hi > 0 { inner.min(hi) } else { inner };
            let inner = if lo > 0 { inner.max(lo) } else { inner };
            inner + 2 * BORDER_WIDTH
        };
        Size::from((clamp_axis(frame.w, min.w, max.w), clamp_axis(frame.h, min.h, max.h)))
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
    ///
    /// The minimum is 1, not a fixed 64×48: a tiled frame can legitimately be
    /// smaller than that, and telling the client it is larger than its frame
    /// makes it draw past the window (the compositor then crops it).
    #[inline]
    pub fn surface_size(&self) -> Size<i32, Logical> {
        if self.is_fullscreen {
            self.geo.size
        } else {
            Size::from((
                (self.geo.size.w - 2 * BORDER_WIDTH).max(1),
                (self.geo.size.h - 2 * BORDER_WIDTH).max(1),
            ))
        }
    }

    /// Send an `xdg_toplevel.configure` event matching the window's assigned geometry and focus.
    pub fn configure(&self, is_focused: bool, tiled_layout: bool) {
        let size = self.surface_size();
        let is_tiled =
            !self.is_floating && !self.is_fullscreen && !self.is_scratchpad && tiled_layout;

        // Self-sizing floating windows get no size: the client picks its natural one.
        let client_picks_size = self.size_from_client
            && (self.is_floating || self.is_scratchpad)
            && !self.is_fullscreen;

        self.toplevel.with_pending_state(|state| {
            state.size = if client_picks_size { None } else { Some(size) };
            state.states.unset(xdg_toplevel::State::Activated);
            state.states.unset(xdg_toplevel::State::Fullscreen);
            state.states.unset(xdg_toplevel::State::TiledLeft);
            state.states.unset(xdg_toplevel::State::TiledRight);
            state.states.unset(xdg_toplevel::State::TiledTop);
            state.states.unset(xdg_toplevel::State::TiledBottom);

            if is_focused {
                state.states.set(xdg_toplevel::State::Activated);
            }
            // Maximize is not implemented: leave the bit clear so a client that
            // asked for it is not stuck "maximized" at its tiled size.
            state.states.unset(xdg_toplevel::State::Maximized);
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

pub(crate) fn floating_layout_geo(
    geo: Rectangle<i32, Logical>,
    float_geo: Rectangle<i32, Logical>,
    is_floating: bool,
    is_scratchpad: bool,
) -> Rectangle<i32, Logical> {
    if is_floating || is_scratchpad {
        float_geo
    } else {
        geo
    }
}

/// Parts of `total` separated by `gap`, summing (with the gaps) back to `total`.
/// Gaps shrink when they would otherwise push the parts past `total`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LengthSplit {
    pub gap: i32,
    pub lengths: Vec<i32>,
}

impl LengthSplit {
    pub fn span(&self) -> i32 {
        let n = self.lengths.len() as i32;
        self.lengths.iter().sum::<i32>() + self.gap * (n - 1).max(0)
    }
}

pub(crate) fn split_lengths(total: i32, count: usize, gap: i32) -> LengthSplit {
    if count == 0 {
        return LengthSplit {
            gap: 0,
            lengths: Vec::new(),
        };
    }
    let total = total.max(0);
    let count_i = count as i32;
    let mut gap = gap.max(0);
    if count > 1 {
        let max_gaps = (total - count_i).max(0);
        let wanted = gap * (count_i - 1);
        if wanted > max_gaps {
            gap = max_gaps / (count_i - 1);
        }
    } else {
        gap = 0;
    }
    let gaps = gap * (count_i - 1).max(0);
    let avail = total - gaps;
    let base = avail / count_i;
    let mut extra = avail % count_i;
    let mut lengths = Vec::with_capacity(count);
    for _ in 0..count {
        let mut len = base;
        if extra > 0 {
            len += 1;
            extra -= 1;
        }
        lengths.push(len);
    }
    LengthSplit { gap, lengths }
}

/// Like [`split_lengths`], but each part's share of the space follows its weight.
pub(crate) fn split_weighted(total: i32, weights: &[f32], gap: i32) -> LengthSplit {
    let mut split = split_lengths(total, weights.len(), gap);
    if weights.windows(2).all(|pair| pair[0] == pair[1]) {
        return split;
    }
    let weights: Vec<f64> = weights.iter().map(|w| f64::from(w.max(0.05))).collect();
    let sum: f64 = weights.iter().sum();
    let avail = f64::from(split.lengths.iter().sum::<i32>());
    let mut acc = 0.0;
    let mut prev = 0;
    for (len, weight) in split.lengths.iter_mut().zip(&weights) {
        acc += weight;
        let edge = (avail * acc / sum).round() as i32;
        *len = edge - prev;
        prev = edge;
    }
    split
}

/// Smallest height a row resize leaves a tiled window.
const MIN_TILED_ROW: i32 = 80;

/// The master/stack divider a tiled resize moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MfactSplit {
    /// `mfact` when the resize started.
    pub start: f32,
    /// `mfact` change per logical pixel the divider moves right.
    pub per_px: f32,
}

/// The divider between two neighbours in one tiled column.
#[derive(Debug, Clone, PartialEq)]
pub struct RowSplit<T> {
    pub upper: T,
    pub lower: T,
    pub upper_h: i32,
    pub lower_h: i32,
    /// Pixels of height per unit of `cfact` in this column.
    pub px_per_cfact: f32,
}

impl<T> RowSplit<T> {
    /// `(upper cfact, lower cfact)` once the divider has moved `dy` pixels down.
    pub fn cfacts(&self, dy: i32) -> (f32, f32) {
        let lo = (MIN_TILED_ROW - self.upper_h).min(0);
        let hi = (self.lower_h - MIN_TILED_ROW).max(0);
        let d = dy.clamp(lo, hi);
        (
            (self.upper_h + d) as f32 / self.px_per_cfact,
            (self.lower_h - d) as f32 / self.px_per_cfact,
        )
    }
}

/// Resizing a tiled window moves the splits beside it instead of floating it:
/// horizontal motion moves the master/stack divider, vertical motion the divider
/// to its neighbour in the same column. Each divider follows the pointer.
#[derive(Debug, Clone, PartialEq)]
pub struct TiledResize<T> {
    pub mfact: Option<MfactSplit>,
    pub rows: Option<RowSplit<T>>,
}

impl<T> TiledResize<T> {
    pub fn is_empty(&self) -> bool {
        self.mfact.is_none() && self.rows.is_none()
    }

    pub fn map<U>(self, mut f: impl FnMut(T) -> U) -> TiledResize<U> {
        TiledResize {
            mfact: self.mfact,
            rows: self.rows.map(|r| RowSplit {
                upper: f(r.upper),
                lower: f(r.lower),
                upper_h: r.upper_h,
                lower_h: r.lower_h,
                px_per_cfact: r.px_per_cfact,
            }),
        }
    }
}

/// `(master, stack, gap)` widths that add up to `usable_w`.
pub(crate) fn master_stack_widths(usable_w: i32, gap: i32, mfact: f32) -> (i32, i32, i32) {
    let usable_w = usable_w.max(0);
    if usable_w < 2 {
        return (usable_w, 0, 0);
    }
    let gap = gap.max(0).min(usable_w - 2);
    let inner = usable_w - gap;
    let mfact = mfact.clamp(0.10, 0.90);
    let master = ((inner as f32) * mfact).round() as i32;
    let master = master.clamp(1, inner - 1);
    (master, inner - master, gap)
}

/// `(left, master, right, gap)` for centered-master with a stack on both sides.
/// The four numbers reconstruct `usable_w` (`left + gap + master + gap + right`).
pub(crate) fn centered_side_widths(usable_w: i32, gap: i32, mfact: f32) -> (i32, i32, i32, i32) {
    let usable_w = usable_w.max(0);
    if usable_w < 3 {
        return (0, usable_w, 0, 0);
    }
    let gap = gap.max(0).min((usable_w - 3) / 2);
    let inner = usable_w - gap * 2;
    let mfact = mfact.clamp(0.10, 0.90);
    let master = ((inner as f32) * mfact).round() as i32;
    let master = master.clamp(1, inner - 2);
    let rem = inner - master;
    (rem / 2, master, rem - rem / 2, gap)
}

/// Centred scratchpad rectangle that stays inside the output, below the bar.
pub(crate) fn scratchpad_geo(output: Size<i32, Logical>, show_bar: bool) -> Rectangle<i32, Logical> {
    let top = if show_bar { BAR_HEIGHT } else { 0 };
    let max_w = (output.w - 16).max(1);
    let max_h = (output.h - top - 16).max(1);
    let sw = ((output.w * 7 / 10).max(480)).min(max_w).max(1);
    let sh = ((output.h * 7 / 10).max(320)).min(max_h).max(1);
    let sx = ((output.w - sw) / 2).clamp(0, (output.w - sw).max(0));
    let sy = (top + ((output.h - top - sh) / 2).max(0)).clamp(0, (output.h - sh).max(0));
    Rectangle::new(Point::from((sx, sy)), Size::from((sw, sh)))
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
        self.set_mfact(self.mfact() + delta);
    }

    pub fn set_mfact(&mut self, mfact: f32) {
        self.pertag[self.cur_tag].mfact = mfact.clamp(0.10, 0.90);
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

    /// Gap between tiled windows, or 0 while gaps are off.
    pub fn inner_gap(&self) -> i32 {
        if self.gaps_enabled {
            self.gap_inner
        } else {
            0
        }
    }

    /// The area tiled windows are laid out in: below the bar, inside the outer gaps.
    pub fn usable_area(&self, output_size: Size<i32, Logical>) -> Rectangle<i32, Logical> {
        let bar_y = if self.show_bar { BAR_HEIGHT } else { 0 };
        let g_outer = if self.gaps_enabled { self.gap_outer } else { 0 };
        Rectangle::new(
            Point::from((g_outer, bar_y + g_outer)),
            Size::from((
                (output_size.w - 2 * g_outer).max(1),
                (output_size.h - bar_y - 2 * g_outer).max(1),
            )),
        )
    }

    /// Indices of the windows the current layout tiles, in tile order.
    pub fn tiled_indices(&self, windows: &[ManagedWindow]) -> Vec<usize> {
        windows
            .iter()
            .enumerate()
            .filter(|(_, w)| {
                w.is_visible_on(self.selected_tags)
                    && !w.is_floating
                    && !w.is_fullscreen
                    && !w.is_scratchpad
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Whether centred-master currently has stack columns on both sides of the master.
    pub fn centered_sides(&self, tiled_count: usize) -> bool {
        let nmaster = self.nmaster().min(tiled_count);
        self.layout() == LayoutMode::CenteredMaster && nmaster > 0 && tiled_count - nmaster >= 2
    }

    /// Plan a resize of tiled window `target` grabbed at `edges`. `tiled` holds every
    /// tiled window as `(index, frame, cfact)`.
    pub(crate) fn plan_tiled_resize(
        &self,
        output_size: Size<i32, Logical>,
        tiled: &[(usize, Rectangle<i32, Logical>, f32)],
        target: usize,
        edges: crate::state::ResizeEdges,
    ) -> TiledResize<usize> {
        let layout = self.layout();
        let centered_sides = self.centered_sides(tiled.len());
        let usable = self.usable_area(output_size);
        let gap = self.inner_gap();
        let mut plan = TiledResize {
            mfact: None,
            rows: None,
        };
        let Some(&(_, geo, _)) = tiled.iter().find(|(i, _, _)| *i == target) else {
            return plan;
        };
        if matches!(layout, LayoutMode::Monocle | LayoutMode::Floating) {
            return plan;
        }

        // Prefer the grabbed edge; at the screen edge use the window's other side.
        if matches!(layout, LayoutMode::Tile | LayoutMode::CenteredMaster)
            && (edges.left || edges.right)
        {
            let left = geo.loc.x;
            let right = geo.loc.x + geo.size.w;
            let inside = |x: i32| x > usable.loc.x && x < usable.loc.x + usable.size.w;
            let picked = if edges.right {
                [right, left]
            } else {
                [left, right]
            }
            .into_iter()
            .find(|&x| inside(x));
            if let Some(divider) = picked {
                let per_px = if centered_sides {
                    let inner = (usable.size.w - 2 * gap).max(1) as f32;
                    let toward_master = if divider * 2 < usable.loc.x * 2 + usable.size.w {
                        -1.0
                    } else {
                        1.0
                    };
                    toward_master * 2.0 / inner
                } else {
                    1.0 / (usable.size.w - gap).max(1) as f32
                };
                plan.mfact = Some(MfactSplit {
                    start: self.mfact(),
                    per_px,
                });
            }
        }

        if edges.top || edges.bottom {
            let mut column: Vec<_> = tiled
                .iter()
                .filter(|(_, g, _)| g.loc.x == geo.loc.x && g.size.w == geo.size.w)
                .copied()
                .collect();
            column.sort_by_key(|(_, g, _)| g.loc.y);
            let pos = column.iter().position(|(i, _, _)| *i == target);
            let pair = pos.and_then(|pos| {
                let above = pos.checked_sub(1).map(|p| (p, pos));
                let below = (pos + 1 < column.len()).then_some((pos, pos + 1));
                if edges.bottom {
                    below.or(above)
                } else {
                    above.or(below)
                }
            });
            if let Some((up, low)) = pair {
                let heights: i32 = column.iter().map(|(_, g, _)| g.size.h).sum();
                let weights: f32 = column.iter().map(|(_, _, c)| c.max(0.05)).sum();
                let (upper, upper_geo, _) = column[up];
                let (lower, lower_geo, _) = column[low];
                plan.rows = Some(RowSplit {
                    upper,
                    lower,
                    upper_h: upper_geo.size.h,
                    lower_h: lower_geo.size.h,
                    px_per_cfact: heights as f32 / weights,
                });
            }
        }
        plan
    }

    /// Compute geometries for all visible windows on the current tags, just like `arrange()` in `dwm`.
    pub fn arrange(
        &self,
        windows: &mut [ManagedWindow],
        focused_idx: Option<usize>,
        output_size: Size<i32, Logical>,
    ) {
        let g_inner = self.inner_gap();
        let usable = self.usable_area(output_size);
        let tiled_indices = self.tiled_indices(windows);

        let n = tiled_indices.len();
        let layout = self.layout();
        let mfact = self.mfact();
        let nmaster_cfg = self.nmaster();

        match layout {
            LayoutMode::Floating => {
                for w in windows.iter_mut() {
                    if w.is_visible_on(self.selected_tags) && !w.is_fullscreen {
                        w.geo =
                            floating_layout_geo(w.geo, w.float_geo, w.is_floating, w.is_scratchpad);
                        w.float_geo = w.geo;
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
                    if nmaster == 0 || nmaster >= n {
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
                        let (master_w, stack_w, col_gap) =
                            master_stack_widths(usable.size.w, g_inner, mfact);
                        Self::arrange_column(
                            windows,
                            &tiled_indices[..nmaster],
                            usable.loc.x,
                            usable.loc.y,
                            master_w,
                            usable.size.h,
                            g_inner,
                        );
                        Self::arrange_column(
                            windows,
                            &tiled_indices[nmaster..],
                            usable.loc.x + master_w + col_gap,
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

                        if nstack == 1 {
                            let (mw, sw, col_gap) =
                                master_stack_widths(usable.size.w, g_inner, mfact);
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
                                usable.loc.x + mw + col_gap,
                                usable.loc.y,
                                sw,
                                usable.size.h,
                                g_inner,
                            );
                        } else {
                            let (left_w, master_w, right_w, col_gap) =
                                centered_side_widths(usable.size.w, g_inner, mfact);
                            let master_x = usable.loc.x + left_w + col_gap;
                            let right_x = master_x + master_w + col_gap;

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
                    let split = split_lengths(usable.size.w, cols, g_inner);
                    let mut cx = usable.loc.x;

                    for c in 0..cols {
                        let start_idx = c * rows;
                        if start_idx >= n {
                            break;
                        }
                        let end_idx = ((c + 1) * rows).min(n);
                        let col_slice = &tiled_indices[start_idx..end_idx];
                        let cw = split.lengths[c];
                        Self::arrange_column(
                            windows,
                            col_slice,
                            cx,
                            usable.loc.y,
                            cw,
                            usable.size.h,
                            g_inner,
                        );
                        cx += cw + split.gap;
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
        let weights: Vec<f32> = indices.iter().map(|&i| windows[i].cfact).collect();
        let split = split_weighted(total_h, &weights, gap_inner);
        let mut cur_y = y;

        for (pos, &win_idx) in indices.iter().enumerate() {
            let h = split.lengths[pos];
            windows[win_idx].geo = Rectangle::new(Point::from((x, cur_y)), Size::from((w, h)));
            cur_y += h + split.gap;
        }
    }
}
