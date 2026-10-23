//! Static image wallpaper, cached at the current output resolution.

use std::path::PathBuf;

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
    utils::{Point, Transform},
};

pub struct Wallpaper {
    image: Option<image::RgbaImage>,
    buffer: Option<(i32, i32, MemoryRenderBuffer)>,
}

impl Wallpaper {
    pub fn from_environment() -> Self {
        let image = std::env::var_os("PURPWM_WALLPAPER").and_then(|path| {
            let path = PathBuf::from(path);
            match image::open(&path) {
                Ok(image) => Some(image.to_rgba8()),
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "Unable to load wallpaper");
                    None
                }
            }
        });
        Self {
            image,
            buffer: None,
        }
    }

    pub fn render_element<R>(
        &mut self,
        renderer: &mut R,
        width: i32,
        height: i32,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        let source = self.image.as_ref()?;
        let width = width.max(1);
        let height = height.max(1);
        if !matches!(self.buffer, Some((w, h, _)) if w == width && h == height) {
            let scaled = image::imageops::resize(
                source,
                width as u32,
                height as u32,
                image::imageops::FilterType::Triangle,
            );
            self.buffer = Some((
                width,
                height,
                MemoryRenderBuffer::from_slice(
                    scaled.as_raw(),
                    Fourcc::Abgr8888,
                    (width, height),
                    1,
                    Transform::Normal,
                    None,
                ),
            ));
        }
        let (_, _, buffer) = self.buffer.as_ref()?;
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            Point::from((0.0, 0.0)),
            buffer,
            Some(1.0),
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }
}
