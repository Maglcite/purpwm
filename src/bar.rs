//! Built-in `dwm`-style translucent purple status bar rendered with `fontdue` into a
//! cached `MemoryRenderBuffer` (re-rasterized only when its inputs change), with a
//! glyph cache, non-blocking Wi-Fi / Bluetooth / volume status pills,
//! direct Linux `/proc` parsing for metrics, and a high-visibility cursor texture.

use std::{
    collections::HashMap,
    fs,
    process::{Command, Output, Stdio},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use fontdue::{Font, FontSettings, Metrics};
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            ImportMem, Renderer,
        },
    },
    utils::{Buffer, Point, Rectangle, Transform},
};

use crate::{
    config::{
        BAR_ACCENT_LINE_RGBA, BAR_ACCENT_RGBA, BAR_BG_RGBA, BAR_FONT_SIZE, BAR_HEIGHT,
        BAR_LAYOUT_BG_RGBA, BAR_TAG_ACTIVE_RGBA, BAR_TEXT_DIM_RGBA, BAR_TEXT_FG_RGBA, FONT_PATHS,
        TAG_NAMES,
    },
    wm::LayoutMode,
};

/// Click target returned when the user clicks on the top status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarClickAction {
    /// Clicked on tag `0..9`.
    Tag(usize),
    /// Clicked on the layout indicator (`[]=`, `[M]`, `|M|`, `###`, `><>`).
    CycleLayout,
    /// Clicked on the Wi-Fi status pill (`[ WIFI: ... ]`).
    Wifi,
    /// Clicked on the Bluetooth status pill (`[ BT: ... ]`).
    Bluetooth,
    /// Clicked on the volume pill (`[ VOL 45% ]`).
    Volume,
    /// Clicked on the Flameshot screenshot button.
    Flameshot,
    /// Clicked on the right-aligned System Metrics area (CPU / MEM / BAT / Clock).
    SystemMonitor,
    /// Clicked on the window title area.
    FocusNext,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct SystemStatusSnapshot {
    wifi_label: String,
    wifi_connected: bool,
    bt_label: String,
    bt_active: bool,
    volume_label: String,
    volume_unmuted: bool,
    sys_metrics: String,
}

/// Coalesced one-shot immediate-refresh request handed to the background status thread;
/// at most one refresh is ever pending, so requests never pile up worker threads.
#[derive(Default)]
struct StatusRefreshRequest {
    pending: Option<Instant>,
}

/// Everything the bar draws besides the status snapshot; a redraw happens only when this changes.
#[derive(Clone, PartialEq)]
struct BarInputs {
    selected_tags: u16,
    occupied_tags: u16,
    focused_tags: u16,
    urgent_tags: u16,
    layout: LayoutMode,
    focused_title: String,
}

/// Upper bound for lazily rasterized non-ASCII glyphs (titles with CJK, emoji, dashes...).
const GLYPH_CACHE_LIMIT: usize = 1024;

#[derive(Clone)]
struct CachedGlyph {
    metrics: Metrics,
    bitmap: Vec<u8>,
}

/// Cached status bar state & software render buffer.
pub struct StatusBar {
    font: Option<Font>,
    glyph_cache: HashMap<char, CachedGlyph>,
    buffer: MemoryRenderBuffer,
    cursor_buffer: MemoryRenderBuffer,
    width: i32,
    height: i32,
    /// Output scale: the bar is rasterized in physical pixels so text stays crisp.
    scale: f64,
    font_px: f32,
    dirty: bool,
    last_status_check: Instant,
    shared_status: Arc<Mutex<SystemStatusSnapshot>>,
    status_request: Arc<(Mutex<StatusRefreshRequest>, Condvar)>,
    current_status: SystemStatusSnapshot,
    last_inputs: Option<BarInputs>,
    tag_cell_width: i32,
    layout_cell_end: i32,
    wifi_pill_range: (i32, i32),
    bt_pill_range: (i32, i32),
    vol_pill_range: (i32, i32),
    flameshot_pill_range: (i32, i32),
    metrics_range: (i32, i32),
}

