//! What one output shows, for either backend: navi's overlay, the pointer, layer surfaces,
//! windows and the wallpaper, in the output's own physical px.

use std::time::Duration;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ImportAll, ImportMem, Renderer, Texture,
            element::{
                AsRenderElements, Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                render_elements,
                solid::{SolidColorBuffer, SolidColorRenderElement},
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
                utils::CropRenderElement,
            },
        },
    },
    desktop::{LayerMap, Window, WindowSurface, layer_map_for_output},
    output::Output,
    utils::{Point, Rectangle, Scale, Transform},
    wayland::shell::wlr_layer::Layer,
};
use tracing::warn;

use crate::{
    cursor::Cursor,
    image::Image,
    layout, monitors,
    state::{Navi, identity},
    theme, wallpaper,
};

render_elements! {
    /// Everything on screen, top first.
    pub Frame<R> where R: ImportAll + ImportMem;
    Overlay=SolidColorRenderElement,
    Surface=WaylandSurfaceRenderElement<R>,
    /// A tiled or floating window's own surfaces, cropped to its clip rect.
    Clipped=CropRenderElement<WaylandSurfaceRenderElement<R>>,
    /// The wallpaper and the pointer.
    Memory=MemoryRenderBufferRenderElement<R>,
}

/// A window button's glyph texture: whose, which, lit, its px size and the colours it was
/// drawn in. One buffer per button keeps element ids unique and stable.
type GlyphKey = (layout::WinId, layout::Button, bool, u32, (u32, u32));
type Physical = Rectangle<i32, smithay::utils::Physical>;

/// Stable decoration identities let the damage tracker recognize an unchanged desktop.
#[derive(Default)]
pub struct Solids {
    rects: Vec<SolidColorBuffer>,
    /// Dialog frames, which draw with their dialogs.
    frames: Vec<SolidColorBuffer>,
    glyphs: std::collections::HashMap<GlyphKey, MemoryRenderBuffer>,
}

/// `rects` drawn from `buffers`, reused in order.
fn solid(
    buffers: &mut Vec<SolidColorBuffer>,
    rects: Vec<(Physical, [f32; 4])>,
) -> Vec<SolidColorRenderElement> {
    buffers.resize_with(rects.len(), SolidColorBuffer::default);
    buffers
        .iter_mut()
        .zip(rects)
        .map(|(buffer, (rect, color))| {
            buffer.update((rect.size.w, rect.size.h), color);
            SolidColorRenderElement::from_buffer(buffer, rect.loc, 1.0, 1.0, Kind::Unspecified)
        })
        .collect()
}

impl Solids {
    fn elements(&mut self, rects: Vec<(Physical, [f32; 4])>) -> Vec<SolidColorRenderElement> {
        solid(&mut self.rects, rects)
    }

    /// The window buttons' glyphs, by window; textures of buttons no longer shown are dropped.
    fn glyphs<R>(
        &mut self,
        renderer: &mut R,
        buttons: Vec<(layout::WinId, layout::Button, bool, Physical)>,
    ) -> Vec<(layout::WinId, MemoryRenderBufferRenderElement<R>)>
    where
        R: Renderer + ImportMem,
        R::TextureId: Texture + Clone + Send + 'static,
    {
        let colours = (theme::resolve(theme::INK), theme::resolve(theme::SELECTED));
        let mut used = std::collections::HashSet::new();
        let mut out = Vec::new();
        for (id, b, lit, r) in buttons {
            let size = r.size.w.max(1) as u32;
            let key = (id, b, lit, size, colours);
            used.insert(key);
            let buffer = match self.glyphs.entry(key) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    let Some(px) = crate::glyph::button(b, lit, size) else {
                        continue;
                    };
                    e.insert(MemoryRenderBuffer::from_slice(
                        px.data(),
                        Fourcc::Abgr8888,
                        (size as i32, size as i32),
                        1,
                        Transform::Normal,
                        None,
                    ))
                }
            };
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                r.loc.to_f64(),
                buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            ) {
                Ok(e) => out.push((id, e)),
                Err(e) => warn!("window button not drawn: {e:?}"),
            }
        }
        self.glyphs.retain(|k, _| used.contains(k));
        out
    }
}

/// navi's own arrow, at a whole-number buffer scale.
pub struct Pointer {
    buffer: MemoryRenderBuffer,
    /// In logical px.
    hotspot: (f64, f64),
}

