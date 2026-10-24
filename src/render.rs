//! Damage-tracked OpenGL ES composition for `purpwm`.
//!
//! The element list is front-to-back. Locking replaces the entire desktop list
//! with the opaque lock screen; screenshots force a full redraw in the caller.

use smithay::{
    backend::renderer::{
        damage::{Error as DamageError, OutputDamageTracker},
        element::{
            memory::MemoryRenderBufferRenderElement,
            solid::SolidColorRenderElement,
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            utils::CropRenderElement,
            Id, Kind,
        },
        gles::{GlesError, GlesRenderer, GlesTarget},
        utils::CommitCounter,
        Color32F,
    },
    input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData},
    utils::{IsAlive, Logical, Physical, Point, Rectangle, Size},
    wayland::compositor::with_states,
};

use crate::{
    config::{
        BORDER_WIDTH, COLOR_BG_ROOT, COLOR_BORDER_ACTIVE, COLOR_BORDER_GLOW, COLOR_BORDER_INACTIVE,
        OPACITY_ACTIVE, OPACITY_INACTIVE,
    },
    state::PurpState,
};

smithay::backend::renderer::element::render_elements! {
    pub PurpRenderElement<=GlesRenderer>;
    Solid=SolidColorRenderElement,
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Crop=CropRenderElement<WaylandSurfaceRenderElement<GlesRenderer>>,
    Memory=MemoryRenderBufferRenderElement<GlesRenderer>,
}

fn surface_elements(
    renderer: &mut GlesRenderer,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    loc: Point<i32, Physical>,
    scale: f64,
    alpha: f32,
    kind: Kind,
) -> Vec<WaylandSurfaceRenderElement<GlesRenderer>> {
    render_elements_from_surface_tree::<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>(
        renderer, surface, loc, scale, alpha, kind,
    )
}

fn border_rects_for_window(
    id: &Id,
    commit: CommitCounter,
    geo: Rectangle<i32, Logical>,
    bw: i32,
    color: Color32F,
    scale: f64,
) -> [SolidColorRenderElement; 4] {
    let geo: Rectangle<i32, Physical> = geo.to_physical_precise_round(scale);
    let bw = ((bw as f64) * scale).round().max(1.0) as i32;
    let x = geo.loc.x;
    let y = geo.loc.y;
    let w = geo.size.w.max(bw * 2);
    let h = geo.size.h.max(bw * 2);

    let top = Rectangle::<i32, Physical>::new(Point::from((x, y)), Size::from((w, bw)));
    let bottom = Rectangle::<i32, Physical>::new(Point::from((x, y + h - bw)), Size::from((w, bw)));
    let left =
        Rectangle::<i32, Physical>::new(Point::from((x, y + bw)), Size::from((bw, h - 2 * bw)));
    let right = Rectangle::<i32, Physical>::new(
        Point::from((x + w - bw, y + bw)),
        Size::from((bw, h - 2 * bw)),
    );

    [top, bottom, left, right].map(|rect| {
        SolidColorRenderElement::new(id.clone(), rect, commit, color, Kind::Unspecified)
    })
}

fn outer_glow_rects_for_window(
    id: &Id,
    geo: Rectangle<i32, Logical>,
    scale: f64,
) -> [SolidColorRenderElement; 4] {
    let expanded = Rectangle::new(
        Point::from((geo.loc.x - 1, geo.loc.y - 1)),
        Size::from((geo.size.w + 2, geo.size.h + 2)),
    );
    border_rects_for_window(
        id,
        CommitCounter::default(),
        expanded,
        1,
        COLOR_BORDER_GLOW,
        scale,
    )
}