impl StatusBar {
    pub fn new(initial_width: i32) -> Self {
        let width = initial_width.max(320);
        let height = BAR_HEIGHT;
        let font = Some(Self::load_font());
        let glyph_cache = HashMap::with_capacity(128);

        let buffer = MemoryRenderBuffer::new(
            Fourcc::Argb8888,
            (width, height),
            1,
            Transform::Normal,
            None,
        );

        let cursor_buffer = Self::create_purple_cursor_buffer();

        let initial_snapshot = SystemStatusSnapshot {
            wifi_label: "WIFI: ...".to_string(),
            wifi_connected: false,
            bt_label: "BT: ...".to_string(),
            bt_active: false,
            volume_label: "VOL ...".to_string(),
            volume_unmuted: false,
            sys_metrics: Self::collect_fast_metrics(),
        };
        let shared_status = Arc::new(Mutex::new(initial_snapshot.clone()));
        let status_request =
            Arc::new((Mutex::new(StatusRefreshRequest::default()), Condvar::new()));

        // Background thread: battery, CPU, memory, volume and clock every 2 seconds; the
        // radios (nmcli / bluetoothctl subprocesses) every 6 seconds. Never blocks the frame loop.
        let bg_shared = Arc::clone(&shared_status);
        let bg_request = Arc::clone(&status_request);
        std::thread::spawn(move || {
            let mut tick = 0u64;
            let mut next_regular = Instant::now();
            loop {
                // Service at most one pending immediate refresh per pass; requests that
                // arrived while one was pending were coalesced into it by the caller.
                let immediate = {
                    let mut request = bg_request.0.lock().unwrap_or_else(|e| e.into_inner());
                    request.pending.take_if(|at| *at <= Instant::now())
                };

                // Start from the latest shared snapshot so fields refreshed more recently
                // by the other polling path are never overwritten with stale values.
                let mut snap = bg_shared.lock().map(|g| g.clone()).unwrap_or_default();
                if immediate.is_some() || tick.is_multiple_of(3) {
                    Self::refresh_radios(&mut snap);
                }
                Self::refresh_fast(&mut snap);
                if let Ok(mut guard) = bg_shared.lock() {
                    *guard = snap;
                }
                tick = tick.wrapping_add(1);

                next_regular += Duration::from_secs(2);
                if next_regular <= Instant::now() {
                    next_regular = Instant::now() + Duration::from_secs(2);
                }

                // Sleep until the next regular poll, waking early as soon as an immediate
                // refresh's debounce has elapsed.
                let mut request = bg_request.0.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    let now = Instant::now();
                    if request.pending.is_some_and(|at| at <= now) || now >= next_regular {
                        break;
                    }
                    let wake_at = request
                        .pending
                        .map_or(next_regular, |at| at.min(next_regular));
                    let wait = wake_at.saturating_duration_since(now);
                    if wait.is_zero() {
                        break;
                    }
                    match bg_request.1.wait_timeout(request, wait) {
                        Ok((guard, _)) => request = guard,
                        Err(err) => request = err.into_inner().0,
                    }
                }
            }
        });

