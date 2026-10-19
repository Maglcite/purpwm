//! `Mod + h` keybinding cheat sheet: a centred overlay drawn by the compositor listing
//! every shortcut. Closed again with `Mod + h`, `Escape`, or any click / tap.

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
    utils::{Buffer, Physical, Point, Rectangle, Size, Transform},
};

use crate::{
    bar::fill_rect,
    config::{BAR_ACCENT_RGBA, BAR_TEXT_DIM_RGBA, BAR_TEXT_FG_RGBA},
    lock::{draw, measure},
};

/// Shown in the overlay, in this order. Keep in sync with `input.rs`.
const SECTIONS: &[(&str, &[(&str, &str)])] = &[
    (
        "Session",
        &[
            ("Mod + h", "Show / hide this help"),
            ("Mod + q", "Close window"),
            ("Mod + Shift + q", "Quit purpwm"),
            ("Mod + Shift + l", "Lock screen"),
        ],
    ),
    (
        "Launch",
        &[
            ("Tap Mod", "App launcher"),
            ("Mod + p", "App launcher"),
            ("Mod + Return", "Terminal"),
            ("Mod + s  /  Mod + `", "Scratchpad terminal"),
            ("Mod + Esc", "System monitor"),
            ("Mod + w", "Wi-Fi manager"),
            ("Mod + Shift + w", "Wi-Fi on / off"),
            ("Mod + Shift + b", "Bluetooth manager"),
            ("Mod + Ctrl + b", "Bluetooth on / off"),
        ],
    ),
    (
        "Windows",
        &[
            ("Mod + j  /  k", "Focus next / previous"),
            ("Mod + Shift + Return", "Swap with master"),
            ("Mod + Arrows", "Move window"),
            ("Mod + Shift + Arrows", "Resize window"),
            ("Mod + Shift + Space", "Toggle floating"),
            ("Mod + Shift + f  /  F11", "Fullscreen"),
            ("Mod + o", "Fully opaque window"),
            ("Mod + Left-drag", "Move window"),
            ("Mod + Right-drag", "Resize window"),
            ("Mod + Middle-click", "Toggle floating"),
        ],
    ),
    (
        "Layouts",
        &[
            ("Mod + t", "Tile  []="),
            ("Mod + u", "Centered master  |M|"),
            ("Mod + g", "Grid  ###"),
            ("Mod + m", "Monocle  [M]"),
            ("Mod + f", "Floating  ><>"),
            ("Mod + Space", "Previous layout"),
            ("Mod + Ctrl + Space", "Cycle layouts"),
            ("Mod + [  /  ]", "Shrink / grow master"),
            ("Mod + i  /  d", "More / fewer masters"),
        ],
    ),
    (
        "Tags",
        &[
            ("Mod + 1..9", "View tag"),
            ("Mod + Shift + 1..9", "Move window to tag"),
            ("Mod + Ctrl + 1..9", "Add tag to view"),
            ("Mod + Ctrl + Shift + 1..9", "Toggle window on tag"),
            ("Mod + 0", "View all tags"),
            ("Mod + Shift + 0", "Window on all tags"),
            ("Mod + Tab", "Previous tags"),
        ],
    ),
    (
        "Look",
        &[
            ("Mod + Ctrl + =  /  -", "Bigger / smaller UI"),
            ("Mod + =  /  -", "More / less gap"),
            ("Mod + Shift + g", "Gaps on / off"),
            ("Mod + Ctrl + 0", "Reset gaps"),
            ("Mod + b", "Show / hide bar"),
        ],
    ),
    (
        "Media & screenshots",
        &[
            ("Print", "Screenshot"),
            ("Shift + Print", "Screenshot window"),
            ("Volume keys", "Volume / mute"),
            ("Brightness keys", "Brightness"),
            ("Media keys", "Play / next / previous"),
        ],
    ),
];

const PANEL_BG_RGBA: [u8; 4] = [0x14, 0x09, 0x24, 242];
const SECTION_RGBA: [u8; 4] = [0xE9, 0xD5, 0xFF, 255];
const COLUMNS: usize = 3;

type DrawnCache = (Size<i32, Physical>, u64, Point<i32, Physical>);

#[derive(Default)]
pub struct HelpOverlay {
    pub visible: bool,
    buffer: Option<MemoryRenderBuffer>,
    /// Output size and scale the buffer was drawn for, plus where it goes on screen.
    drawn: Option<DrawnCache>,
}

impl HelpOverlay {
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    /// Centred overlay element, re-rasterised only when the output size or scale changes.
    pub fn render_element<R>(
        &mut self,
        renderer: &mut R,
        font: Option<&Font>,
        output: Size<i32, Physical>,
        scale: f64,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        if !self.visible {
            return None;
        }
        let key = (output, scale.to_bits());
        if self.drawn.map(|(size, s, _)| (size, s)) != Some(key) || self.buffer.is_none() {
            let (buffer, loc) = rasterize(font, output, scale);
            self.buffer = Some(buffer);
            self.drawn = Some((output, scale.to_bits(), loc));
        }
        let loc = self.drawn?.2;
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            loc.to_f64(),
            self.buffer.as_ref()?,
            None,
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }
}