pub fn render_output_frame(
    state: &mut PurpState,
    renderer: &mut GlesRenderer,
    framebuffer: &mut GlesTarget<'_>,
    age: usize,
    tracker: &mut OutputDamageTracker,
    physical_size: Size<i32, Physical>,
) -> Result<Option<Vec<Rectangle<i32, Physical>>>, DamageError<GlesError>> {
    let scale = state.scale;

    if let Some(ref mut lock) = state.lock_screen {
        let font = state.bar.font();
        let elements: Vec<PurpRenderElement> = lock
            .render_element(renderer, font, physical_size, scale)
            .into_iter()
            .map(PurpRenderElement::from)
            .collect();
        let result = tracker.render_output(renderer, framebuffer, age, &elements, COLOR_BG_ROOT)?;
        return Ok(result.damage.cloned());
    }

    let mut elements = Vec::new();

    if state.screenshot_request.is_none() {
        match state.cursor_status.clone() {
            CursorImageStatus::Hidden => {}
            CursorImageStatus::Surface(surface) if surface.alive() => {
                let hotspot = with_states(&surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|data| data.lock().unwrap().hotspot)
                        .unwrap_or_default()
                });
                let loc =
                    (state.pointer_location - hotspot.to_f64()).to_physical_precise_round(scale);
                elements.extend(
                    surface_elements(renderer, &surface, loc, scale, 1.0, Kind::Cursor)
                        .into_iter()
                        .map(PurpRenderElement::from),
                );
            }
            status => {
                let icon = match status {
                    CursorImageStatus::Named(icon) => icon,
                    _ => CursorIcon::Default,
                };
                let cpos = state.pointer_location.to_physical(scale);
                if let Some(cursor) = state
                    .cursor_theme
                    .render_element(renderer, icon, cpos, scale)
                {
                    elements.push(cursor.into());
                } else if let Some(cursor) = state.bar.cursor_render_element(renderer, cpos) {
                    elements.push(cursor.into());
                }
            }
        }
    }

    if let Some(icon) = state.dnd_icon.as_ref().filter(|s| s.alive()) {
        let loc = state.pointer_location.to_physical_precise_round(scale);
        elements.extend(
            surface_elements(renderer, icon, loc, scale, 0.92, Kind::Unspecified)
                .into_iter()
                .map(PurpRenderElement::from),
        );
    }

    {
        let font = state.bar.font();
        if let Some(help) = state
            .help
            .render_element(renderer, font, physical_size, scale)
        {
            elements.push(help.into());
        }
    }

    let bar_visible = state.bar_visible();
    {
        let font = state.bar.font();
        if let Some(notifications) = state.notifications.render_element(
            renderer,
            font,
            state.output_size.w,
            bar_visible,
            scale,
        ) {
            elements.push(notifications.into());
        }
    }

    for layer in state.layer_surfaces.iter().rev() {
        if !layer.surface.alive()
            || !layer.is_above_windows()
            || layer.namespace == "Spotlight-keepalive"
        {
            continue;
        }
        let loc = layer.geo.loc.to_physical_precise_round(scale);
        let surfaces = surface_elements(
            renderer,
            layer.surface.wl_surface(),
            loc,
            scale,
            OPACITY_ACTIVE,
            Kind::Unspecified,
        );
        if surfaces.is_empty() {
            continue;
        }
        elements.extend(
            border_rects_for_window(
                &layer.border_render_id,
                CommitCounter::default(),
                layer.geo,
                BORDER_WIDTH,
                COLOR_BORDER_ACTIVE,
                scale,
            )
            .into_iter()
            .map(PurpRenderElement::from),
        );
        elements.extend(
            outer_glow_rects_for_window(&layer.glow_render_id, layer.geo, scale)
                .into_iter()
                .map(PurpRenderElement::from),
        );
        elements.extend(surfaces.into_iter().map(PurpRenderElement::from));
    }

    for (popup, loc) in state.visible_popups() {
        elements.extend(
            surface_elements(
                renderer,
                popup.wl_surface(),
                loc.to_physical_precise_round(scale),
                scale,
                1.0,
                Kind::Unspecified,
            )
            .into_iter()
            .map(PurpRenderElement::from),
        );
    }

    if bar_visible {
        let occupied_tags = state
            .windows
            .iter()
            .filter(|w| !w.is_scratchpad)
            .fold(0u16, |acc, w| acc | w.tags);
        let urgent_tags = state
            .windows
            .iter()
            .filter(|w| w.is_urgent)
            .fold(0u16, |acc, w| acc | w.tags);
        let focused_tags = state
            .focused_idx
            .and_then(|idx| state.windows.get(idx))
            .map(|w| w.tags)
            .unwrap_or(0);
        let focused_title = state
            .focused_idx
            .and_then(|idx| state.windows.get(idx))
            .filter(|w| w.is_visible_on(state.workspace.selected_tags))
            .map(|w| {
                if w.is_scratchpad {
                    format!("[scratch] {}", w.title())
                } else if w.opacity_override == Some(1.0) {
                    format!("[opaque] {}", w.title())
                } else {
                    w.title()
                }
            })
            .unwrap_or_else(|| {
                String::from(
                    "purpwm — Mod+h: Help | Tap Mod: Spotlight | Mod+Enter: Term | Mod+w: Wi-Fi | Mod+Shift+b: BT",
                )
            });
        if let Some(bar) = state.bar.render_element(
            renderer,
            physical_size.w,
            state.workspace.selected_tags,
            occupied_tags,
            focused_tags,
            urgent_tags,
            state.workspace.layout(),
            &focused_title,
        ) {
            elements.push(bar.into());
        }
    }

    let focused_idx = state.focused_idx;
    for idx in state.stacking_order() {
        let w = &mut state.windows[idx];
        let is_focused = focused_idx == Some(idx);
        let alpha = if w.is_fullscreen {
            1.0
        } else {
            w.opacity_override.unwrap_or(if is_focused {
                OPACITY_ACTIVE
            } else {
                OPACITY_INACTIVE
            })
        };
        let loc = w.surface_loc().to_physical_precise_round(scale);
        let clip: Rectangle<i32, Physical> =
            Rectangle::new(w.content_loc(), w.surface_size()).to_physical_precise_round(scale);

        if !w.is_fullscreen && BORDER_WIDTH > 0 {
            let color = if is_focused {
                COLOR_BORDER_ACTIVE
            } else {
                COLOR_BORDER_INACTIVE
            };
            if w.border_color != Some(color) {
                w.border_color = Some(color);
                w.border_commit.increment();
            }
            elements.extend(
                border_rects_for_window(
                    &w.border_render_id,
                    w.border_commit,
                    w.geo,
                    BORDER_WIDTH,
                    color,
                    scale,
                )
                .into_iter()
                .map(PurpRenderElement::from),
            );
            if is_focused {
                elements.extend(
                    outer_glow_rects_for_window(&w.glow_render_id, w.geo, scale)
                        .into_iter()
                        .map(PurpRenderElement::from),
                );
            }
        }

        elements.extend(
            surface_elements(
                renderer,
                w.wl_surface(),
                loc,
                scale,
                alpha,
                Kind::Unspecified,
            )
            .into_iter()
            .filter_map(|elem| CropRenderElement::from_element(elem, scale, clip))
            .map(PurpRenderElement::from),
        );

        if w.is_fullscreen {
            elements.push(
                SolidColorRenderElement::new(
                    w.backdrop_render_id.clone(),
                    clip,
                    CommitCounter::default(),
                    Color32F::new(0.0, 0.0, 0.0, 1.0),
                    Kind::Unspecified,
                )
                .into(),
            );
        }
    }

    for layer in &state.layer_surfaces {
        if !layer.surface.alive()
            || layer.is_above_windows()
            || layer.namespace == "Spotlight-keepalive"
        {
            continue;
        }
        elements.extend(
            surface_elements(
                renderer,
                layer.surface.wl_surface(),
                layer.geo.loc.to_physical_precise_round(scale),
                scale,
                1.0,
                Kind::Unspecified,
            )
            .into_iter()
            .map(PurpRenderElement::from),
        );
    }

    if let Some(wallpaper) =
        state
            .wallpaper
            .render_element(renderer, physical_size.w, physical_size.h)
    {
        elements.push(wallpaper.into());
    }

    let result = tracker.render_output(renderer, framebuffer, age, &elements, COLOR_BG_ROOT)?;
    Ok(result.damage.cloned())
}