        let mut bar = Self {
            font,
            glyph_cache,
            buffer,
            cursor_buffer,
            width,
            height,
            scale: 1.0,
            font_px: BAR_FONT_SIZE,
            dirty: true,
            last_status_check: Instant::now() - Duration::from_secs(10),
            shared_status,
            status_request,
            current_status: initial_snapshot,
            last_inputs: None,
            tag_cell_width: 34,
            layout_cell_end: 34 * 9 + 52,
            wifi_pill_range: (0, 0),
            bt_pill_range: (0, 0),
            vol_pill_range: (0, 0),
            flameshot_pill_range: (0, 0),
            metrics_range: (0, 0),
        };
        bar.cache_ascii_glyphs();
        bar
    }

    /// Pre-rasterize printable ASCII (`32..=126`) at the current font size for O(1) redraws.
    fn cache_ascii_glyphs(&mut self) {
        let ascii: String = (32u8..=126u8).map(char::from).collect();
        self.cache_glyphs(&ascii);
    }

    /// Re-rasterize the bar for a new output scale (bar height, font and spacing all scale).
    pub fn set_scale(&mut self, scale: f64) {
        if (scale - self.scale).abs() < f64::EPSILON {
            return;
        }
        self.scale = scale;
        self.font_px = BAR_FONT_SIZE * scale as f32;
        self.height = (BAR_HEIGHT as f64 * scale).round() as i32;
        self.tag_cell_width = (34.0 * scale).round() as i32;
        // Keep a non-empty layout-symbol target until the next raster measures it.
        // Setting this equal to the tag strip made the symbol unclickable.
        self.layout_cell_end =
            self.tag_cell_width * TAG_NAMES.len() as i32 + (52.0 * scale).round() as i32;
        self.glyph_cache.clear();
        self.cache_ascii_glyphs();
        self.dirty = true;
    }

    fn create_purple_cursor_buffer() -> MemoryRenderBuffer {
        const CW: i32 = 20;
        const CH: i32 = 26;
        let mut buf =
            MemoryRenderBuffer::new(Fourcc::Argb8888, (CW, CH), 1, Transform::Normal, None);
        let mut ctx = buf.render();
        let _ = ctx.draw(|pixels| {
            pixels.fill(0);
            // High-contrast 20x26 pointer cursor:
            // 'W' = Crisp white-lavender outer outline (`#FAF5FF`)
            // 'P' = Vibrant royal purple interior (`#9333EA`)
            // 'L' = Bright lavender highlight (`#D8B4FE`)
            // 'S' = Dark drop shadow for visibility on light backgrounds
            let rows: &[&str] = &[
                "W...................",
                "WW..................",
                "WLW.................",
                "WLLW................",
                "WLPLW...............",
                "WLPPLW..............",
                "WLPPPLW.............",
                "WLPPPPLW............",
                "WLPPPPPLW...........",
                "WLPPPPPPLW..........",
                "WLPPPPPPPLW.........",
                "WLPPPPPPPPLW........",
                "WLPPPPPPPPPLW.......",
                "WLPPPPPPWWWWWW......",
                "WLPPWLPPLWSSSS......",
                "WLPW.WLPPLW.........",
                "WLW..SWLPPLW........",
                "WW....WLPPLW........",
                "W.....SWLPPLW.......",
                ".......WLPPLW.......",
                ".......SWLPLW.......",
                "........WWWW........",
                "........SSSS........",
            ];
            for (y, row) in rows.iter().enumerate() {
                for (x, ch) in row.chars().enumerate() {
                    let rgba = match ch {
                        'W' => [0xFA, 0xF5, 0xFF, 255],
                        'L' => [0xD8, 0xB4, 0xFE, 255],
                        'P' => [0x93, 0x33, 0xEA, 255],
                        'S' => [0x0D, 0x05, 0x18, 190],
                        _ => continue,
                    };
                    put_pixel_argb8888(pixels, CW, CH, x as i32, y as i32, rgba);
                }
            }
            Result::<Vec<Rectangle<i32, Buffer>>, ()>::Ok(vec![Rectangle::from_size(
                (CW, CH).into(),
            )])
        });
        drop(ctx);
        buf
    }

    /// Render the software cursor at `pos`.
    pub fn cursor_render_element<R>(
        &self,
        renderer: &mut R,
        pos: Point<f64, smithay::utils::Physical>,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            pos,
            &self.cursor_buffer,
            Some(1.0),
            None,
            None,
            Kind::Cursor,
        )
        .ok()
    }

    fn load_font() -> Font {
        let load = |path: &str| {
            let bytes = fs::read(path).ok()?;
            match Font::from_bytes(bytes, FontSettings::default()) {
                Ok(font) => {
                    tracing::info!("Loaded status bar font: {}", path);
                    Some(font)
                }
                Err(err) => {
                    tracing::warn!("Unusable status bar font {}: {}", path, err);
                    None
                }
            }
        };
        let configured = std::env::var("PURPWM_FONT").ok().filter(|p| !p.is_empty());
        configured
            .as_deref()
            .into_iter()
            .chain(FONT_PATHS.iter().copied())
            .find_map(load)
            // Only ask fontconfig when none of the known paths exist on this distro.
            .or_else(|| fontconfig_monospace().as_deref().and_then(load))
            .or_else(|| {
                // Text must never disappear: the lock screen and help overlay use this font too.
                tracing::warn!("No system font usable; using the bundled DejaVu Sans Mono");
                Font::from_bytes(BUNDLED_FONT, FontSettings::default()).ok()
            })
            .expect("bundled DejaVu Sans Mono must be a valid font")
    }

    /// The loaded status bar font (shared with the lock screen).
    pub fn font(&self) -> Option<&Font> {
        self.font.as_ref()
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Trigger an immediate asynchronous refresh of the Wi-Fi, Bluetooth & volume pills.
    ///
    /// Single-flight: while a refresh is already pending, further calls are coalesced into
    /// it instead of each spawning a delayed thread. Returns `true` when this call armed
    /// the pending refresh.
    pub fn request_immediate_status_refresh(&mut self) -> bool {
        let mut request = self
            .status_request
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if request.pending.is_some() {
            return false;
        }
        request.pending = Some(Instant::now() + Duration::from_millis(350));
        self.status_request.1.notify_one();
        true
    }

    /// Check if the background status thread produced a new snapshot.
    pub fn tick_status(&mut self) -> bool {
        let mut changed = false;
        if self.last_status_check.elapsed() >= Duration::from_millis(500) {
            self.last_status_check = Instant::now();
            if let Ok(guard) = self.shared_status.lock() {
                if *guard != self.current_status {
                    self.current_status = guard.clone();
                    self.dirty = true;
                    changed = true;
                }
            }
        }
        changed
    }

    /// Wi-Fi & Bluetooth pills (subprocess-backed, so polled less often).
    fn refresh_radios(snap: &mut SystemStatusSnapshot) {
        (snap.wifi_label, snap.wifi_connected) = Self::query_wifi_status();
        (snap.bt_label, snap.bt_active) = Self::query_bluetooth_status();
    }

    /// Volume pill and `/proc` / `/sys` metrics.
    fn refresh_fast(snap: &mut SystemStatusSnapshot) {
        (snap.volume_label, snap.volume_unmuted) = Self::query_volume();
        snap.sys_metrics = Self::collect_fast_metrics();
    }

    fn query_volume() -> (String, bool) {
        // `wpctl get-volume` prints e.g. `Volume: 0.45` or `Volume: 0.45 [MUTED]`
        let Some(out) = command_output_timeout("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])
        else {
            return ("VOL N/A".to_string(), false);
        };
        let text = String::from_utf8_lossy(&out.stdout);
        let Some(level) = text
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse::<f64>().ok())
        else {
            return ("VOL N/A".to_string(), false);
        };
        let percent = (level * 100.0).round() as i64;
        if text.contains("[MUTED]") {
            (format!("VOL Mute {}%", percent), false)
        } else {
            (format!("VOL {}%", percent), true)
        }
    }

    fn query_wifi_status() -> (String, bool) {
        // Fast direct sysfs check for rfkill state
        if rfkill_soft_blocked("wlan") {
            return ("WIFI: Off".to_string(), false);
        }

        if let Some(out) = command_output_timeout(
            "nmcli",
            &["-t", "-f", "TYPE,STATE,CONNECTION", "device", "status"],
        ) {
            return wifi_label_from_nmcli(&String::from_utf8_lossy(&out.stdout));
        }
        ("WIFI: N/A".to_string(), false)
    }

    fn query_bluetooth_status() -> (String, bool) {
        if rfkill_soft_blocked("bluetooth") {
            return ("BT: Off".to_string(), false);
        }

        // `bluetoothctl` waits forever when bluetoothd is down, hence the timeout.
        let Some(show_out) = command_output_timeout("bluetoothctl", &["show"]) else {
            return ("BT: N/A".to_string(), false);
        };
        let show_txt = String::from_utf8_lossy(&show_out.stdout);
        let powered = show_txt
            .lines()
            .any(|l| l.trim().eq_ignore_ascii_case("Powered: yes"));
        if !powered {
            return ("BT: Off".to_string(), false);
        }

        if let Some(conn_out) = command_output_timeout("bluetoothctl", &["devices", "Connected"]) {
            let conn_txt = String::from_utf8_lossy(&conn_out.stdout);
            if let Some(first_line) = conn_txt.lines().next() {
                let mut parts = first_line.splitn(3, ' ');
                let _ = parts.next();
                let _ = parts.next();
                if let Some(name) = parts.next() {
                    let short_name: String = name.trim().chars().take(12).collect();
                    if !short_name.is_empty() {
                        return (format!("BT: {}", short_name), true);
                    }
                }
            }
        }
        ("BT: On".to_string(), true)
    }

    fn collect_fast_metrics() -> String {
        // 1. CPU utilisation since the previous sample, from /proc/stat
        let cpu = cpu_usage_percent()
            .map(|p| format!("{:>2}%", p))
            .unwrap_or_else(|| "--%".to_string());

        // 2. Direct /proc/meminfo read
        let mem_str = fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|content| {
                let mut total_kb = 0u64;
                let mut avail_kb = 0u64;
                for line in content.lines() {
                    if let Some(rest) = line.strip_prefix("MemTotal:") {
                        total_kb = rest
                            .split_whitespace()
                            .next()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                    } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
                        avail_kb = rest
                            .split_whitespace()
                            .next()
                            .and_then(|v| v.parse().ok())
                            .unwrap_or(0);
                    }
                }
                if total_kb > 0 {
                    let used_gb = (total_kb.saturating_sub(avail_kb) as f64) / (1024.0 * 1024.0);
                    Some(format!("{:.1}G", used_gb))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "N/A".to_string());

        let bat_prefix = battery_prefix();

        // 4. Local wall-clock time via libc::localtime_r
        let clock = Self::format_local_time();

        format!(
            "{}CPU {}  |  MEM {}  |  {}",
            bat_prefix, cpu, mem_str, clock
        )
    }

    pub(crate) fn format_local_time() -> String {
        unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&now, &mut tm).is_null() {
                return "Jan 01 00:00".to_string();
            }
            let months = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let mon = months
                .get(tm.tm_mon.clamp(0, 11) as usize)
                .copied()
                .unwrap_or("Oct");
            format!(
                "{} {:02} {:02}:{:02}",
                mon, tm.tm_mday, tm.tm_hour, tm.tm_min
            )
        }
    }

    /// Map an X coordinate (on the bar) to a clickable bar region.
    /// `x` is in logical coordinates (like the pointer); the bar itself is laid out in pixels.
    pub fn hit_test(&self, x: i32) -> BarClickAction {
        let x = (x as f64 * self.scale) as i32;
        let tags_total_w = self.tag_cell_width * TAG_NAMES.len() as i32;
        if x >= 0 && x < tags_total_w {
            let idx = (x / self.tag_cell_width) as usize;
            return BarClickAction::Tag(idx.min(TAG_NAMES.len() - 1));
        }
        if x >= tags_total_w && x < self.layout_cell_end {
            return BarClickAction::CycleLayout;
        }
        if self.wifi_pill_range.1 > self.wifi_pill_range.0
            && x >= self.wifi_pill_range.0
            && x < self.wifi_pill_range.1
        {
            return BarClickAction::Wifi;
        }
        if self.bt_pill_range.1 > self.bt_pill_range.0
            && x >= self.bt_pill_range.0
            && x < self.bt_pill_range.1
        {
            return BarClickAction::Bluetooth;
        }
        if self.vol_pill_range.1 > self.vol_pill_range.0
            && x >= self.vol_pill_range.0
            && x < self.vol_pill_range.1
        {
            return BarClickAction::Volume;
        }
        if self.metrics_range.1 > self.metrics_range.0
            && x >= self.metrics_range.0
            && x < self.metrics_range.1
        {
            return BarClickAction::SystemMonitor;
        }
        if x >= self.flameshot_pill_range.0 && x < self.flameshot_pill_range.1 {
            return BarClickAction::Flameshot;
        }
        BarClickAction::FocusNext
    }

    /// Produce a [`MemoryRenderBufferRenderElement`] for the current frame, re-rasterizing only if dirty.
    #[allow(clippy::too_many_arguments)]
    pub fn render_element<R>(
        &mut self,
        renderer: &mut R,
        output_width: i32,
        selected_tags: u16,
        occupied_tags: u16,
        focused_tags: u16,
        urgent_tags: u16,
        layout: LayoutMode,
        focused_title: &str,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        let target_w = output_width.max(320);
        if target_w != self.width {
            self.width = target_w;
            self.dirty = true;
        }

        // Client commits trigger frames constantly (video, animations); only re-rasterize
        // the bar when something it shows actually changed. Status snapshots are synced
        // by the main loop's `tick_status` call before rendering.
        let inputs = BarInputs {
            selected_tags,
            occupied_tags,
            focused_tags,
            urgent_tags,
            layout,
            focused_title: focused_title.to_string(),
        };
        if self.last_inputs.as_ref() != Some(&inputs) {
            self.last_inputs = Some(inputs);
            self.dirty = true;
        }

        if self.dirty {
            self.redraw_software_buffer(
                selected_tags,
                occupied_tags,
                focused_tags,
                urgent_tags,
                layout,
                focused_title,
            );
            self.dirty = false;
        }

        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            Point::from((0.0, 0.0)),
            &self.buffer,
            Some(1.0),
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }

    #[allow(clippy::too_many_arguments)]
    fn redraw_software_buffer(
        &mut self,
        selected_tags: u16,
        occupied_tags: u16,
        focused_tags: u16,
        urgent_tags: u16,
        layout: LayoutMode,
        focused_title: &str,
    ) {
        let snap = self.current_status.clone();
        for text in [
            focused_title,
            &snap.wifi_label,
            &snap.bt_label,
            &snap.volume_label,
        ] {
            self.cache_glyphs(text);
        }

        let w = self.width;
        let h = self.height;
        let font_px = self.font_px;
        let scale = self.scale;
        let px = |v: i32| ((v as f64) * scale).round() as i32;
        let font_ref = self.font.as_ref();
        let glyph_cache = &self.glyph_cache;
        let tag_w = self.tag_cell_width;
        let text_y = ((h - font_px as i32) / 2).max(px(4));
        let mut layout_end_out = self.layout_cell_end;
        let mut wifi_range_out = (0, 0);
        let mut bt_range_out = (0, 0);
        let mut vol_range_out = (0, 0);
        let mut flameshot_range_out = (0, 0);
        let mut metrics_range_out = (0, 0);

        let mut ctx = self.buffer.render();
        ctx.resize((w, h));

        let _ = ctx.draw(|buf| {
            // 1. Fill base smoked translucent purple background
            fill_rect(buf, w, h, 0, 0, w, h, BAR_BG_RGBA);

            // 2. Draw 1px vibrant purple bottom border line
            fill_rect(buf, w, h, 0, h - 1, w, 1, BAR_ACCENT_LINE_RGBA);

            // 3. Draw 9 Tag cells (`1..9`)
            let mut cur_x = 0;
            for (i, label) in TAG_NAMES.iter().enumerate() {
                let mask = 1u16 << i;
                let is_selected = (selected_tags & mask) != 0;
                let is_occupied = (occupied_tags & mask) != 0;
                let has_focused = (focused_tags & mask) != 0;
                let is_urgent = (urgent_tags & mask) != 0;

                if is_urgent {
                    fill_rect(buf, w, h, cur_x, 0, tag_w, h - 1, [0xEC, 0x48, 0x99, 240]);
                } else if is_selected {
                    fill_rect(buf, w, h, cur_x, 0, tag_w, h - 1, BAR_TAG_ACTIVE_RGBA);
                }

                // Classic dwm top-left occupancy square
                if is_occupied {
                    let sq_color = if is_selected || is_urgent {
                        BAR_TEXT_FG_RGBA
                    } else {
                        BAR_ACCENT_RGBA
                    };
                    if has_focused {
                        fill_rect(buf, w, h, cur_x + px(4), px(4), px(5), px(5), sq_color);
                    } else {
                        fill_rect(buf, w, h, cur_x + px(4), px(4), px(5), px(1), sq_color);
                        fill_rect(buf, w, h, cur_x + px(4), px(8), px(5), px(1), sq_color);
                        fill_rect(buf, w, h, cur_x + px(4), px(4), px(1), px(5), sq_color);
                        fill_rect(buf, w, h, cur_x + px(8), px(4), px(1), px(5), sq_color);
                    }
                }

                let text_color = if is_selected || is_occupied || is_urgent {
                    BAR_TEXT_FG_RGBA
                } else {
                    BAR_TEXT_DIM_RGBA
                };

                let text_w = measure_text(font_ref, glyph_cache, label, font_px);
                let tx = cur_x + (tag_w - text_w) / 2;
                draw_text(
                    buf,
                    w,
                    h,
                    font_ref,
                    glyph_cache,
                    label,
                    tx,
                    text_y,
                    font_px,
                    text_color,
                );

                cur_x += tag_w;
            }

            // 4. Draw Layout indicator pill (`[]=`, `[M]`, `|M|`, `###`, `><>`)
            let sym = layout.symbol();
            let sym_w = measure_text(font_ref, glyph_cache, sym, font_px) + px(20);
            fill_rect(buf, w, h, cur_x, 0, sym_w, h - 1, BAR_LAYOUT_BG_RGBA);
            draw_text(
                buf,
                w,
                h,
                font_ref,
                glyph_cache,
                sym,
                cur_x + px(10),
                text_y,
                font_px,
                BAR_ACCENT_RGBA,
            );
            cur_x += sym_w;
            layout_end_out = cur_x;

            // 5. Right-aligned system metrics + Clickable Bluetooth Pill + Clickable Wi-Fi Pill
            let metrics_w =
                measure_text(font_ref, glyph_cache, &snap.sys_metrics, font_px) + px(18);
            let mut right_cursor = w - metrics_w;
            draw_text(
                buf,
                w,
                h,
                font_ref,
                glyph_cache,
                &snap.sys_metrics,
                right_cursor + px(6),
                text_y,
                font_px,
                BAR_TEXT_FG_RGBA,
            );
            metrics_range_out = (right_cursor, w);

            // Volume clickable pill (`[ VOL 45% ]`): click mutes, scroll adjusts, right-click mixer
            let vol_text_w = measure_text(font_ref, glyph_cache, &snap.volume_label, font_px);
            let vol_pill_w = vol_text_w + px(20);
            right_cursor -= vol_pill_w + px(8);
            if right_cursor > cur_x + px(40) {
                let vol_fg = if snap.volume_unmuted {
                    BAR_TEXT_FG_RGBA
                } else {
                    BAR_TEXT_DIM_RGBA
                };
                fill_rect(
                    buf,
                    w,
                    h,
                    right_cursor,
                    px(3),
                    vol_pill_w,
                    h - px(7),
                    BAR_LAYOUT_BG_RGBA,
                );
                draw_text(
                    buf,
                    w,
                    h,
                    font_ref,
                    glyph_cache,
                    &snap.volume_label,
                    right_cursor + px(10),
                    text_y,
                    font_px,
                    vol_fg,
                );
                vol_range_out = (right_cursor, right_cursor + vol_pill_w);
            }

            // Bluetooth clickable pill (`[ BT: ... ]`)
            let bt_text_w = measure_text(font_ref, glyph_cache, &snap.bt_label, font_px);
            let bt_pill_w = bt_text_w + px(20);
            right_cursor -= bt_pill_w + px(8);
            if right_cursor > cur_x + px(40) {
                let bt_bg = if snap.bt_active {
                    [0x4C, 0x1D, 0x95, 225]
                } else {
                    BAR_LAYOUT_BG_RGBA
                };
                let bt_fg = if snap.bt_active {
                    BAR_TEXT_FG_RGBA
                } else {
                    BAR_TEXT_DIM_RGBA
                };
                fill_rect(buf, w, h, right_cursor, px(3), bt_pill_w, h - px(7), bt_bg);
                draw_text(
                    buf,
                    w,
                    h,
                    font_ref,
                    glyph_cache,
                    &snap.bt_label,
                    right_cursor + px(10),
                    text_y,
                    font_px,
                    bt_fg,
                );
                bt_range_out = (right_cursor, right_cursor + bt_pill_w);
            }

            // Wi-Fi clickable pill (`[ WIFI: ... ]`)
            let wifi_text_w = measure_text(font_ref, glyph_cache, &snap.wifi_label, font_px);
            let wifi_pill_w = wifi_text_w + px(20);
            right_cursor -= wifi_pill_w + px(8);
            if right_cursor > cur_x + px(40) {
                let wifi_bg = if snap.wifi_connected {
                    BAR_TAG_ACTIVE_RGBA
                } else {
                    BAR_LAYOUT_BG_RGBA
                };
                let wifi_fg = if snap.wifi_connected {
                    BAR_TEXT_FG_RGBA
                } else {
                    BAR_TEXT_DIM_RGBA
                };
                fill_rect(
                    buf,
                    w,
                    h,
                    right_cursor,
                    px(3),
                    wifi_pill_w,
                    h - px(7),
                    wifi_bg,
                );
                draw_text(
                    buf,
                    w,
                    h,
                    font_ref,
                    glyph_cache,
                    &snap.wifi_label,
                    right_cursor + px(10),
                    text_y,
                    font_px,
                    wifi_fg,
                );
                wifi_range_out = (right_cursor, right_cursor + wifi_pill_w);
            }

            // Flameshot screenshot button, beside the Wi-Fi pill.
            let flameshot_w = measure_text(font_ref, glyph_cache, "Flameshot", font_px) + px(20);
            let flameshot_x = right_cursor - flameshot_w - px(8);
            if flameshot_x > cur_x + px(40) {
                right_cursor = flameshot_x;
                fill_rect(
                    buf,
                    w,
                    h,
                    right_cursor,
                    px(3),
                    flameshot_w,
                    h - px(7),
                    BAR_LAYOUT_BG_RGBA,
                );
                draw_text(
                    buf,
                    w,
                    h,
                    font_ref,
                    glyph_cache,
                    "Flameshot",
                    right_cursor + px(10),
                    text_y,
                    font_px,
                    BAR_ACCENT_RGBA,
                );
                flameshot_range_out = (right_cursor, right_cursor + flameshot_w);
            }

            // 6. Draw active window title in the middle section
            let title_avail_w = (right_cursor - cur_x - px(24)).max(0);
            if title_avail_w > px(32) && !focused_title.is_empty() {
                let clipped = clip_text_to_width(
                    font_ref,
                    glyph_cache,
                    focused_title,
                    font_px,
                    title_avail_w,
                );
                draw_text(
                    buf,
                    w,
                    h,
                    font_ref,
                    glyph_cache,
                    &clipped,
                    cur_x + px(12),
                    text_y,
                    font_px,
                    BAR_TEXT_FG_RGBA,
                );
            }

            Result::<Vec<Rectangle<i32, Buffer>>, ()>::Ok(vec![Rectangle::from_size((w, h).into())])
        });
        drop(ctx);
        self.layout_cell_end = layout_end_out;
        self.wifi_pill_range = wifi_range_out;
        self.bt_pill_range = bt_range_out;
        self.vol_pill_range = vol_range_out;
        self.flameshot_pill_range = flameshot_range_out;
        self.metrics_range = metrics_range_out;
    }

    /// Rasterize any glyphs of `text` not cached yet, so bar redraws never re-rasterize.
    fn cache_glyphs(&mut self, text: &str) {
        let Some(font) = self.font.as_ref() else {
            return;
        };
        let font_px = self.font_px;
        for ch in text.chars() {
            if self.glyph_cache.len() >= GLYPH_CACHE_LIMIT {
                return;
            }
            self.glyph_cache.entry(ch).or_insert_with(|| {
                let (metrics, bitmap) = font.rasterize(ch, font_px);
                CachedGlyph { metrics, bitmap }
            });
        }
    }
}

