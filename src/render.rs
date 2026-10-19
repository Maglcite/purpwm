//! OpenGL ES (`GlesRenderer`) frame composition for `purpwm`:
//! 1. If the session is locked (`state.lock_screen`), renders only the full-screen purple PAM
//!    lock screen so no window, bar, or notification is visible while locked
//! 2. Otherwise clears output with translucent deep purple backdrop (`COLOR_BG_ROOT`)
//! 3. Renders visible windows with active (`0.94`), inactive (`0.84`), or custom per-window (`1.0`) opacity
//! 4. Draws 2px glowing electric purple borders (`COLOR_BORDER_ACTIVE` + 1px outer `COLOR_BORDER_GLOW`)
//! 5. Renders built-in translucent purple top status bar (`MemoryRenderBufferRenderElement`),
//!    hidden while the focused window is fullscreen
//! 6. Renders popup menus above windows and the bar
//! 7. Renders `wlr-layer-shell` surfaces: `Background`/`Bottom` under the windows, `Top`/`Overlay`
//!    (such as `/home/mag/Documents/Spotlight`) on top
//! 8. Renders active desktop notification toasts (`NotificationCenter`) in the top-right corner
//! 9. Renders any active Wayland drag-and-drop icon (`state.dnd_icon`) and the software cursor

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
    utils::{IsAlive, Logical, Physical, Point, Rectangle, Size, Transform},
};

use crate::{
    config::{
        BORDER_WIDTH, COLOR_BG_ROOT, COLOR_BORDER_ACTIVE, COLOR_BORDER_GLOW, COLOR_BORDER_INACTIVE,
        OPACITY_ACTIVE, OPACITY_INACTIVE,
    },
    state::PurpState,
};

type WindowRenderBatch = (
    Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
    Option<[SolidColorRenderElement; 4]>,
    Option<[SolidColorRenderElement; 4]>,
);

type LayerRenderBatch = (
    Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
    [SolidColorRenderElement; 4],
    [SolidColorRenderElement; 4],
);

/// Build the 4 rectangular border strips (`Top`, `Bottom`, `Left`, `Right`) around the
/// logical `geo`, `bw` logical pixels thick, converted to physical pixels at `scale`.
fn border_rects_for_window(
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
        SolidColorRenderElement::new(
            Id::new(),
            rect,
            CommitCounter::default(),
            color,
            Kind::Unspecified,
        )
    })
}

