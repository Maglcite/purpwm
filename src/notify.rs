//! Built-in Freedesktop notification daemon (`org.freedesktop.Notifications`) and translucent
//! purple toast overlay rendered directly by the compositor (`MemoryRenderBuffer`).
//!
//! No external `mako` / `dunst` / `swaync` daemon is required, and `cosmic-notifications` (which
//! depends on COSMIC-only compositor protocols) is replaced cleanly. Toasts never steal keyboard
//! focus and can be dismissed by clicking or tapping them.

use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use fontdue::Font;
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
    utils::{Buffer, Logical, Physical, Point, Rectangle, Transform},
};

use crate::{
    bar::{blend_pixel_argb8888, fill_rect},
    config::{
        BAR_ACCENT_RGBA, BAR_BG_RGBA, BAR_FONT_SIZE, BAR_HEIGHT, BAR_TAG_ACTIVE_RGBA,
        BAR_TEXT_DIM_RGBA, BAR_TEXT_FG_RGBA, GAP_OUTER,
    },
};

/// Maximum number of notification toasts visible at once.
const MAX_VISIBLE_TOASTS: usize = 4;
/// Logical width of each notification card.
const TOAST_WIDTH: i32 = 340;
/// Logical height of each notification card.
const TOAST_HEIGHT: i32 = 68;
/// Vertical gap between stacked notification cards in logical pixels.
const TOAST_GAP: i32 = 8;
/// Default notification timeout in milliseconds (5 seconds).
const DEFAULT_TIMEOUT_MS: u64 = 5000;
/// Critical urgency border colour (`#F43F5E`).
const CRITICAL_BORDER_RGBA: [u8; 4] = [0xF4, 0x3F, 0x5E, 245];

static NEXT_NOTIFICATION_ID: AtomicU32 = AtomicU32::new(1);

/// A single active desktop notification toast.
#[derive(Debug, Clone)]
pub struct Notification {
    pub id: u32,
    pub app_name: String,
    pub summary: String,
    pub body: String,
    pub urgency: u8,
    pub expires_at: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToastDrawKey {
    id: u32,
    app_name: String,
    summary: String,
    body: String,
    urgency: u8,
}

/// Manages active notification toasts, D-Bus ingestion, hit-testing, and software rasterization.
pub struct NotificationCenter {
    toasts: Vec<Notification>,
    incoming: Arc<Mutex<Vec<Notification>>>,
    buffer: MemoryRenderBuffer,
    last_drawn: Option<(i32, Vec<ToastDrawKey>)>,
}

impl Default for NotificationCenter {
    fn default() -> Self {
        Self::new()
    }
}

impl NotificationCenter {
    pub fn new() -> Self {
        Self {
            toasts: Vec::new(),
            incoming: Arc::new(Mutex::new(Vec::new())),
            buffer: MemoryRenderBuffer::new(Fourcc::Argb8888, (1, 1), 1, Transform::Normal, None),
            last_drawn: None,
        }
    }

    /// Push or replace a notification toast directly (used by D-Bus ingestion, internal alerts, and tests).
    pub fn push(
        &mut self,
        replaces_id: u32,
        app_name: impl Into<String>,
        summary: impl Into<String>,
        body: impl Into<String>,
        urgency: u8,
        timeout_ms: Option<u64>,
    ) -> u32 {
        let id = if replaces_id > 0 {
            replaces_id
        } else {
            NEXT_NOTIFICATION_ID.fetch_add(1, Ordering::Relaxed)
        };
        let duration =
            Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).clamp(1000, 30_000));
        let toast = Notification {
            id,
            app_name: sanitize_line(&app_name.into()),
            summary: sanitize_line(&summary.into()),
            body: sanitize_line(&body.into()),
            urgency,
            expires_at: Instant::now() + duration,
        };