/// Last-resort status bar font, compiled in (see `assets/fonts/DejaVuSansMono-LICENSE`).
pub(crate) const BUNDLED_FONT: &[u8] = include_bytes!("../assets/fonts/DejaVuSansMono.ttf");

/// The system's monospace font according to fontconfig, if `fc-match` is installed.
fn fontconfig_monospace() -> Option<String> {
    let output = Command::new("fc-match")
        .args(["-f", "%{file}", "monospace"])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let path = String::from_utf8(output.stdout).ok()?;
    (output.status.success() && !path.is_empty()).then_some(path)
}

/// Prefer a connected wifi device over an earlier disconnected adapter.
/// `text` is `nmcli -t -f TYPE,STATE,CONNECTION device status`.
pub(crate) fn wifi_label_from_nmcli(text: &str) -> (String, bool) {
    let mut fallback: Option<(String, bool)> = None;
    for line in text.lines() {
        let mut parts = line.splitn(3, ':');
        let dev_type = parts.next().unwrap_or("");
        let state = parts.next().unwrap_or("").trim();
        let conn = parts.next().unwrap_or("").trim();
        if dev_type != "wifi" {
            continue;
        }
        if state == "connected" && !conn.is_empty() {
            let short_ssid: String = conn.chars().take(14).collect();
            return (format!("WIFI: {short_ssid}"), true);
        } else if state.starts_with("connecting") {
            fallback = Some(("WIFI: ...".to_string(), false));
        } else if fallback.is_none() {
            fallback = Some(("WIFI: Disconn".to_string(), false));
        }
    }
    fallback.unwrap_or_else(|| ("WIFI: N/A".to_string(), false))
}