/// Lay the sections out in columns and draw the panel; shrinks the text until it fits.
pub(crate) fn rasterize(
    font: Option<&Font>,
    output: Size<i32, Physical>,
    scale: f64,
) -> (MemoryRenderBuffer, Point<i32, Physical>) {
    let mut px = 15.0 * scale as f32;
    loop {
        let layout = Layout::new(font, px);
        let fits = layout.width <= output.w * 95 / 100 && layout.height <= output.h * 92 / 100;
        if fits || px < 8.0 {
            let mut buffer = MemoryRenderBuffer::new(
                Fourcc::Argb8888,
                (layout.width.max(1), layout.height.max(1)),
                1,
                Transform::Normal,
                None,
            );
            layout.paint(&mut buffer, font);
            let loc = Point::from((
                (output.w - layout.width) / 2,
                (output.h - layout.height) / 2,
            ));
            return (buffer, loc);
        }
        px *= 0.9;
    }
}

struct Layout {
    px: f32,
    /// Section indices per column.
    columns: Vec<Vec<usize>>,
    key_w: i32,
    col_w: i32,
    line_h: i32,
    pad: i32,
    header_h: i32,
    width: i32,
    height: i32,
}

impl Layout {
    fn new(font: Option<&Font>, px: f32) -> Self {
        let line_h = (px * 1.6).round() as i32;
        let pad = (px * 1.6).round() as i32;
        let gap = (px * 1.4).round() as i32;
        let header_h = (px * 1.4 * 2.2).round() as i32;

        let all = SECTIONS.iter().flat_map(|(_, rows)| rows.iter());
        let key_w = all.clone().map(|(k, _)| measure(font, k, px)).max().unwrap_or(0);
        let desc_w = all.map(|(_, d)| measure(font, d, px)).max().unwrap_or(0);
        let col_w = key_w + gap + desc_w;

        // Sections are kept whole; fill each column up to an even share of the lines.
        let section_lines: Vec<usize> = SECTIONS.iter().map(|(_, rows)| rows.len() + 2).collect();
        let share = section_lines.iter().sum::<usize>().div_ceil(COLUMNS);
        let mut columns: Vec<Vec<usize>> = vec![Vec::new()];
        let mut filled = 0;
        for (i, lines) in section_lines.iter().enumerate() {
            if filled >= share && columns.len() < COLUMNS {
                columns.push(Vec::new());
                filled = 0;
            }
            columns.last_mut().unwrap().push(i);
            filled += lines;
        }
        let tallest = columns
            .iter()
            .map(|c| c.iter().map(|&i| section_lines[i]).sum::<usize>())
            .max()
            .unwrap_or(0) as i32;

        let width = pad * 2 + col_w * columns.len() as i32 + gap * 2 * (columns.len() as i32 - 1);
        let height = pad * 2 + header_h + tallest * line_h;
        Self {
            px,
            columns,
            key_w,
            col_w,
            line_h,
            pad,
            header_h,
            width,
            height,
        }
    }

    fn paint(&self, buffer: &mut MemoryRenderBuffer, font: Option<&Font>) {
        let (w, h, px) = (self.width, self.height, self.px);
        let gap = (px * 1.4).round() as i32;
        let border = ((px / 7.0).round() as i32).max(2);
        let mut ctx = buffer.render();
        let _ = ctx.draw(|buf| {
            fill_rect(buf, w, h, 0, 0, w, h, BAR_ACCENT_RGBA);
            fill_rect(buf, w, h, border, border, w - 2 * border, h - 2 * border, PANEL_BG_RGBA);

            let title_px = px * 1.4;
            draw(buf, w, h, font, "purpwm keybindings", self.pad, self.pad, title_px, BAR_TEXT_FG_RGBA);
            let hint = "Mod = Super or Alt   \u{00b7}   Mod + h / Esc / click to close";
            let hint_x = w - self.pad - measure(font, hint, px);
            let hint_top = self.pad + ((title_px - px) * 0.8) as i32;
            draw(buf, w, h, font, hint, hint_x, hint_top, px, BAR_TEXT_DIM_RGBA);

            for (c, sections) in self.columns.iter().enumerate() {
                let x = self.pad + c as i32 * (self.col_w + gap * 2);
                let mut y = self.pad + self.header_h;
                for &i in sections {
                    let (title, rows) = SECTIONS[i];
                    draw(buf, w, h, font, title, x, y, px, SECTION_RGBA);
                    fill_rect(buf, w, h, x, y + self.line_h - (px * 0.35) as i32, self.col_w, 1, BAR_ACCENT_RGBA);
                    y += self.line_h;
                    for (keys, desc) in rows.iter() {
                        draw(buf, w, h, font, keys, x, y, px, BAR_ACCENT_RGBA);
                        draw(buf, w, h, font, desc, x + self.key_w + gap, y, px, BAR_TEXT_FG_RGBA);
                        y += self.line_h;
                    }
                    y += self.line_h;
                }
            }
            Result::<Vec<Rectangle<i32, Buffer>>, ()>::Ok(vec![Rectangle::from_size((w, h).into())])
        });
    }
}