impl Pointer {
    /// An arrow loaded at `scale`, so it stays sharp on outputs up to that scale.
    pub fn new(c: &Cursor, scale: i32) -> Pointer {
        let buffer = MemoryRenderBuffer::from_slice(
            &c.pixels,
            Fourcc::Argb8888,
            (c.w as i32, c.h as i32),
            scale,
            Transform::Normal,
            None,
        );
        let s = f64::from(scale);
        Pointer {
            buffer,
            hotspot: (f64::from(c.hotspot.0) / s, f64::from(c.hotspot.1) / s),
        }
    }
}

/// An output's wallpaper, uploaded on first use.
pub struct Wallpaper {
    buffer: MemoryRenderBuffer,
    size: (i32, i32),
}

impl Wallpaper {
    pub fn new(img: &Image) -> Wallpaper {
        let size = (img.w as i32, img.h as i32);
        let buffer = MemoryRenderBuffer::from_slice(
            &img.rgba,
            Fourcc::Abgr8888,
            size,
            1,
            Transform::Normal,
            None,
        );
        Wallpaper { buffer, size }
    }
}

/// A frame of a float's, tagged with the float.
type Owned<R> = (Option<layout::WinId>, Frame<R>);

/// How the pointer is drawn on this output.
pub enum Arrow<'a> {
    /// The host draws it (nested, no theme).
    None,
    Image(&'a Pointer),
    /// No cursor theme on real hardware: a small accent square, so the pointer is never lost.
    Square,
}