/// First battery under `/sys/class/power_supply`, not only `BAT0`/`BAT1`.
fn battery_prefix() -> String {
    let Ok(entries) = fs::read_dir("/sys/class/power_supply") else {
        return String::new();
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        let kind = fs::read_to_string(path.join("type")).unwrap_or_default();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let looks_like_battery =
            kind.trim().eq_ignore_ascii_case("Battery") || name.to_ascii_lowercase().starts_with("bat");
        if !looks_like_battery {
            continue;
        }
        let Ok(cap_s) = fs::read_to_string(path.join("capacity")) else {
            continue;
        };
        let status_s = fs::read_to_string(path.join("status")).unwrap_or_default();
        let is_charging = status_s.trim().eq_ignore_ascii_case("charging");
        let sym = if is_charging { "+" } else { "" };
        return format!("BAT {}{}%  |  ", sym, cap_s.trim());
    }
    String::new()
}

/// Whether any rfkill switch of `kind` (`wlan`, `bluetooth`) is soft-blocked.
/// rfkill indices are assigned at boot, so match on the `type` file instead of the number.
fn rfkill_soft_blocked(kind: &str) -> bool {
    let Ok(entries) = fs::read_dir("/sys/class/rfkill") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        fs::read_to_string(path.join("type")).is_ok_and(|t| t.trim() == kind)
            && fs::read_to_string(path.join("soft")).is_ok_and(|s| s.trim() == "1")
    })
}

