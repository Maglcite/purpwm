//! OpenGL ES (`GlesRenderer`) frame composition for `purpwm`:
//! 1. Clears output with translucent deep purple backdrop (`COLOR_BG_ROOT`)
//! 2. Renders visible windows with active (`0.94`), inactive (`0.84`), or custom per-window (`1.0`) opacity
//! 3. Draws 2px glowing electric purple borders (`COLOR_BORDER_ACTIVE` + 1px outer `COLOR_BORDER_GLOW`)
//! 4. Renders built-in translucent purple top status bar (`MemoryRenderBufferRenderElement`)
//! 5. Renders `wlr-layer-shell` overlay surfaces (such as `/home/mag/Documents/Spotlight`) centered on top
//! 6. Always renders a crisp high-contrast purple/white pointer cursor at `state.pointer_location`

use smithay::{
    backend::renderer::{
        element::{
            memory::MemoryRenderBufferRenderElement,
            solid::SolidColorRenderElement,
            surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement},
            Id, Kind,
        },
        gles::{GlesError, GlesRenderer, GlesTarget},
        utils::{draw_render_elements, CommitCounter},
        Color32F, Frame, Renderer,
    },
    input::pointer::CursorImageStatus,
    utils::{Logical, Physical, Point, Rectangle, Size, Transform},
};

use crate::{
    config::{
        BORDER_WIDTH, COLOR_BG_ROOT, COLOR_BORDER_ACTIVE, COLOR_BORDER_GLOW, COLOR_BORDER_INACTIVE,
        OPACITY_ACTIVE, OPACITY_INACTIVE,
    },
    state::PurpState,
};

/// Build the 4 rectangular border strips (`Top`, `Bottom`, `Left`, `Right`) around `geo`.
fn border_rects_for_window(
    geo: Rectangle<i32, Logical>,
    bw: i32,
    color: Color32F,
) -> [SolidColorRenderElement; 4] {
    let x = geo.loc.x;
    let y = geo.loc.y;
    let w = geo.size.w.max(bw * 2);
    let h = geo.size.h.max(bw * 2);

    let top = Rectangle::<i32, Physical>::new(Point::from((x, y)), Size::from((w, bw)));
    let bottom =
        Rectangle::<i32, Physical>::new(Point::from((x, y + h - bw)), Size::from((w, bw)));
    let left =
        Rectangle::<i32, Physical>::new(Point::from((x, y + bw)), Size::from((bw, h - 2 * bw)));
    let right = Rectangle::<i32, Physical>::new(
        Point::from((x + w - bw, y + bw)),
        Size::from((bw, h - 2 * bw)),
    );

    [
        SolidColorRenderElement::new(
            Id::new(),
            top,
            CommitCounter::default(),
            color,
            Kind::Unspecified,
        ),
        SolidColorRenderElement::new(
            Id::new(),
            bottom,
            CommitCounter::default(),
            color,
            Kind::Unspecified,
        ),
        SolidColorRenderElement::new(
            Id::new(),
            left,
            CommitCounter::default(),
            color,
            Kind::Unspecified,
        ),
        SolidColorRenderElement::new(
            Id::new(),
            right,
            CommitCounter::default(),
            color,
            Kind::Unspecified,
        ),
    ]
}

/// Build an outer 1px soft amethyst glow halo around `geo` for the focused window.
fn outer_glow_rects_for_window(geo: Rectangle<i32, Logical>) -> [SolidColorRenderElement; 4] {
    let expanded = Rectangle::new(
        Point::from((geo.loc.x - 1, geo.loc.y - 1)),
        Size::from((geo.size.w + 2, geo.size.h + 2)),
    );
    border_rects_for_window(expanded, 1, COLOR_BORDER_GLOW)
}