/// Everything `output` shows, top first. A wallpaper that fails to upload is dropped, so the
/// output is solid from then on.
pub fn elements<R>(
    renderer: &mut R,
    state: &Navi,
    output: &Output,
    arrow: Arrow,
    wallpaper: &mut Option<Wallpaper>,
    solids: &mut Solids,
) -> Vec<Frame<R>>
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let Some(geo) = state.space.output_geometry(output) else {
        return Vec::new();
    };
    let scale = output.current_scale().fractional_scale();
    if state.lock.active {
        return state
            .lock
            .surfaces
            .iter()
            .filter(|(o, s)| o == output && s.alive())
            .flat_map(|(_, s)| {
                render_elements_from_surface_tree::<R, Frame<R>>(
                    renderer,
                    s.wl_surface(),
                    (0, 0),
                    scale,
                    1.0,
                    Kind::Unspecified,
                )
            })
            .collect();
    }
    let origin = (geo.loc.x, geo.loc.y);
    let rect = |r: layout::Rect| {
        let r = monitors::to_local(r, origin, scale);
        Rectangle::new((r.x, r.y).into(), (r.w, r.h).into())
    };
    let pointer = state.pointer_px();
    let mut out: Vec<Frame<R>> = Vec::new();
    let mut decorations = Vec::new();
    // Only the square pointer draws over the shell's panels; tile decorations go under them.
    let mut above_panels = 0;
    let full = layout::Rect {
        x: geo.loc.x,
        y: geo.loc.y,
        w: geo.size.w,
        h: geo.size.h,
    };
    let on_output = pointer.0 >= full.x
        && pointer.0 < full.x + full.w
        && pointer.1 >= full.y
        && pointer.1 < full.y + full.h;
    match arrow {
        Arrow::Image(p) if on_output => {
            let loc = state.pointer_location() - geo.loc.to_f64() - Point::from(p.hotspot);
            let at = loc.to_physical(scale);
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                at,
                &p.buffer,
                None,
                None,
                None,
                Kind::Cursor,
            ) {
                Ok(e) => out.push(Frame::from(e)),
                Err(e) => warn!("cursor not drawn: {e:?}"),
            }
        }
        Arrow::Square if on_output => {
            above_panels = 1;
            let r = layout::Rect {
                x: pointer.0,
                y: pointer.1,
                w: 8,
                h: 8,
            };
            decorations.push((rect(r), theme::rgba(theme::ACCENT, 1.0)));
        }
        _ => {}
    }
    decorations.extend(
        layout::overlay(
            &state.layout,
            state.drag.as_ref(),
            pointer,
            state.now_ms(),
            state.focused_tile(),
        )
        .into_iter()
        .filter(|(r, _)| overlaps(*r, full))
        .map(|(r, color)| (rect(r), color)),
    );
    // Float decorations follow the tiles', so one buffer list keeps every id stable.
    let tile_decorations = decorations.len();
    let float_overlay = layout::float_overlay(
        &state.layout,
        state.drag.as_ref(),
        pointer,
        state.focused_tile(),
    );
    let float_overlay = float_overlay
        .into_iter()
        .filter(|(_, r, _)| overlaps(*r, full));
    let (owners, float_decorations): (Vec<layout::WinId>, Vec<_>) = float_overlay
        .map(|(id, r, color)| (id, (rect(r), color)))
        .unzip();
    decorations.extend(float_decorations);
    let buttons = layout::buttons(&state.layout, state.drag.as_ref(), pointer)
        .into_iter()
        .filter(|&(.., r, _)| overlaps(r, full))
        .map(|(id, b, r, lit)| (id, b, lit, rect(r)))
        .collect();
    let floating = |id| state.layout.tile(id).is_some_and(|t| t.floating);
    let (float_glyphs, mut glyphs): (Vec<_>, Vec<_>) = solids
        .glyphs(renderer, buttons)
        .into_iter()
        .partition(|(id, _)| floating(*id));
    // `Solids::elements` keeps order, and the square pointer is the first decoration.
    let mut tiles = solids.elements(decorations);
    let float_decorations = owners.into_iter().zip(tiles.split_off(tile_decorations));
    let mut pointer: Vec<_> = tiles.drain(..above_panels).collect();
    let (mut panels, mut lower) = surfaces(renderer, state, output, scale);
    // Every shown dialog's frame, so even a transparent one is seen: bright with the keyboard.
    let focus = state
        .seat
        .get_keyboard()
        .and_then(|kb| kb.current_focus())
        .and_then(|f| f.surface());
    let (framed, frame_rects): (Vec<_>, Vec<_>) = state
        .space
        .elements()
        .filter_map(|w| {
            let r = state.dialog_rect(w)?;
            let focused = focus.is_some() && crate::state::window_surface(w) == focus;
            Some((w.clone(), layout::dialog_frame(r, focused)))
        })
        .filter(|(_, f)| f.iter().any(|(r, _)| overlaps(*r, full)))
        .unzip();
    let frame_rects = frame_rects.into_iter().flatten();
    let mut frame_elements = solid(
        &mut solids.frames,
        frame_rects.map(|(r, k)| (rect(r), k)).collect(),
    )
    .into_iter();
    let frames: Vec<(Window, Vec<SolidColorRenderElement>)> = framed
        .into_iter()
        .map(|w| (w, frame_elements.by_ref().take(4).collect()))
        .collect();
    let (float_windows, mut windows) = windows(renderer, state, geo, scale, rect, frames);
    // Each float's buttons, bar and frame right above its own window and dialogs, front float
    // first, so a float in front covers all of a float behind it.
    let mut float_frames: Vec<Owned<R>> = float_glyphs
        .into_iter()
        .map(|(id, e)| (Some(id), Frame::from(e)))
        .chain(float_decorations.map(|(id, e)| (Some(id), Frame::from(e))))
        .chain(float_windows)
        .collect();
    let mut floats = Vec::new();
    for id in state.layout.floats().into_iter().map(Some).chain([None]) {
        let (mine, rest) = std::mem::take(&mut float_frames)
            .into_iter()
            .partition(|(o, _)| *o == id);
        float_frames = rest;
        floats.extend(mine.into_iter().map(|(_, f): (_, Frame<R>)| f));
    }
    floats.extend(float_frames.into_iter().map(|(_, f)| f));
    for part in STACK {
        match part {
            Part::Pointer => out.extend(pointer.drain(..).map(Frame::from)),
            Part::Panels => out.extend(panels.drain(..).map(Frame::from)),
            Part::Floats => out.append(&mut floats),
            Part::Glyphs => out.extend(glyphs.drain(..).map(|(_, e)| Frame::from(e))),
            Part::Decorations => out.extend(tiles.drain(..).map(Frame::from)),
            Part::Windows => {
                out.append(&mut windows);
                out.extend(lower.drain(..).map(Frame::from));
            }
        }
    }
    if let Some(w) = wallpaper.as_ref() {
        match wallpaper_element(renderer, w, (geo.size.w, geo.size.h)) {
            Ok(e) => out.push(Frame::from(e)),
            Err(e) => {
                warn!(
                    output = identity(output),
                    "wallpaper not drawn, solid from now on: {e:?}"
                );
                *wallpaper = None;
            }
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Part {
    Pointer,
    /// The Overlay and Top layers: navi-bar's panels.
    Panels,
    /// Floats, front first, each with its buttons, bar and frame above its window, dialogs and
    /// popups.
    Floats,
    /// The window buttons on navi's bars.
    Glyphs,
    /// navi's tile bars, frames, strips and drag previews.
    Decorations,
    /// Tiled windows, their dialogs and popups, and the lower layers.
    Windows,
}

/// Top first, above the wallpaper.
const STACK: [Part; 6] = [
    Part::Pointer,
    Part::Panels,
    Part::Floats,
    Part::Glyphs,
    Part::Decorations,
    Part::Windows,
];

fn overlaps(a: layout::Rect, b: layout::Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// The background colour behind everything.
pub fn clear() -> [f32; 4] {
    theme::rgba(theme::BG, 1.0)
}

/// Frame callbacks for what `output` shows, after a frame.
pub fn send_frames(state: &mut Navi, output: &Output) {
    let time = state.start_time.elapsed();
    if state.lock.active {
        for (o, s) in &state.lock.surfaces {
            if o == output && s.alive() {
                smithay::desktop::utils::send_frames_surface_tree(
                    s.wl_surface(),
                    output,
                    time,
                    Some(Duration::ZERO),
                    |_, _| Some(output.clone()),
                );
            }
        }
        return;
    }
    let on_output = |_: &_, _: &_| Some(output.clone());
    for window in state.space.elements_for_output(output) {
        window.send_frame(output, time, Some(Duration::ZERO), on_output);
    }
    let mut layers = layer_map_for_output(output);
    for layer in layers.layers() {
        layer.send_frame(output, time, Some(Duration::ZERO), on_output);
    }
    layers.cleanup();
}

/// The floating windows, by the float they belong to, and the tiled ones over `geo`, each top
/// first. A window's own surfaces are cropped to its clip rect, its popups to their configured
/// geometry on their output; a dialog's `frames` go right above it. `rect` maps a global logical
/// rect to `geo`'s physical px.
fn windows<R>(
    renderer: &mut R,
    state: &Navi,
    geo: Rectangle<i32, smithay::utils::Logical>,
    scale: f64,
    rect: impl Fn(layout::Rect) -> Physical,
    mut frames: Vec<(Window, Vec<SolidColorRenderElement>)>,
) -> (Vec<Owned<R>>, Vec<Frame<R>>)
where
    R: Renderer + ImportAll + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let (mut floats, mut tiles) = (Vec::new(), Vec::new());
    for w in state.space.elements().rev() {
        let (Some(loc), Some(bbox)) =
            (state.space.element_location(w), state.space.element_bbox(w))
        else {
            continue;
        };
        if !geo.overlaps(bbox) {
            continue;
        }
        let loc = (loc - w.geometry().loc - geo.loc).to_physical_precise_round(scale);
        let (float, clip) = state.stacking(w);
        let mut out = Vec::new();
        if let Some(i) = frames.iter().position(|(f, _)| f == w) {
            out.extend(frames.swap_remove(i).1.into_iter().map(Frame::from));
        }
        for (surface, at, popup) in state.popups(w) {
            let at = (at - geo.loc).to_physical_precise_round(scale);
            let k = Kind::Unspecified;
            let tree = render_elements_from_surface_tree(renderer, &surface, at, scale, 1.0, k);
            out.extend(
                tree.into_iter()
                    .filter_map(|e| CropRenderElement::from_element(e, scale, rect(popup)))
                    .map(Frame::from),
            );
        }
        let own = window_elements(renderer, w, loc, scale);
        match clip {
            Some(c) => out.extend(
                own.into_iter()
                    .filter_map(|e| CropRenderElement::from_element(e, scale, rect(c)))
                    .map(Frame::from),
            ),
            None => out.extend(own.into_iter().map(Frame::from)),
        }
        match float {
            true => {
                let owner = state.float_owner(w);
                floats.extend(out.into_iter().map(|f| (owner, f)));
            }
            false => tiles.extend(out),
        }
    }
    (floats, tiles)
}

/// A window's own surface tree, without its popups.
fn window_elements<R>(
    renderer: &mut R,
    w: &Window,
    loc: Point<i32, smithay::utils::Physical>,
    scale: f64,
) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R: Renderer + ImportAll,
    R::TextureId: Texture + Clone + Send + 'static,
{
    match w.underlying_surface() {
        WindowSurface::Wayland(t) => {
            let k = Kind::Unspecified;
            render_elements_from_surface_tree(renderer, t.wl_surface(), loc, scale, 1.0, k)
        }
        _ => AsRenderElements::<R>::render_elements(w, renderer, loc, scale.into(), 1.0),
    }
}

/// The output's upper layer surfaces, and its lower layers, top first. While a window is
/// fullscreen there, the Top layer (the taskbar) is left out.
fn surfaces<R>(
    renderer: &mut R,
    state: &Navi,
    output: &Output,
    scale: f64,
) -> (
    Vec<WaylandSurfaceRenderElement<R>>,
    Vec<WaylandSurfaceRenderElement<R>>,
)
where
    R: Renderer + ImportAll,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let map = layer_map_for_output(output);
    let upper: &[Layer] = match state.layout.fullscreen_on(&identity(output)) {
        true => &[Layer::Overlay],
        false => &[Layer::Overlay, Layer::Top],
    };
    let panels = layers(renderer, &map, upper, scale);
    let lower = layers(renderer, &map, &[Layer::Bottom, Layer::Background], scale);
    (panels, lower)
}

/// The surfaces on `which` layers, in that order, the most recently mapped first.
fn layers<R>(
    renderer: &mut R,
    map: &LayerMap,
    which: &[Layer],
    scale: f64,
) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R: Renderer + ImportAll,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let mut out = Vec::new();
    for &layer in which {
        for l in map.layers_on(layer).rev() {
            let Some(geo) = map.layer_geometry(l) else {
                continue;
            };
            out.extend(AsRenderElements::<R>::render_elements(
                l,
                renderer,
                geo.loc.to_physical_precise_round(scale),
                Scale::from(scale),
                1.0,
            ));
        }
    }
    out
}