/// Run a status command, giving up (and killing it) after 3 seconds.
fn command_output_timeout(program: &str, args: &[&str]) -> Option<Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Busy CPU percentage across all cores since the previous call (`None` on the first sample).
fn cpu_usage_percent() -> Option<u64> {
    static PREV: Mutex<Option<(u64, u64)>> = Mutex::new(None);

    let stat = fs::read_to_string("/proc/stat").ok()?;
    let fields: Vec<u64> = stat
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse().ok())
        .collect();
    // user nice system idle iowait irq softirq steal
    let idle = fields.get(3)? + fields.get(4).copied().unwrap_or(0);
    let total: u64 = fields.iter().take(8).sum();

    let mut prev = PREV.lock().ok()?;
    let result = prev.and_then(|(prev_idle, prev_total)| {
        let d_total = total.checked_sub(prev_total)?;
        let d_idle = idle.checked_sub(prev_idle)?;
        (d_total > 0).then(|| (d_total - d_idle.min(d_total)) * 100 / d_total)
    });
    *prev = Some((idle, total));
    result
}

/// Write a premultiplied `Fourcc::Argb8888` pixel (little-endian `[B, G, R, A]`) into `buf`.
#[inline]
fn put_pixel_argb8888(buf: &mut [u8], stride_w: i32, h: i32, x: i32, y: i32, rgba: [u8; 4]) {
    if x < 0 || y < 0 || x >= stride_w || y >= h {
        return;
    }
    let idx = ((y * stride_w + x) as usize) * 4;
    if idx + 3 >= buf.len() {
        return;
    }
    let a = rgba[3] as u16;
    let r = ((rgba[0] as u16 * a) / 255) as u8;
    let g = ((rgba[1] as u16 * a) / 255) as u8;
    let b = ((rgba[2] as u16 * a) / 255) as u8;
    buf[idx] = b;
    buf[idx + 1] = g;
    buf[idx + 2] = r;
    buf[idx + 3] = rgba[3];
}

