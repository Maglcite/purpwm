//! Pointer cursor drawn from the system XCursor theme (`XCURSOR_THEME`, else the `default`
//! theme, e.g. Adwaita), at `XCURSOR_SIZE` (default 24) times the output scale.
//!
//! Named shapes (from `wp_cursor_shape_v1`, or the arrow by default) come from the theme;
//! clients that draw their own cursor surface are rendered by `render.rs` instead.

use std::{collections::HashMap, fs};

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
    input::pointer::CursorIcon,
    utils::{Logical, Physical, Point, Size, Transform},
};

/// A theme image uploaded once and reused every frame.
struct LoadedCursor {
    buffer: MemoryRenderBuffer,
    /// Hotspot in physical pixels at the drawn size.
    hotspot: Point<f64, Physical>,
    /// Drawn size in physical pixels.
    size: Size<i32, Logical>,
}

pub struct CursorTheme {
    theme: xcursor::CursorTheme,
    base_size: u32,
    /// Keyed by icon name and target pixel size; `None` = not in the theme.
    cache: HashMap<(&'static str, u32), Option<LoadedCursor>>,
}

impl CursorTheme {
    pub fn load() -> Self {
        let name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "default".to_string());
        let base_size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&s: &u32| s > 0)
            .unwrap_or(24);
        Self {
            theme: xcursor::CursorTheme::load(&name),
            base_size,
            cache: HashMap::new(),
        }
    }

    /// Render element for `icon` with its hotspot at `pointer` (physical pixels), or `None`
    /// if the theme has neither the icon nor the default arrow.
    pub fn render_element<R>(
        &mut self,
        renderer: &mut R,
        icon: CursorIcon,
        pointer: Point<f64, Physical>,
        scale: f64,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        let target = (self.base_size as f64 * scale).round().max(8.0) as u32;
        let icon = if self.get(icon, target).is_some() {
            icon
        } else {
            CursorIcon::Default
        };
        let cursor = self.get(icon, target)?;
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            pointer - cursor.hotspot,
            &cursor.buffer,
            None,
            None,
            Some(cursor.size),
            Kind::Cursor,
        )
        .ok()
    }

    fn get(&mut self, icon: CursorIcon, target: u32) -> Option<&LoadedCursor> {
        let theme = &self.theme;
        self.cache
            .entry((icon.name(), target))
            .or_insert_with(|| {
                std::iter::once(icon.name())
                    .chain(icon.alt_names().iter().copied())
                    .find_map(|name| load_icon(theme, name, target))
            })
            .as_ref()
    }
}

/// Load the theme image closest to `target` pixels (preferring the next larger one) and
/// scale it to exactly `target` so the cursor size follows the output scale.
fn load_icon(theme: &xcursor::CursorTheme, name: &str, target: u32) -> Option<LoadedCursor> {
    let bytes = fs::read(theme.load_icon(name)?).ok()?;
    let images = xcursor::parser::parse_xcursor(&bytes)?;
    // Animated cursors list every frame; the first frame of the best size is used.
    let image = images
        .iter()
        .min_by_key(|img| {
            let below = img.size < target;
            (below, img.size.abs_diff(target))
        })?;

    let factor = target as f64 / image.size.max(1) as f64;
    // Xcursor pixels are little-endian ARGB words (premultiplied), i.e. `Argb8888`.
    let buffer = MemoryRenderBuffer::from_slice(
        &image.pixels_rgba,
        Fourcc::Argb8888,
        (image.width as i32, image.height as i32),
        1,
        Transform::Normal,
        None,
    );
    Some(LoadedCursor {
        buffer,
        hotspot: Point::from((image.xhot as f64 * factor, image.yhot as f64 * factor)),
        size: Size::from((
            (image.width as f64 * factor).round() as i32,
            (image.height as f64 * factor).round() as i32,
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The arrow and an I-beam load from the system theme at a scaled size.
    #[test]
    fn loads_theme_cursors_at_scaled_size() {
        let mut theme = CursorTheme::load();
        for icon in [CursorIcon::Default, CursorIcon::Text, CursorIcon::Pointer] {
            let cursor = theme.get(icon, 36).expect("icon missing from cursor theme");
            assert!(cursor.size.w >= 24 && cursor.size.w <= 72, "{:?}", cursor.size);
            assert!(cursor.hotspot.x >= 0.0 && cursor.hotspot.y >= 0.0);
        }
    }
}