        if let Some(existing) = self.toasts.iter_mut().find(|t| t.id == id) {
            *existing = toast;
        } else {
            self.toasts.insert(0, toast);
            if self.toasts.len() > MAX_VISIBLE_TOASTS {
                self.toasts.truncate(MAX_VISIBLE_TOASTS);
            }
        }
        self.last_drawn = None;
        id
    }

    /// Drain incoming D-Bus notifications and expire timed-out toasts.
    /// Returns `true` if the visible toast list changed.
    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        if let Ok(mut pending) = self.incoming.lock() {
            for notif in pending.drain(..) {
                if let Some(existing) = self.toasts.iter_mut().find(|t| t.id == notif.id) {
                    *existing = notif;
                } else {
                    self.toasts.insert(0, notif);
                    if self.toasts.len() > MAX_VISIBLE_TOASTS {
                        self.toasts.truncate(MAX_VISIBLE_TOASTS);
                    }
                }
                changed = true;
            }
        }

        let now = Instant::now();
        let before = self.toasts.len();
        self.toasts.retain(|t| t.expires_at > now);
        if self.toasts.len() != before {
            changed = true;
        }
        if changed {
            self.last_drawn = None;
        }
        changed
    }

    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.toasts.len()
    }

    /// Logical bounding box of the toast stack on an output of `output_width`.
    pub fn stack_geometry(
        &self,
        output_width: i32,
        bar_visible: bool,
    ) -> Option<Rectangle<i32, Logical>> {
        if self.toasts.is_empty() {
            return None;
        }
        let count = self.toasts.len() as i32;
        let top = (if bar_visible { BAR_HEIGHT } else { 0 }) + GAP_OUTER;
        let x = (output_width - TOAST_WIDTH - GAP_OUTER).max(0);
        let h = count * TOAST_HEIGHT + (count - 1).max(0) * TOAST_GAP;
        Some(Rectangle::new(
            Point::from((x, top)),
            (TOAST_WIDTH, h).into(),
        ))
    }

    /// Dismiss the toast at `point` (logical coordinates) if clicked or tapped.
    /// Returns `true` if a toast was hit and dismissed.
    pub fn dismiss_at(
        &mut self,
        point: Point<f64, Logical>,
        output_width: i32,
        bar_visible: bool,
    ) -> bool {
        let Some(stack) = self.stack_geometry(output_width, bar_visible) else {
            return false;
        };
        let p = point.to_i32_round();
        if !stack.contains(p) {
            return false;
        }
        let rel_y = p.y - stack.loc.y;
        let stride = TOAST_HEIGHT + TOAST_GAP;
        let idx = (rel_y / stride) as usize;
        let within_card = (rel_y % stride) < TOAST_HEIGHT;
        if within_card && idx < self.toasts.len() {
            self.toasts.remove(idx);
            self.last_drawn = None;
            return true;
        }
        false
    }

    /// Render the visible notification stack as a single `MemoryRenderBufferRenderElement`.
    pub fn render_element<R>(
        &mut self,
        renderer: &mut R,
        font: Option<&Font>,
        output_width_logical: i32,
        bar_visible: bool,
        scale: f64,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        let stack_geo = self.stack_geometry(output_width_logical, bar_visible)?;
        let phys_rect: Rectangle<i32, Physical> = stack_geo.to_physical_precise_round(scale);
        let scale_key = (scale * 100.0).round() as i32;
        let draw_keys: Vec<ToastDrawKey> = self
            .toasts
            .iter()
            .map(|t| ToastDrawKey {
                id: t.id,
                app_name: t.app_name.clone(),
                summary: t.summary.clone(),
                body: t.body.clone(),
                urgency: t.urgency,
            })
            .collect();

        if self.last_drawn.as_ref() != Some(&(scale_key, draw_keys.clone())) {
            self.redraw(
                font,
                phys_rect.size.w.max(1),
                phys_rect.size.h.max(1),
                scale,
            );
            self.last_drawn = Some((scale_key, draw_keys));
        }

        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            Point::<f64, Physical>::from((phys_rect.loc.x as f64, phys_rect.loc.y as f64)),
            &self.buffer,
            Some(1.0),
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }

    fn redraw(&mut self, font: Option<&Font>, w: i32, h: i32, scale: f64) {
        let px = |v: i32| ((v as f64) * scale).round() as i32;
        let card_w = w;
        let card_h = px(TOAST_HEIGHT);
        let gap = px(TOAST_GAP);
        let border = px(2).max(1);
        let title_px = (BAR_FONT_SIZE + 0.5) * scale as f32;
        let body_px = (BAR_FONT_SIZE - 1.0) * scale as f32;
        let toasts = self.toasts.clone();

        let mut ctx = self.buffer.render();
        ctx.resize((w, h));
        let _ = ctx.draw(|buf| {
            buf.fill(0);
            for (i, toast) in toasts.iter().enumerate() {
                let y0 = (i as i32) * (card_h + gap);
                let border_rgba = if toast.urgency >= 2 {
                    CRITICAL_BORDER_RGBA
                } else {
                    BAR_TAG_ACTIVE_RGBA
                };
                // Outer border + smoked purple body + left accent stripe
                fill_rect(buf, w, h, 0, y0, card_w, card_h, border_rgba);
                fill_rect(
                    buf,
                    w,
                    h,
                    border,
                    y0 + border,
                    card_w - border * 2,
                    card_h - border * 2,
                    BAR_BG_RGBA,
                );
                fill_rect(
                    buf,
                    w,
                    h,
                    border,
                    y0 + border,
                    px(4).max(2),
                    card_h - border * 2,
                    BAR_ACCENT_RGBA,
                );

                let text_x = px(14);
                let max_text_w = (card_w - text_x - px(12)).max(px(40));
                let header = if toast.app_name.is_empty() {
                    toast.summary.clone()
                } else if toast.summary.is_empty() {
                    toast.app_name.clone()
                } else {
                    format!("{} · {}", toast.app_name, toast.summary)
                };
                draw_clipped_line(
                    buf,
                    w,
                    h,
                    font,
                    &header,
                    text_x,
                    y0 + px(10),
                    title_px,
                    max_text_w,
                    BAR_TEXT_FG_RGBA,
                );
                let sub = if toast.body.is_empty() {
                    "Click or tap to dismiss"
                } else {
                    &toast.body
                };
                draw_clipped_line(
                    buf,
                    w,
                    h,
                    font,
                    sub,
                    text_x,
                    y0 + px(36),
                    body_px,
                    max_text_w,
                    BAR_TEXT_DIM_RGBA,
                );
            }
            Result::<Vec<Rectangle<i32, Buffer>>, ()>::Ok(vec![Rectangle::from_size((w, h).into())])
        });
    }

    /// Spawn a background D-Bus listener owning `org.freedesktop.Notifications` on the user's
    /// session bus, forwarding notifications into this `NotificationCenter`.
    pub fn start_dbus_listener(&self, runtime_dir: &Path) {
        const DBUS_SCRIPT: &str = r#"
import sys, gi
gi.require_version("GLib", "2.0")
gi.require_version("Gio", "2.0")
from gi.repository import GLib, Gio

XML = """<node>
  <interface name="org.freedesktop.Notifications">
    <method name="GetCapabilities">
      <arg type="as" name="caps" direction="out"/>
    </method>
    <method name="Notify">
      <arg type="s" name="app_name" direction="in"/>
      <arg type="u" name="replaces_id" direction="in"/>
      <arg type="s" name="app_icon" direction="in"/>
      <arg type="s" name="summary" direction="in"/>
      <arg type="s" name="body" direction="in"/>
      <arg type="as" name="actions" direction="in"/>
      <arg type="a{sv}" name="hints" direction="in"/>
      <arg type="i" name="expire_timeout" direction="in"/>
      <arg type="u" name="id" direction="out"/>
    </method>
    <method name="CloseNotification">
      <arg type="u" name="id" direction="in"/>
    </method>
    <method name="GetServerInformation">
      <arg type="s" name="name" direction="out"/>
      <arg type="s" name="vendor" direction="out"/>
      <arg type="s" name="version" direction="out"/>
      <arg type="s" name="spec_version" direction="out"/>
    </method>
  </interface>
</node>"""

next_id = 1
info = Gio.DBusNodeInfo.new_for_xml(XML)

def clean(s):
    return (s or "").replace("\t", " ").replace("\n", " ").strip()

def on_call(conn, sender, path, iface, method, params, invocation):
    global next_id
    if method == "GetCapabilities":
        invocation.return_value(GLib.Variant("(as)", (["body"],)))
    elif method == "GetServerInformation":
        invocation.return_value(GLib.Variant("(ssss)", ("purpwm", "purpwm", "0.1.0", "1.2")))
    elif method == "CloseNotification":
        invocation.return_value(None)
    elif method == "Notify":
        app_name, replaces_id, _icon, summary, body, _actions, hints, timeout = params.unpack()
        nid = replaces_id if replaces_id > 0 else next_id
        if replaces_id == 0:
            next_id += 1
        urgency = int(hints.get("urgency", 1)) if isinstance(hints, dict) else 1
        sys.stdout.write(f"{nid}\t{urgency}\t{int(timeout)}\t{clean(app_name)}\t{clean(summary)}\t{clean(body)}\n")
        sys.stdout.flush()
        invocation.return_value(GLib.Variant("(u)", (nid,)))

def on_bus_acquired(conn, name):
    conn.register_object("/org/freedesktop/Notifications", info.interfaces[0], on_call, None, None)

loop = GLib.MainLoop()
Gio.bus_own_name(
    Gio.BusType.SESSION,
    "org.freedesktop.Notifications",
    Gio.BusNameOwnerFlags.REPLACE | Gio.BusNameOwnerFlags.ALLOW_REPLACEMENT,
    on_bus_acquired,
    None,
    lambda *_: loop.quit(),
)
loop.run()
"#;

        let mut cmd = Command::new("python3");
        cmd.args(["-u", "-c", DBUS_SCRIPT])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        let bus_socket = runtime_dir.join("bus");
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() && bus_socket.exists() {
            cmd.env(
                "DBUS_SESSION_BUS_ADDRESS",
                format!("unix:path={}", bus_socket.display()),
            );
        }

        let Ok(mut child) = cmd.spawn() else {
            tracing::warn!("Failed to spawn built-in D-Bus notification listener");
            return;
        };
        let Some(stdout) = child.stdout.take() else {
            return;
        };
        let incoming = Arc::clone(&self.incoming);
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if let Some(notif) = parse_dbus_notification_line(&line) {
                    if let Ok(mut guard) = incoming.lock() {
                        guard.push(notif);
                    }
                }
            }
            let _ = child.wait();
        });
    }
}

