//! Built-in `dwm`-style translucent purple status bar rendered with `fontdue` into a
//! cached `MemoryRenderBuffer` (re-rasterized only when state changes), with an
//! O(1) pre-rasterized ASCII glyph cache, non-blocking Wi-Fi & Bluetooth status pills,
//! direct Linux `/proc` parsing (no external subprocesses), and a high-visibility cursor texture.

use std::{
    collections::HashMap,
    fs,
    process::Command,
    sync::{Arc, Mutex},
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
    sys_metrics: String,
}

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
    dirty: bool,
    last_status_check: Instant,
    shared_status: Arc<Mutex<SystemStatusSnapshot>>,
    current_status: SystemStatusSnapshot,
    tag_cell_width: i32,
    layout_cell_end: i32,
    wifi_pill_range: (i32, i32),
    bt_pill_range: (i32, i32),
    metrics_range: (i32, i32),
}

impl StatusBar {
    pub fn new(initial_width: i32) -> Self {
        let width = initial_width.max(320);
        let height = BAR_HEIGHT;
        let font = Self::load_system_font();
        let mut glyph_cache = HashMap::with_capacity(128);

        // Pre-rasterize printable ASCII characters (`32..=126`) at startup for O(1) bar redraws
        if let Some(ref f) = font {
            for b in 32u8..=126u8 {
                let ch = b as char;
                let (metrics, bitmap) = f.rasterize(ch, BAR_FONT_SIZE);
                glyph_cache.insert(ch, CachedGlyph { metrics, bitmap });
            }
        }

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
            sys_metrics: Self::collect_fast_metrics(),
        };
        let shared_status = Arc::new(Mutex::new(initial_snapshot.clone()));

        // Spawn a background daemon thread to poll Wi-Fi (nmcli), Bluetooth (bluetoothctl),
        // battery, CPU, memory, and clock every 2 seconds without ever blocking the compositor frame loop.
        let bg_shared = Arc::clone(&shared_status);
        std::thread::spawn(move || loop {
            let snap = Self::collect_full_snapshot();
            if let Ok(mut guard) = bg_shared.lock() {
                *guard = snap;
            }
            std::thread::sleep(Duration::from_secs(2));
        });

        Self {
            font,
            glyph_cache,
            buffer,
            cursor_buffer,
            width,
            height,
            dirty: true,
            last_status_check: Instant::now() - Duration::from_secs(10),
            shared_status,
            current_status: initial_snapshot,
            tag_cell_width: 34,
            layout_cell_end: 34 * 9 + 52,
            wifi_pill_range: (0, 0),
            bt_pill_range: (0, 0),
            metrics_range: (0, 0),
        }
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