/// Alpha-blend a foreground glyph pixel onto an existing premultiplied `Argb8888` pixel.
#[inline]
pub(crate) fn blend_pixel_argb8888(
    buf: &mut [u8],
    stride_w: i32,
    h: i32,
    x: i32,
    y: i32,
    fg_rgba: [u8; 4],
    coverage: u8,
) {
    if x < 0 || y < 0 || x >= stride_w || y >= h || coverage == 0 {
        return;
    }
    let idx = ((y * stride_w + x) as usize) * 4;
    if idx + 3 >= buf.len() {
        return;
    }
    let src_a = ((fg_rgba[3] as u32) * (coverage as u32)) / 255;
    if src_a == 0 {
        return;
    }
    let inv_a = 255 - src_a;
    let src_r = ((fg_rgba[0] as u32) * src_a) / 255;
    let src_g = ((fg_rgba[1] as u32) * src_a) / 255;
    let src_b = ((fg_rgba[2] as u32) * src_a) / 255;

    let dst_b = buf[idx] as u32;
    let dst_g = buf[idx + 1] as u32;
    let dst_r = buf[idx + 2] as u32;
    let dst_a = buf[idx + 3] as u32;

    buf[idx] = (src_b + (dst_b * inv_a) / 255).min(255) as u8;
    buf[idx + 1] = (src_g + (dst_g * inv_a) / 255).min(255) as u8;
    buf[idx + 2] = (src_r + (dst_r * inv_a) / 255).min(255) as u8;
    buf[idx + 3] = (src_a + (dst_a * inv_a) / 255).min(255) as u8;
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn fill_rect(
    buf: &mut [u8],
    stride_w: i32,
    buf_h: i32,
    rx: i32,
    ry: i32,
    rw: i32,
    rh: i32,
    rgba: [u8; 4],
) {
    let x0 = rx.clamp(0, stride_w);
    let y0 = ry.clamp(0, buf_h);
    let x1 = (rx + rw).clamp(0, stride_w);
    let y1 = (ry + rh).clamp(0, buf_h);
    for y in y0..y1 {
        for x in x0..x1 {
            put_pixel_argb8888(buf, stride_w, buf_h, x, y, rgba);
        }
    }
}

fn measure_text(
    font: Option<&Font>,
    glyph_cache: &HashMap<char, CachedGlyph>,
    text: &str,
    px: f32,
) -> i32 {
    if let Some(f) = font {
        text.chars()
            .map(|c| {
                if let Some(cached) = glyph_cache.get(&c) {
                    cached.metrics.advance_width.ceil() as i32
                } else {
                    f.metrics(c, px).advance_width.ceil() as i32
                }
            })
            .sum()
    } else {
        (text.chars().count() as i32) * 8
    }
}

fn clip_text_to_width(
    font: Option<&Font>,
    glyph_cache: &HashMap<char, CachedGlyph>,
    text: &str,
    px: f32,
    max_w: i32,
) -> String {
    let mut acc = 0;
    let mut out = String::new();
    for ch in text.chars() {
        let cw = if let Some(cached) = glyph_cache.get(&ch) {
            cached.metrics.advance_width.ceil() as i32
        } else if let Some(f) = font {
            f.metrics(ch, px).advance_width.ceil() as i32
        } else {
            8
        };
        if acc + cw > max_w {
            break;
        }
        acc += cw;
        out.push(ch);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    buf: &mut [u8],
    stride_w: i32,
    buf_h: i32,
    font: Option<&Font>,
    glyph_cache: &HashMap<char, CachedGlyph>,
    text: &str,
    start_x: i32,
    start_y: i32,
    px: f32,
    fg_rgba: [u8; 4],
) {
    let Some(f) = font else {
        return;
    };

    let mut pen_x = start_x;
    let baseline_y = start_y + (px as i32) - 2;

    for ch in text.chars() {
        if let Some(cached) = glyph_cache.get(&ch) {
            let metrics = &cached.metrics;
            let gx = pen_x + metrics.xmin;
            let gy = baseline_y - metrics.height as i32 - metrics.ymin;

            for row in 0..metrics.height {
                for col in 0..metrics.width {
                    let cov = cached.bitmap[row * metrics.width + col];
                    if cov > 0 {
                        blend_pixel_argb8888(
                            buf,
                            stride_w,
                            buf_h,
                            gx + col as i32,
                            gy + row as i32,
                            fg_rgba,
                            cov,
                        );
                    }
                }
            }
            pen_x += metrics.advance_width.ceil() as i32;
        } else {
            let (metrics, bitmap) = f.rasterize(ch, px);
            let gx = pen_x + metrics.xmin;
            let gy = baseline_y - metrics.height as i32 - metrics.ymin;

            for row in 0..metrics.height {
                for col in 0..metrics.width {
                    let cov = bitmap[row * metrics.width + col];
                    if cov > 0 {
                        blend_pixel_argb8888(
                            buf,
                            stride_w,
                            buf_h,
                            gx + col as i32,
                            gy + row as i32,
                            fg_rgba,
                            cov,
                        );
                    }
                }
            }
            pen_x += metrics.advance_width.ceil() as i32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_refresh_is_single_flight() {
        let mut bar = StatusBar::new(800);
        assert!(bar.request_immediate_status_refresh());
        // A second call while one refresh is pending is coalesced, not re-armed.
        assert!(!bar.request_immediate_status_refresh());
        let pending = bar
            .status_request
            .0
            .lock()
            .map(|request| request.pending)
            .expect("request lock");
        assert!(pending.is_some());
    }

    #[test]
    fn immediate_refresh_rearms_once_the_pending_one_is_gone() {
        let mut bar = StatusBar::new(800);
        assert!(bar.request_immediate_status_refresh());
        // Simulate the status thread consuming the pending refresh.
        bar.status_request.0.lock().expect("request lock").pending = None;
        assert!(bar.request_immediate_status_refresh());
    }
}