pub(crate) fn parse_dbus_notification_line(line: &str) -> Option<Notification> {
    let mut parts = line.splitn(6, '\t');
    let id: u32 = parts.next()?.parse().ok()?;
    let urgency: u8 = parts.next()?.parse().unwrap_or(1);
    let timeout_raw: i64 = parts.next()?.parse().unwrap_or(-1);
    let app_name = sanitize_line(parts.next().unwrap_or(""));
    let summary = sanitize_line(parts.next().unwrap_or(""));
    let body = sanitize_line(parts.next().unwrap_or(""));
    let timeout_ms = if timeout_raw > 0 {
        (timeout_raw as u64).clamp(1000, 30_000)
    } else if urgency >= 2 {
        12_000
    } else {
        DEFAULT_TIMEOUT_MS
    };
    Some(Notification {
        id,
        app_name,
        summary,
        body,
        urgency,
        expires_at: Instant::now() + Duration::from_millis(timeout_ms),
    })
}

fn sanitize_line(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

#[allow(clippy::too_many_arguments)]
fn draw_clipped_line(
    buf: &mut [u8],
    w: i32,
    h: i32,
    font: Option<&Font>,
    text: &str,
    x: i32,
    top: i32,
    px: f32,
    max_w: i32,
    rgba: [u8; 4],
) {
    let Some(font) = font else {
        return;
    };
    let baseline = top + (px * 0.8) as i32;
    let mut pen = x;
    for ch in text.chars() {
        let cw = font.metrics(ch, px).advance_width.ceil() as i32;
        if pen - x + cw > max_w {
            break;
        }
        let (m, bitmap) = font.rasterize(ch, px);
        let gx = pen + m.xmin;
        let gy = baseline - m.height as i32 - m.ymin;
        for row in 0..m.height {
            for col in 0..m.width {
                let cov = bitmap[row * m.width + col];
                if cov > 0 {
                    blend_pixel_argb8888(buf, w, h, gx + col as i32, gy + row as i32, rgba, cov);
                }
            }
        }
        pen += cw;
    }
}