    fn load_system_font() -> Option<Font> {
        for path in FONT_PATHS {
            if let Ok(bytes) = fs::read(path) {
                if let Ok(font) = Font::from_bytes(bytes, FontSettings::default()) {
                    tracing::info!("Loaded status bar font: {}", path);
                    return Some(font);
                }
            }
        }
        tracing::warn!("No TTF font found in FONT_PATHS; using built-in fallback");
        None
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Trigger an immediate asynchronous refresh of the Wi-Fi & Bluetooth status pills.
    pub fn request_immediate_status_refresh(&mut self) {
        let bg_shared = Arc::clone(&self.shared_status);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(350));
            let snap = Self::collect_full_snapshot();
            if let Ok(mut guard) = bg_shared.lock() {
                *guard = snap;
            }
        });
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

    fn collect_full_snapshot() -> SystemStatusSnapshot {
        let (wifi_label, wifi_connected) = Self::query_wifi_status();
        let (bt_label, bt_active) = Self::query_bluetooth_status();
        let sys_metrics = Self::collect_fast_metrics();
        SystemStatusSnapshot {
            wifi_label,
            wifi_connected,
            bt_label,
            bt_active,
            sys_metrics,
        }
    }

    fn query_wifi_status() -> (String, bool) {
        // Fast direct sysfs check for rfkill state
        let rfkill_blocked = fs::read_to_string("/sys/class/rfkill/rfkill1/soft")
            .or_else(|_| fs::read_to_string("/sys/class/rfkill/rfkill3/soft"))
            .map(|s| s.trim() == "1")
            .unwrap_or(false);
        if rfkill_blocked {
            return ("WIFI: Off".to_string(), false);
        }

        if let Ok(out) = Command::new("nmcli")
            .args(["-t", "-f", "TYPE,STATE,CONNECTION", "device", "status"])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                let mut parts = line.splitn(3, ':');
                let dev_type = parts.next().unwrap_or("");
                let state = parts.next().unwrap_or("");
                let conn = parts.next().unwrap_or("").trim();
                if dev_type == "wifi" {
                    if state == "connected" && !conn.is_empty() {
                        let short_ssid: String = conn.chars().take(14).collect();
                        return (format!("WIFI: {}", short_ssid), true);
                    } else if state.starts_with("connecting") {
                        return ("WIFI: ...".to_string(), false);
                    } else {
                        return ("WIFI: Disconn".to_string(), false);
                    }
                }
            }
        }
        ("WIFI: N/A".to_string(), false)
    }

    fn query_bluetooth_status() -> (String, bool) {
        let bt_blocked = fs::read_to_string("/sys/class/rfkill/rfkill0/soft")
            .or_else(|_| fs::read_to_string("/sys/class/rfkill/rfkill2/soft"))
            .map(|s| s.trim() == "1")
            .unwrap_or(false);
        if bt_blocked {
            return ("BT: Off".to_string(), false);
        }

        let Ok(show_out) = Command::new("bluetoothctl").arg("show").output() else {
            return ("BT: N/A".to_string(), false);
        };
        let show_txt = String::from_utf8_lossy(&show_out.stdout);
        let powered = show_txt
            .lines()
            .any(|l| l.trim().eq_ignore_ascii_case("Powered: yes"));
        if !powered {
            return ("BT: Off".to_string(), false);
        }

        if let Ok(conn_out) = Command::new("bluetoothctl")
            .args(["devices", "Connected"])
            .output()
        {
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
        // 1. Direct /proc/loadavg read
        let load = fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next().map(String::from))
            .unwrap_or_else(|| "0.00".to_string());

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

        // 3. Laptop Battery percentage from /sys/class/power_supply/BAT*
        let bat_opt = ["BAT0", "BAT1"].iter().find_map(|bat| {
            let cap_path = format!("/sys/class/power_supply/{}/capacity", bat);
            let status_path = format!("/sys/class/power_supply/{}/status", bat);
            if let Ok(cap_s) = fs::read_to_string(cap_path) {
                let status_s = fs::read_to_string(status_path).unwrap_or_default();
                let is_charging = status_s.trim().eq_ignore_ascii_case("charging");
                let sym = if is_charging { "+" } else { "" };
                Some(format!("BAT {}{}%  |  ", sym, cap_s.trim()))
            } else {
                None
            }
        });
        let bat_prefix = bat_opt.unwrap_or_default();

        // 4. Local wall-clock time via libc::localtime_r
        let clock = Self::format_local_time();

        format!("{}CPU {}  |  MEM {}  |  {}", bat_prefix, load, mem_str, clock)
    }

    fn format_local_time() -> String {
        unsafe {
            let now = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            libc::localtime_r(&now, &mut tm);
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
    pub fn hit_test(&self, x: i32) -> BarClickAction {
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
        if self.metrics_range.1 > self.metrics_range.0
            && x >= self.metrics_range.0
            && x < self.metrics_range.1
        {
            return BarClickAction::SystemMonitor;
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

        self.tick_status();

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
        let w = self.width;
        let h = self.height;
        let font_ref = self.font.as_ref();
        let glyph_cache = &self.glyph_cache;
        let snap = self.current_status.clone();
        let tag_w = self.tag_cell_width;
        let text_y = ((h - BAR_FONT_SIZE as i32) / 2).max(4);
        let mut layout_end_out = self.layout_cell_end;
        let mut wifi_range_out = (0, 0);
        let mut bt_range_out = (0, 0);
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
                        fill_rect(buf, w, h, cur_x + 4, 4, 5, 5, sq_color);
                    } else {
                        fill_rect(buf, w, h, cur_x + 4, 4, 5, 1, sq_color);
                        fill_rect(buf, w, h, cur_x + 4, 8, 5, 1, sq_color);
                        fill_rect(buf, w, h, cur_x + 4, 4, 1, 5, sq_color);
                        fill_rect(buf, w, h, cur_x + 8, 4, 1, 5, sq_color);
                    }
                }

                let text_color = if is_selected || is_occupied || is_urgent {
                    BAR_TEXT_FG_RGBA
                } else {
                    BAR_TEXT_DIM_RGBA
                };

                let text_w = measure_text(font_ref, glyph_cache, label, BAR_FONT_SIZE);
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
                    BAR_FONT_SIZE,
                    text_color,
                );

                cur_x += tag_w;
            }

            // 4. Draw Layout indicator pill (`[]=`, `[M]`, `|M|`, `###`, `><>`)
            let sym = layout.symbol();
            let sym_w = measure_text(font_ref, glyph_cache, sym, BAR_FONT_SIZE) + 20;
            fill_rect(buf, w, h, cur_x, 0, sym_w, h - 1, BAR_LAYOUT_BG_RGBA);
            draw_text(
                buf,
                w,
                h,
                font_ref,
                glyph_cache,
                sym,
                cur_x + 10,
                text_y,
                BAR_FONT_SIZE,
                BAR_ACCENT_RGBA,
            );
            cur_x += sym_w;
            layout_end_out = cur_x;

            // 5. Right-aligned system metrics + Clickable Bluetooth Pill + Clickable Wi-Fi Pill
            let metrics_w =
                measure_text(font_ref, glyph_cache, &snap.sys_metrics, BAR_FONT_SIZE) + 18;
            let mut right_cursor = w - metrics_w;
            draw_text(
                buf,
                w,
                h,
                font_ref,
                glyph_cache,
                &snap.sys_metrics,
                right_cursor + 6,
                text_y,
                BAR_FONT_SIZE,
                BAR_TEXT_FG_RGBA,
            );
            metrics_range_out = (right_cursor, w);

            // Bluetooth clickable pill (`[ BT: ... ]`)
            let bt_text_w = measure_text(font_ref, glyph_cache, &snap.bt_label, BAR_FONT_SIZE);
            let bt_pill_w = bt_text_w + 20;
            right_cursor -= bt_pill_w + 8;
            if right_cursor > cur_x + 40 {
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
                fill_rect(buf, w, h, right_cursor, 3, bt_pill_w, h - 7, bt_bg);
                draw_text(
                    buf,
                    w,
                    h,
                    font_ref,
                    glyph_cache,
                    &snap.bt_label,
                    right_cursor + 10,
                    text_y,
                    BAR_FONT_SIZE,
                    bt_fg,
                );
                bt_range_out = (right_cursor, right_cursor + bt_pill_w);
            }

            // Wi-Fi clickable pill (`[ WIFI: ... ]`)
            let wifi_text_w =
                measure_text(font_ref, glyph_cache, &snap.wifi_label, BAR_FONT_SIZE);
            let wifi_pill_w = wifi_text_w + 20;
            right_cursor -= wifi_pill_w + 8;
            if right_cursor > cur_x + 40 {
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
                fill_rect(buf, w, h, right_cursor, 3, wifi_pill_w, h - 7, wifi_bg);
                draw_text(
                    buf,
                    w,
                    h,
                    font_ref,
                    glyph_cache,
                    &snap.wifi_label,
                    right_cursor + 10,
                    text_y,
                    BAR_FONT_SIZE,
                    wifi_fg,
                );
                wifi_range_out = (right_cursor, right_cursor + wifi_pill_w);
            }

            // 6. Draw active window title in the middle section
            let title_avail_w = (right_cursor - cur_x - 24).max(0);
            if title_avail_w > 32 && !focused_title.is_empty() {
                let clipped = clip_text_to_width(
                    font_ref,
                    glyph_cache,
                    focused_title,
                    BAR_FONT_SIZE,
                    title_avail_w,
                );
                draw_text(
                    buf,
                    w,
                    h,
                    font_ref,
                    glyph_cache,
                    &clipped,
                    cur_x + 12,
                    text_y,
                    BAR_FONT_SIZE,
                    BAR_TEXT_FG_RGBA,
                );
            }

            Result::<Vec<Rectangle<i32, Buffer>>, ()>::Ok(vec![Rectangle::from_size(
                (w, h).into(),
            )])
        });
        drop(ctx);
        self.layout_cell_end = layout_end_out;
        self.wifi_pill_range = wifi_range_out;
        self.bt_pill_range = bt_range_out;
        self.metrics_range = metrics_range_out;
    }
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
fn blend_pixel_argb8888(
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
fn fill_rect(
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