/// The wallpaper filling a `w`×`h` (logical) output, scaled and cropped.
fn wallpaper_element<R>(
    renderer: &mut R,
    wp: &Wallpaper,
    (w, h): (i32, i32),
) -> Result<MemoryRenderBufferRenderElement<R>, R::Error>
where
    R: Renderer + ImportMem,
    R::TextureId: Texture + Clone + Send + 'static,
{
    let src = wallpaper::cover(wp.size.0, wp.size.1, w, h);
    let src = Rectangle::new((src.x, src.y).into(), (src.w, src.h).into()).to_f64();
    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        (0.0, 0.0),
        &wp.buffer,
        None,
        Some(src),
        Some((w, h).into()),
        Kind::Unspecified,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::backend::renderer::damage::OutputDamageTracker;

    #[test]
    fn panels_draw_above_window_bars() {
        let at = |p| STACK.iter().position(|&q| q == p).unwrap();
        assert_eq!(at(Part::Pointer), 0);
        assert!(at(Part::Panels) < at(Part::Glyphs));
        assert!(at(Part::Panels) < at(Part::Floats), "floats below navi");
        assert!(
            at(Part::Glyphs) < at(Part::Decorations),
            "buttons on their bar"
        );
        assert!(
            at(Part::Floats) < at(Part::Glyphs),
            "floats above every tile's decorations"
        );
        assert!(at(Part::Decorations) < at(Part::Windows));
    }

    #[test]
    fn static_decorations_have_no_damage_but_moves_colors_and_removal_do() {
        let mut solids = Solids::default();
        let mut tracker = OutputDamageTracker::new((100, 100), 1.0, Transform::Normal);
        let rect = Rectangle::new((10, 10).into(), (30, 20).into());
        let elements = solids.elements(vec![(rect, [1.0, 0.0, 0.0, 1.0])]);
        assert!(tracker.damage_output(1, &elements).unwrap().0.is_some());
        let elements = solids.elements(vec![(rect, [1.0, 0.0, 0.0, 1.0])]);
        assert!(tracker.damage_output(1, &elements).unwrap().0.is_none());
        let elements = solids.elements(vec![(rect, [0.0, 1.0, 0.0, 1.0])]);
        assert!(tracker.damage_output(1, &elements).unwrap().0.is_some());
        let elements = solids.elements(vec![(
            Rectangle::new((50, 10).into(), rect.size),
            [0.0, 1.0, 0.0, 1.0],
        )]);
        assert!(tracker.damage_output(1, &elements).unwrap().0.is_some());
        let elements = solids.elements(Vec::new());
        assert!(tracker.damage_output(1, &elements).unwrap().0.is_some());
        assert!(tracker.damage_output(1, &elements).unwrap().0.is_none());
    }
}