/// Build an outer 1px soft amethyst glow halo around `geo` for the focused window.
fn outer_glow_rects_for_window(
    geo: Rectangle<i32, Logical>,
    scale: f64,
) -> [SolidColorRenderElement; 4] {
    let expanded = Rectangle::new(
        Point::from((geo.loc.x - 1, geo.loc.y - 1)),
        Size::from((geo.size.w + 2, geo.size.h + 2)),
    );
    border_rects_for_window(expanded, 1, COLOR_BORDER_GLOW, scale)
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
    let scale = state.scale;

    // 0. If locked, draw only the full-screen purple PAM lock screen (never expose desktop contents).
    if let Some(ref mut lock) = state.lock_screen {
        let font = state.bar.font();
        let lock_elem = lock.render_element(renderer, font, physical_size, scale);
        let mut frame = renderer.render(framebuffer, physical_size, transform)?;
        frame.clear(COLOR_BG_ROOT, &[damage])?;
        if let Some(ref elem) = lock_elem {
            draw_render_elements(&mut frame, 1.0, std::slice::from_ref(elem), &[damage])?;
        }
        let _ = frame.finish()?;
        return Ok(());
    }

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
            String::from(
                "purpwm — Tap Mod: Spotlight | Mod+Enter: Term | Mod+w: Wi-Fi | Mod+Shift+b: BT",
            )
        });

    let bar_visible = state.bar_visible();
    let bar_elem: Option<MemoryRenderBufferRenderElement<GlesRenderer>> = if bar_visible {
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

    // 2. Collect visible window surface elements & their 2px purple borders + 1px outer glow,
    //    topmost first (same order the pointer hit-testing uses).
    let ordered_indices = state.stacking_order();

    let mut window_batches: Vec<WindowRenderBatch> = Vec::with_capacity(ordered_indices.len());

    for idx in ordered_indices {
        let w = &state.windows[idx];
        let is_focused = state.focused_idx == Some(idx);
        let alpha = w.opacity_override.unwrap_or(if is_focused {
            OPACITY_ACTIVE
        } else {
            OPACITY_INACTIVE
        });
        let loc_phys = w.surface_loc().to_physical_precise_round(scale);

        let surf_elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                renderer,
                w.wl_surface(),
                loc_phys,
                scale,
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
                Some(outer_glow_rects_for_window(w.geo, scale))
            } else {
                None
            };
            (
                Some(border_rects_for_window(w.geo, BORDER_WIDTH, color, scale)),
                glow_opt,
            )
        } else {
            (None, None)
        };

        window_batches.push((surf_elems, borders, glow));
    }

    // 3. Popup menus of visible windows, drawn above all windows (front-to-back like all
    //    element lists here: topmost first).
    let popup_elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = state
        .visible_popups()
        .into_iter()
        .flat_map(|(popup, loc)| {
            render_elements_from_surface_tree(
                renderer,
                popup.wl_surface(),
                loc.to_physical_precise_round(scale),
                scale,
                1.0,
                Kind::Unspecified,
            )
        })
        .collect();

    // 4. `wlr-layer-shell` surfaces: `Background`/`Bottom` (wallpapers) go under the windows,
    //    `Top`/`Overlay` (e.g. the `Spotlight` launcher) above them with a glowing border.
    let mut below_layer_elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = Vec::new();
    let mut layer_batches: Vec<LayerRenderBatch> = Vec::with_capacity(state.layer_surfaces.len());

    for layer in &state.layer_surfaces {
        if !layer.surface.alive() {
            continue;
        }
        let loc_phys = layer.geo.loc.to_physical_precise_round(scale);
        let alpha = if layer.is_above_windows() {
            OPACITY_ACTIVE
        } else {
            1.0
        };
        let elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
            render_elements_from_surface_tree(
                renderer,
                layer.surface.wl_surface(),
                loc_phys,
                scale,
                alpha,
                Kind::Unspecified,
            );
        if elems.is_empty() {
            continue;
        }
        if layer.is_above_windows() {
            let border =
                border_rects_for_window(layer.geo, BORDER_WIDTH, COLOR_BORDER_ACTIVE, scale);
            let glow = outer_glow_rects_for_window(layer.geo, scale);
            layer_batches.push((elems, border, glow));
        } else {
            // Later layers sit on top, so they go to the front of the list.
            below_layer_elems.splice(0..0, elems);
        }
    }

    // 5. Notification toasts in the top-right corner
    let notif_elem: Option<MemoryRenderBufferRenderElement<GlesRenderer>> = {
        let font = state.bar.font();
        state
            .notifications
            .render_element(renderer, font, state.output_size.w, bar_visible, scale)
    };

    // 6. Drag-and-drop icon surface (if an active client DnD session provided one)
    let dnd_elems: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = state
        .dnd_icon
        .as_ref()
        .filter(|s| s.alive())
        .map(|icon| {
            let loc_phys = state.pointer_location.to_physical_precise_round(scale);
            render_elements_from_surface_tree(
                renderer,
                icon,
                loc_phys,
                scale,
                0.92,
                Kind::Unspecified,
            )
        })
        .unwrap_or_default();

    // 7. Always render our high-contrast purple/white pointer cursor on top unless explicitly
    //    hidden (or a screenshot is being taken of this frame)
    let cursor_elem: Option<MemoryRenderBufferRenderElement<GlesRenderer>> =
        if !matches!(state.cursor_status, CursorImageStatus::Hidden)
            && state.screenshot_request.is_none()
        {
            let cpos = state.pointer_location.to_physical(scale);
            state.bar.cursor_render_element(renderer, cpos)
        } else {
            None
        };

    // 8. Begin OpenGL ES frame & draw
    let mut frame = renderer.render(framebuffer, physical_size, transform)?;

    // Clear background with deep translucent purple
    frame.clear(COLOR_BG_ROOT, &[damage])?;

    // Wallpaper / bottom layer-shell surfaces
    draw_render_elements(&mut frame, scale, &below_layer_elems, &[damage])?;

    // Draw windows from back to front so alpha blending layers properly over the purple backdrop
    for (surf_elems, maybe_borders, maybe_glow) in window_batches.iter().rev() {
        draw_render_elements(&mut frame, scale, surf_elems, &[damage])?;
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

    // Menus / dropdowns above windows and the bar
    draw_render_elements(&mut frame, scale, &popup_elems, &[damage])?;

    // Draw layer-shell overlays (such as Spotlight) centered on top of everything
    for (layer_elems, border, glow) in &layer_batches {
        draw_render_elements(&mut frame, scale, layer_elems, &[damage])?;
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

    // Draw desktop notification toasts
    if let Some(ref notif) = notif_elem {
        draw_render_elements(&mut frame, 1.0, std::slice::from_ref(notif), &[damage])?;
    }

    // Draw drag-and-drop icon under the cursor
    draw_render_elements(&mut frame, scale, &dnd_elems, &[damage])?;

    // Draw software cursor on top of all surfaces
    if let Some(ref cur) = cursor_elem {
        // The cursor bitmap is in logical pixels, so it grows with the scale too.
        draw_render_elements(&mut frame, scale, std::slice::from_ref(cur), &[damage])?;
    }

    let _ = frame.finish()?;
    Ok(())
}