/// Render a complete compositor frame onto `framebuffer`.
pub fn render_output_frame<'a>(
    state: &mut PurpState,
    renderer: &mut GlesRenderer,
    framebuffer: &mut GlesTarget<'a>,
    physical_size: Size<i32, Physical>,
    transform: Transform,
) -> Result<(), GlesError> {
    let damage = Rectangle::from_size(physical_size);

    // 1. Prepare the status bar element
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
            String::from("purpwm — Tap Mod: Spotlight | Mod+Enter: Term | Mod+w: Wi-Fi | Mod+Shift+b: BT")
        });

    let bar_elem: Option<MemoryRenderBufferRenderElement<GlesRenderer>> =
        if state.workspace.show_bar {
            state.bar.render_element(
                renderer,
                physical_size.w,
                state.workspace.selected_tags,
                occupied_tags,
                focused_tags,
                urgent_tags,
                state.workspace.layout(),
                &focused_title,
            )
        } else {
            None
        };

    // 2. Collect visible window surface elements & their 2px purple borders + 1px outer glow.
    let mut ordered_indices: Vec<usize> = Vec::with_capacity(state.windows.len());

    // Topmost: focused window
    if let Some(f_idx) = state.focused_idx {
        if f_idx < state.windows.len()
            && state.windows[f_idx].is_visible_on(state.workspace.selected_tags)
        {
            ordered_indices.push(f_idx);
        }
    }

    // Next: visible scratchpad or floating windows
    for (idx, w) in state.windows.iter().enumerate() {
        if state.focused_idx == Some(idx) || !w.is_visible_on(state.workspace.selected_tags) {
            continue;
        }
        if w.is_floating || w.is_scratchpad {
            ordered_indices.push(idx);
        }
    }

    // Next: remaining tiled windows
    for (idx, w) in state.windows.iter().enumerate() {
        if state.focused_idx == Some(idx) || !w.is_visible_on(state.workspace.selected_tags) {
            continue;
        }
        if !w.is_floating && !w.is_scratchpad {
            ordered_indices.push(idx);
        }
    }

    let mut window_batches: Vec<(
        Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
        Option<[SolidColorRenderElement; 4]>,
        Option<[SolidColorRenderElement; 4]>,
    )> = Vec::with_capacity(ordered_indices.len());

    for idx in ordered_indices {
        let w = &state.windows[idx];
        let is_focused = state.focused_idx == Some(idx);
        let alpha = w.opacity_override.unwrap_or(if is_focused {
            OPACITY_ACTIVE
        } else {
            OPACITY_INACTIVE
        });
        let surf_loc = w.surface_loc();
        let loc_phys = Point::<i32, Physical>::from((surf_loc.x, surf_loc.y));

        let surf_elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                renderer,
                w.wl_surface(),
                loc_phys,
                1.0,
                alpha,
                Kind::Unspecified,
            );

        let (borders, glow) = if !w.is_fullscreen && BORDER_WIDTH > 0 {
            let color = if is_focused {
                COLOR_BORDER_ACTIVE
            } else {
                COLOR_BORDER_INACTIVE
            };
            let glow_opt = if is_focused {
                Some(outer_glow_rects_for_window(w.geo))
            } else {
                None
            };
            (
                Some(border_rects_for_window(w.geo, BORDER_WIDTH, color)),
                glow_opt,
            )
        } else {
            (None, None)
        };

        window_batches.push((surf_elems, borders, glow));
    }

    // 3. Collect active `wlr-layer-shell` overlay surfaces (e.g. `Spotlight` launcher)
    let mut layer_batches: Vec<(
        Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
        [SolidColorRenderElement; 4],
        [SolidColorRenderElement; 4],
    )> = Vec::with_capacity(state.layer_surfaces.len());

    for layer in &state.layer_surfaces {
        if !layer.surface.alive() {
            continue;
        }
        let loc_phys = Point::<i32, Physical>::from((layer.geo.loc.x, layer.geo.loc.y));
        let elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                renderer,
                layer.surface.wl_surface(),
                loc_phys,
                1.0,
                OPACITY_ACTIVE,
                Kind::Unspecified,
            );
        if !elems.is_empty() {
            let border = border_rects_for_window(layer.geo, BORDER_WIDTH, COLOR_BORDER_ACTIVE);
            let glow = outer_glow_rects_for_window(layer.geo);
            layer_batches.push((elems, border, glow));
        }
    }

    // 4. Always render our high-contrast purple/white pointer cursor on top unless explicitly hidden
    let cursor_elem: Option<MemoryRenderBufferRenderElement<GlesRenderer>> =
        if !matches!(state.cursor_status, CursorImageStatus::Hidden) {
            let cpos = Point::<f64, Physical>::from((
                state.pointer_location.x,
                state.pointer_location.y,
            ));
            state.bar.cursor_render_element(renderer, cpos)
        } else {
            None
        };

    // 5. Begin OpenGL ES frame & draw
    let mut frame = renderer.render(framebuffer, physical_size, transform)?;

    // Clear background with deep translucent purple
    frame.clear(COLOR_BG_ROOT, &[damage])?;

    // Draw windows from back to front so alpha blending layers properly over the purple backdrop
    for (surf_elems, maybe_borders, maybe_glow) in window_batches.iter().rev() {
        draw_render_elements(&mut frame, 1.0, surf_elems, &[damage])?;
        if let Some(glow) = maybe_glow {
            draw_render_elements::<GlesRenderer, f64, SolidColorRenderElement>(
                &mut frame,
                1.0,
                glow,
                &[damage],
            )?;
        }
        if let Some(borders) = maybe_borders {
            draw_render_elements::<GlesRenderer, f64, SolidColorRenderElement>(
                &mut frame,
                1.0,
                borders,
                &[damage],
            )?;
        }
    }

    // Draw the top status bar on top of tiled windows
    if let Some(ref bar) = bar_elem {
        draw_render_elements(&mut frame, 1.0, std::slice::from_ref(bar), &[damage])?;
    }

    // Draw layer-shell overlays (such as Spotlight) centered on top of everything
    for (layer_elems, border, glow) in &layer_batches {
        draw_render_elements(&mut frame, 1.0, layer_elems, &[damage])?;
        draw_render_elements::<GlesRenderer, f64, SolidColorRenderElement>(
            &mut frame,
            1.0,
            glow,
            &[damage],
        )?;
        draw_render_elements::<GlesRenderer, f64, SolidColorRenderElement>(
            &mut frame,
            1.0,
            border,
            &[damage],
        )?;
    }

    // Draw software cursor on top of all surfaces
    if let Some(ref cur) = cursor_elem {
        draw_render_elements(&mut frame, 1.0, std::slice::from_ref(cur), &[damage])?;
    }

    let _ = frame.finish()?;
    Ok(())
}
