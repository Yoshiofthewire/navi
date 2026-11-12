//! What one output shows, for either backend: navi's overlay, the pointer, layer surfaces,
//! windows and the wallpaper, in the output's own physical px.

use std::time::Duration;

use smithay::{
    backend::{
        allocator::Fourcc,
        egl::EGLContext,
        renderer::{
            ImportAll, ImportMem, Renderer, Texture,
            element::{
                AsRenderElements, Element, Id, Kind, RenderElement, UnderlyingStorage,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                render_elements,
                solid::{SolidColorBuffer, SolidColorRenderElement},
                surface::{WaylandSurfaceRenderElement, render_elements_from_surface_tree},
                utils::CropRenderElement,
            },
            gles::{
                GlesError, GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, Uniform,
                UniformName, UniformType, UniformValue,
            },
            utils::{CommitCounter, DamageSet, OpaqueRegions},
        },
    },
    desktop::{LayerMap, Window, WindowSurface, layer_map_for_output},
    output::Output,
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    utils::{Buffer, Point, Rectangle, Scale, Size, Transform},
    wayland::shell::wlr_layer::Layer,
};
use tracing::{debug, warn};

use crate::{
    activation,
    cursor::{self, Cursor},
    image::Image,
    layout::{self, Cut, Shape},
    monitors,
    state::{CursorSurface, Navi, identity},
    theme, wallpaper,
};

render_elements! {
    /// Everything on screen, top first.
    pub Frame<R> where R: Gl;
    Overlay=SolidColorRenderElement,
    /// Chrome in a rounded outline's corner square.
    Corner=RoundedRect,
    Surface=WaylandSurfaceRenderElement<R>,
    /// A tiled or floating window's own surfaces, cropped to its clip rect.
    Clipped=CropRenderElement<WaylandSurfaceRenderElement<R>>,
    /// The same, reaching into its outline's corners.
    Rounded=Rounded<CropRenderElement<WaylandSurfaceRenderElement<R>>>,
    /// The wallpaper, the pointer and window buttons.
    Memory=MemoryRenderBufferRenderElement<R>,
    /// A window button in its bar's corner.
    RoundedMemory=Rounded<MemoryRenderBufferRenderElement<R>>,
}

/// A renderer navi can draw rounded outlines with: GLES, directly (nested) or through the DRM
/// backend's `MultiRenderer`.
pub trait Gl: Renderer + ImportAll + ImportMem {
    /// The GLES renderer underneath; `None` (a test renderer) draws everything square.
    fn gles(&mut self) -> Option<&mut GlesRenderer>;
    /// Runs `f` on the GLES frame underneath `frame`, if there is one.
    fn on_gles<'f, 'b>(
        frame: &mut Self::Frame<'f, 'b>,
        f: impl FnOnce(&mut GlesFrame<'f, 'b>) -> Result<(), GlesError>,
    ) -> Result<(), Self::Error>
    where
        'b: 'f,
        Self: 'f;
}

impl Gl for GlesRenderer {
    fn gles(&mut self) -> Option<&mut GlesRenderer> {
        Some(self)
    }
    fn on_gles<'f, 'b>(
        frame: &mut GlesFrame<'f, 'b>,
        f: impl FnOnce(&mut GlesFrame<'f, 'b>) -> Result<(), GlesError>,
    ) -> Result<(), GlesError>
    where
        'b: 'f,
    {
        f(frame)
    }
}

#[cfg(test)]
impl Gl for smithay::backend::renderer::test::DummyRenderer {
    fn gles(&mut self) -> Option<&mut GlesRenderer> {
        None
    }
    fn on_gles<'f, 'b>(
        _: &mut Self::Frame<'f, 'b>,
        _: impl FnOnce(&mut GlesFrame<'f, 'b>) -> Result<(), GlesError>,
    ) -> Result<(), Self::Error>
    where
        'b: 'f,
    {
        Ok(())
    }
}

/// The signed distance to a rounded rect and the coverage of a px centre at `p`, in output px.
/// Radii are top-left, top-right, bottom-right, bottom-left; a `hole` with no width is none.
const SHAPE_GLSL: &str = r#"
uniform vec4 shape;
uniform vec4 radii;
uniform vec4 hole;
uniform vec4 hole_radii;

float edge(vec2 p, vec4 r, vec4 k) {
    vec2 h = r.zw * 0.5;
    vec2 c = p - r.xy - h;
    float k1 = c.y < 0.0 ? (c.x < 0.0 ? k.x : k.y) : (c.x < 0.0 ? k.w : k.z);
    vec2 q = abs(c) - h + k1;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - k1;
}

float cover(vec2 p) {
    float a = clamp(0.5 - edge(p, shape, radii), 0.0, 1.0);
    if (hole.z > 0.0) {
        a *= clamp(0.5 + edge(p, hole, hole_radii), 0.0, 1.0);
    }
    return a;
}
"#;

/// Output px reach 4K and more, past mediump's exact range.
const PRECISION: &str = "
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif
";

/// A solid colour cut to the shape; `v_coords` spans the element, `origin` is where it starts.
const SOLID_FRAG: &str = r#"
uniform float alpha;
uniform vec2 size;
varying vec2 v_coords;
uniform vec4 color;
uniform vec2 origin;

void main() {
    gl_FragColor = color * alpha * cover(origin + v_coords * size);
}
"#;

/// Smithay's texture shader, cut to the shape. `to_px` takes `gl_FragCoord` to output px.
const TEXTURE_FRAG: &str = r#"#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif
"#;

const TEXTURE_MAIN: &str = r#"
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif
uniform mat3 to_px;

void main() {
    vec4 color = texture2D(tex, v_coords);
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif
    color = color * cover((to_px * vec3(gl_FragCoord.xy, 1.0)).xy);
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
"#;

/// The two programs, compiled once per GL context.
#[derive(Clone)]
struct Programs {
    solid: GlesPixelProgram,
    texture: GlesTexProgram,
}

fn shape_uniforms() -> Vec<UniformName<'static>> {
    ["shape", "radii", "hole", "hole_radii"]
        .map(|n| UniformName::new(n, UniformType::_4f))
        .into()
}

fn compile(r: &mut GlesRenderer) -> Result<Programs, GlesError> {
    let mut solid = shape_uniforms();
    solid.push(UniformName::new("color", UniformType::_4f));
    solid.push(UniformName::new("origin", UniformType::_2f));
    let mut texture = shape_uniforms();
    texture.push(UniformName::new("to_px", UniformType::Matrix3x3));
    Ok(Programs {
        solid: r
            .compile_custom_pixel_shader(format!("{PRECISION}{SHAPE_GLSL}{SOLID_FRAG}"), &solid)?,
        texture: r.compile_custom_texture_shader(
            format!("{TEXTURE_FRAG}{PRECISION}{SHAPE_GLSL}{TEXTURE_MAIN}"),
            &texture,
        )?,
    })
}

/// Whether `r`'s context has the programs, compiling them on first use. A failure is logged
/// once, and navi draws square from then on.
fn rounding(r: Option<&mut GlesRenderer>) -> bool {
    let Some(r) = r else {
        return false;
    };
    if let Some(p) = r.egl_context().user_data().get::<Option<Programs>>() {
        return p.is_some();
    }
    let p = compile(r)
        .inspect_err(|e| warn!("rounded corners off, drawing square: {e:?}"))
        .ok();
    let ok = p.is_some();
    r.egl_context().user_data().insert_if_missing(|| p);
    ok
}

fn programs(ctx: &EGLContext) -> Option<&Programs> {
    ctx.user_data().get::<Option<Programs>>()?.as_ref()
}

fn rect4(r: layout::Rect) -> UniformValue {
    UniformValue::_4f(r.x as f32, r.y as f32, r.w as f32, r.h as f32)
}

fn radii4(k: [i32; 4]) -> UniformValue {
    UniformValue::_4f(k[0] as f32, k[1] as f32, k[2] as f32, k[3] as f32)
}

fn cut_uniforms(c: &Cut) -> Vec<Uniform<'static>> {
    let none = Shape::new(layout::Rect::new(0, 0, 0, 0), 0);
    let hole = c.hole.unwrap_or(none);
    vec![
        Uniform::new("shape", rect4(c.shape.rect)),
        Uniform::new("radii", radii4(c.shape.radii)),
        Uniform::new("hole", rect4(hole.rect)),
        Uniform::new("hole_radii", radii4(hole.radii)),
    ]
}

/// Takes `gl_FragCoord` to output px for a frame whose output-px-to-clip-space `projection`
/// (column major) is this. The viewport is 1:1 in px, so its size is in the projection's scale.
fn frag_to_px(m: [f32; 9]) -> [f32; 9] {
    let m = m.map(f64::from);
    let (sx, sy) = (m[0].abs() + m[3].abs(), m[1].abs() + m[4].abs());
    // clip = A·px + t; clip = s·frag - 1.
    let det = m[0] * m[4] - m[3] * m[1];
    let inv = [m[4] / det, -m[3] / det, -m[1] / det, m[0] / det];
    let (tx, ty) = (-1.0 - m[6], -1.0 - m[7]);
    [
        inv[0] * sx,
        inv[2] * sx,
        0.0,
        inv[1] * sy,
        inv[3] * sy,
        0.0,
        inv[0] * tx + inv[1] * ty,
        inv[2] * tx + inv[3] * ty,
        1.0,
    ]
    .map(|v| v as f32)
}

fn physical(r: layout::Rect) -> Physical {
    Rectangle::new((r.x, r.y).into(), (r.w, r.h).into())
}

fn local(r: Physical) -> layout::Rect {
    layout::Rect::new(r.loc.x, r.loc.y, r.size.w, r.size.h)
}

/// A rect of navi's chrome inside one of an outline's corner squares, antialiased to it.
pub struct RoundedRect {
    id: Id,
    commit: CommitCounter,
    rect: Physical,
    color: [f32; 4],
    cut: Cut,
}

impl Element for RoundedRect {
    fn id(&self) -> &Id {
        &self.id
    }
    fn current_commit(&self) -> CommitCounter {
        self.commit
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        Rectangle::from_size(
            self.rect
                .size
                .to_f64()
                .to_logical(1.0)
                .to_buffer(1.0, Transform::Normal),
        )
    }
    fn geometry(&self, _scale: Scale<f64>) -> Physical {
        self.rect
    }
    fn kind(&self) -> Kind {
        Kind::Unspecified
    }
}

impl<R: Gl> RenderElement<R> for RoundedRect {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Physical,
        damage: &[Physical],
        _opaque: &[Physical],
    ) -> Result<(), R::Error> {
        R::on_gles(frame, |f| {
            let Some(p) = programs(f.egl_context()).cloned() else {
                return Ok(());
            };
            let mut uniforms = cut_uniforms(&self.cut);
            uniforms.push(Uniform::new("color", self.color));
            uniforms.push(Uniform::new("origin", (dst.loc.x as f32, dst.loc.y as f32)));
            let size = Size::<i32, Buffer>::from((dst.size.w, dst.size.h));
            f.render_pixel_shader_to(&p.solid, src, dst, size, Some(damage), 1.0, &uniforms)
        })
    }

    fn underlying_storage(&self, _: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

/// A texture element cut to `shape` (output px): its opaque regions lose the corner squares,
/// so what shows through a corner is drawn, and it is never scanned out directly.
pub struct Rounded<E> {
    inner: E,
    shape: Shape,
}

impl<E: Element> Element for Rounded<E> {
    fn id(&self) -> &Id {
        self.inner.id()
    }
    fn current_commit(&self) -> CommitCounter {
        self.inner.current_commit()
    }
    fn location(&self, scale: Scale<f64>) -> Point<i32, smithay::utils::Physical> {
        self.inner.location(scale)
    }
    fn src(&self) -> Rectangle<f64, Buffer> {
        self.inner.src()
    }
    fn transform(&self) -> Transform {
        self.inner.transform()
    }
    fn geometry(&self, scale: Scale<f64>) -> Physical {
        self.inner.geometry(scale)
    }
    fn damage_since(
        &self,
        scale: Scale<f64>,
        commit: Option<CommitCounter>,
    ) -> DamageSet<i32, smithay::utils::Physical> {
        self.inner.damage_since(scale, commit)
    }
    fn opaque_regions(&self, scale: Scale<f64>) -> OpaqueRegions<i32, smithay::utils::Physical> {
        // Opaque regions are relative to the element.
        let (at, r) = (self.inner.geometry(scale).loc, self.shape.rect);
        let rect = layout::Rect::new(r.x - at.x, r.y - at.y, r.w, r.h);
        let shape = Shape { rect, ..self.shape };
        self.inner
            .opaque_regions(scale)
            .into_iter()
            .flat_map(|r| shape.outside_corners(local(r)))
            .map(physical)
            .collect()
    }
    fn alpha(&self) -> f32 {
        self.inner.alpha()
    }
    fn kind(&self) -> Kind {
        self.inner.kind()
    }
}

impl<R: Gl, E: RenderElement<R>> RenderElement<R> for Rounded<E> {
    fn draw(
        &self,
        frame: &mut R::Frame<'_, '_>,
        src: Rectangle<f64, Buffer>,
        dst: Physical,
        damage: &[Physical],
        opaque: &[Physical],
    ) -> Result<(), R::Error> {
        R::on_gles(frame, |f| {
            if let Some(p) = programs(f.egl_context()).cloned() {
                let mut uniforms = cut_uniforms(&Cut::solid(self.shape));
                let matrices = vec![frag_to_px(*f.projection())];
                let to_px = UniformValue::Matrix3x3 {
                    matrices,
                    transpose: false,
                };
                uniforms.push(Uniform::new("to_px", to_px));
                f.override_default_tex_program(p.texture, uniforms);
            }
            Ok(())
        })?;
        let drawn = self.inner.draw(frame, src, dst, damage, opaque);
        R::on_gles(frame, |f| {
            f.clear_tex_program_override();
            Ok(())
        })?;
        drawn
    }

    fn underlying_storage(&self, _: &mut R) -> Option<UnderlyingStorage<'_>> {
        None
    }
}

/// An antialiased corner rect's stable identity and what it last drew.
struct CornerBuffer {
    id: Id,
    commit: CommitCounter,
    last: Option<(Physical, [f32; 4], Cut)>,
}

impl Default for CornerBuffer {
    fn default() -> Self {
        CornerBuffer {
            id: Id::new(),
            commit: CommitCounter::default(),
            last: None,
        }
    }
}

/// A window button's glyph texture: whose, which, lit, its px size and the colours it was
/// drawn in. One buffer per button keeps element ids unique and stable.
type GlyphKey = (layout::WinId, layout::Button, bool, u32, (u32, u32));
type Physical = Rectangle<i32, smithay::utils::Physical>;

/// Stable decoration identities let the damage tracker recognize an unchanged desktop.
#[derive(Default)]
pub struct Solids {
    rects: Vec<SolidColorBuffer>,
    corners: Vec<CornerBuffer>,
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

/// A rect of chrome in output px, its colour and what it is cut to.
type Piece = (layout::Rect, [f32; 4], Option<Cut>);

impl Solids {
    /// Each of `rects`' elements. A rect meeting its cut's corner squares is split: the part
    /// outside them stays solid, the part inside is antialiased to the cut (`rounded`: the
    /// programs compiled; else everything is square). A ring's corner squares are drawn whole,
    /// once, by the first of its rects to reach each.
    fn chrome<R: Gl>(&mut self, rects: Vec<Piece>, rounded: bool) -> Vec<Vec<Frame<R>>> {
        let n = rects.len();
        let (mut plain, mut corners) = (Vec::new(), Vec::new());
        let mut drawn: Vec<(Cut, usize)> = Vec::new();
        for (i, (r, k, cut)) in rects.into_iter().enumerate() {
            let Some(c) = cut.filter(|c| rounded && c.shape.meets(r)) else {
                plain.push((i, r, k));
                continue;
            };
            plain.extend(c.shape.outside_corners(r).into_iter().map(|p| (i, p, k)));
            for (at, sq) in c.shape.corners().into_iter().enumerate() {
                let Some(part) = layout::intersect(r, sq) else {
                    continue;
                };
                match c.hole {
                    Some(_) if drawn.contains(&(c, at)) => {}
                    Some(_) => {
                        drawn.push((c, at));
                        corners.push((i, sq, k, c));
                    }
                    None => corners.push((i, part, k, c)),
                }
            }
        }
        let mut out: Vec<Vec<Frame<R>>> = (0..n).map(|_| Vec::new()).collect();
        let solids = solid(
            &mut self.rects,
            plain.iter().map(|&(_, r, k)| (physical(r), k)).collect(),
        );
        for ((i, ..), e) in plain.iter().zip(solids) {
            out[*i].push(Frame::from(e));
        }
        self.corners
            .resize_with(corners.len(), CornerBuffer::default);
        for (b, (i, r, color, cut)) in self.corners.iter_mut().zip(corners) {
            let now = (physical(r), color, cut);
            if b.last != Some(now) {
                b.commit.increment();
                b.last = Some(now);
            }
            out[i].push(Frame::from(RoundedRect {
                id: b.id.clone(),
                commit: b.commit,
                rect: physical(r),
                color,
                cut,
            }));
        }
        out
    }

    /// The window buttons' glyphs, by window; textures of buttons no longer shown are dropped.
    fn glyphs<R>(
        &mut self,
        renderer: &mut R,
        buttons: Vec<(layout::WinId, layout::Button, bool, Physical)>,
    ) -> Vec<(layout::WinId, Physical, MemoryRenderBufferRenderElement<R>)>
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
                Ok(e) => out.push((id, r, e)),
                Err(e) => warn!("window button not drawn: {e:?}"),
            }
        }
        self.glyphs.retain(|k, _| used.contains(k));
        out
    }
}

/// navi's cursor shapes, each uploaded on first use at a whole-number buffer scale.
pub struct Cursors {
    shapes: cursor::Shapes<Pointer>,
    scale: i32,
    /// The shape drawn last, to log changes.
    last: &'static str,
}

impl Cursors {
    /// The user's theme loaded at `scale`, so it stays sharp on outputs up to that scale.
    pub fn from_env(scale: i32) -> Option<Cursors> {
        let shapes = cursor::from_env(f64::from(scale), |c| Pointer::new(c, scale))?;
        Some(Cursors {
            shapes,
            scale,
            last: "default",
        })
    }

    pub fn theme(&self) -> &str {
        &self.shapes.theme
    }

    /// How to draw `shape`.
    pub fn arrow(&mut self, shape: cursor::Shape<CursorSurface>) -> Arrow<'_> {
        let name = match shape {
            cursor::Shape::Named(n) => n,
            cursor::Shape::Client(_) => "client surface",
            cursor::Shape::Hidden => "hidden",
        };
        if name != self.last {
            debug!(shape = name, "cursor");
            self.last = name;
        }
        let scale = self.scale;
        match shape {
            cursor::Shape::Named(n) => Arrow::Image(self.shapes.get(n, |c| Pointer::new(c, scale))),
            cursor::Shape::Client(s) => Arrow::Client(s),
            cursor::Shape::Hidden => Arrow::None,
        }
    }
}

/// One cursor shape.
pub struct Pointer {
    buffer: MemoryRenderBuffer,
    /// In logical px.
    hotspot: (f64, f64),
}

impl Pointer {
    fn new(c: &Cursor, scale: i32) -> Pointer {
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
/// A tile-layer window's frames, tagged with the tile it is (`None` for a dialog or menu).
type Tiled<R> = (Option<layout::WinId>, Vec<Frame<R>>);

/// How the pointer is drawn on this output.
pub enum Arrow<'a> {
    /// The host draws it (nested, no theme).
    None,
    Image(&'a Pointer),
    /// A client's cursor surface that passed `cursor::fits`.
    Client(CursorSurface),
    /// No cursor theme on real hardware: a small accent square, so the pointer is never lost.
    Square,
}

impl Arrow<'_> {
    /// The client cursor surface this draws, for `send_frames`.
    pub fn surface(&self) -> Option<WlSurface> {
        match self {
            Arrow::Client((s, _)) => Some(s.clone()),
            _ => None,
        }
    }
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
    R: Gl,
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
    let to_local = move |r: layout::Rect| monitors::to_local(r, origin, scale);
    let rect = |r: layout::Rect| physical(to_local(r));
    let rounded = rounding(renderer.gles());
    let cut = |c: Option<Cut>| c.map(|c| c.scaled(to_local, scale));
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
    let on_output = has_pointer(state, output);
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
        Arrow::Client((s, hotspot)) if on_output => {
            let loc = state.pointer_location() - geo.loc.to_f64() - hotspot.to_f64();
            out.extend(render_elements_from_surface_tree::<R, Frame<R>>(
                renderer,
                &s,
                loc.to_physical(scale).to_i32_round(),
                scale,
                1.0,
                Kind::Cursor,
            ));
        }
        Arrow::Square if on_output => {
            above_panels = 1;
            let r = layout::Rect {
                x: pointer.0,
                y: pointer.1,
                w: 8,
                h: 8,
            };
            decorations.push((to_local(r), theme::rgba(theme::ACCENT, 1.0), None));
        }
        _ => {}
    }
    let under = state.zone_at(state.pointer_location());
    let (groups, tile_overlay): (Vec<Option<layout::WinId>>, Vec<_>) = layout::overlay(
        &state.layout,
        state.drag.as_ref(),
        under,
        state.now_ms(),
        state.focused_tile(),
    )
    .into_iter()
    .filter(|(_, r, ..)| overlaps(*r, full))
    .map(|(g, r, color, c)| (g, (to_local(r), color, cut(c))))
    .unzip();
    decorations.extend(tile_overlay);
    // Float decorations follow the tiles', so one buffer list keeps every id stable.
    let tile_decorations = decorations.len();
    let float_overlay = layout::float_overlay(
        &state.layout,
        state.drag.as_ref(),
        under,
        state.focused_tile(),
    );
    let float_overlay = float_overlay
        .into_iter()
        .filter(|(_, r, ..)| overlaps(*r, full));
    let (owners, float_decorations): (Vec<layout::WinId>, Vec<_>) = float_overlay
        .map(|(id, r, color, c)| (id, (to_local(r), color, cut(c))))
        .unzip();
    decorations.extend(float_decorations);
    let buttons = layout::buttons(&state.layout, state.drag.as_ref(), under)
        .into_iter()
        .filter(|&(.., r, _)| overlaps(r, full))
        .map(|(id, b, r, lit)| (id, b, lit, rect(r)))
        .collect();
    let floating = |id| state.layout.tile(id).is_some_and(|t| t.floating);
    // A button in its bar's corner is cut as the bar is.
    let glyph = |(id, r, e): (layout::WinId, Physical, _)| {
        let shape = state.layout.shape(id).map(|s| s.scaled(to_local, scale));
        let f = match shape.filter(|s| rounded && s.meets(local(r))) {
            Some(shape) => Frame::from(Rounded { inner: e, shape }),
            None => Frame::from(e),
        };
        (Some(id), f)
    };
    let (float_glyphs, glyphs): (Vec<_>, Vec<_>) = solids
        .glyphs(renderer, buttons)
        .into_iter()
        .partition(|(id, ..)| floating(*id));
    // `Solids::chrome` keeps order, and the square pointer is the first decoration.
    let mut tiles = solids.chrome::<R>(decorations, rounded);
    let float_decorations = owners
        .into_iter()
        .zip(tiles.split_off(tile_decorations))
        .flat_map(|(id, fs)| fs.into_iter().map(move |f| (Some(id), f)));
    let mut pointer: Vec<Frame<R>> = tiles.drain(..above_panels).flatten().collect();
    // Buttons on their bar, each group's chrome tagged for `tile_stack`.
    let chrome: Vec<(Option<layout::WinId>, Frame<R>)> = glyphs
        .into_iter()
        .map(glyph)
        .chain(
            groups
                .into_iter()
                .zip(tiles)
                .flat_map(|(g, fs)| fs.into_iter().map(move |f| (g, f))),
        )
        .collect();
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
            let (r, waiting) = state.dialog_rect(w)?;
            let focused = focus.is_some() && crate::state::window_surface(w) == focus;
            Some((w.clone(), layout::dialog_frame(r, focused, waiting)))
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
    let shaped = |s: Shape| s.scaled(to_local, scale);
    let (float_windows, windows) = windows(
        renderer,
        state,
        geo,
        scale,
        rect,
        rounded.then_some(&shaped),
        frames,
    );
    let mut tile_layer = layout::tile_stack(chrome, windows);
    // Each float's buttons, bar and frame right above its own window and dialogs, front float
    // first, so a float in front covers all of a float behind it.
    let mut float_frames: Vec<Owned<R>> = float_glyphs
        .into_iter()
        .map(glyph)
        .chain(float_decorations)
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
            Part::Pointer => out.append(&mut pointer),
            Part::Panels => out.extend(panels.drain(..).map(Frame::from)),
            Part::Floats => out.append(&mut floats),
            Part::Tiles => out.append(&mut tile_layer),
            Part::Lower => out.extend(lower.drain(..).map(Frame::from)),
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
    /// The tile layer (`layout::tile_stack`): the drag grid and preview, then tiled windows,
    /// dialogs, menus and their popups in space order, each group's bar, buttons and frame right
    /// above its window; minimized strips last.
    Tiles,
    /// The Bottom and Background layers.
    Lower,
}

/// Top first, above the wallpaper.
const STACK: [Part; 5] = [
    Part::Pointer,
    Part::Panels,
    Part::Floats,
    Part::Tiles,
    Part::Lower,
];

fn overlaps(a: layout::Rect, b: layout::Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// The background colour behind everything.
pub fn clear() -> [f32; 4] {
    theme::rgba(theme::BG, 1.0)
}

/// Whether the pointer is on `output`.
fn has_pointer(state: &Navi, output: &Output) -> bool {
    let (x, y) = state.pointer_px();
    state
        .space
        .output_geometry(output)
        .is_some_and(|g| g.contains((x, y)))
}

/// Frame callbacks for what `output` shows, after a frame; `cursor` is the cursor surface the
/// frame drew, if any.
pub fn send_frames(state: &mut Navi, output: &Output, cursor: Option<&WlSurface>) {
    let time = state.start_time.elapsed();
    track_layers(state, output);
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
    if let Some(s) = cursor.filter(|_| has_pointer(state, output)) {
        smithay::desktop::utils::send_frames_surface_tree(
            s,
            output,
            time,
            Some(Duration::ZERO),
            on_output,
        );
    }
    for window in state.space.elements_for_output(output) {
        window.send_frame(output, time, Some(Duration::ZERO), on_output);
    }
    let mut layers = layer_map_for_output(output);
    for layer in layers.layers() {
        layer.send_frame(output, time, Some(Duration::ZERO), on_output);
    }
    layers.cleanup();
}

/// Notes which of `output`'s layer surfaces are on screen after this frame, and since when.
fn track_layers(state: &mut Navi, output: &Output) {
    let (now, locked) = (std::time::Instant::now(), state.lock.active);
    let fullscreen = state.layout.fullscreen_on(&identity(output));
    let map = layer_map_for_output(output);
    for l in map.layers() {
        let shown = activation::layer_shown(l.layer() == Layer::Top, fullscreen, locked);
        activation::track_shown(&mut state.layers_shown, l.wl_surface(), shown, now);
    }
    state.layers_shown.retain(|(s, _)| s.is_alive());
}

/// The floating windows, by the float they belong to, and the tile layer's over `geo`, each
/// top first. A window's own surfaces are cropped to its clip rect, its popups to their
/// configured geometry on their output, both cut to the window's outline (`Navi::cut`) where
/// they reach its corners; a dialog's `frames` go right above it. `rect` maps a global logical
/// rect to `geo`'s physical px, `shaped` an outline (`None`: everything square).
fn windows<R>(
    renderer: &mut R,
    state: &Navi,
    geo: Rectangle<i32, smithay::utils::Logical>,
    scale: f64,
    rect: impl Fn(layout::Rect) -> Physical,
    shaped: Option<&dyn Fn(Shape) -> Shape>,
    mut frames: Vec<(Window, Vec<SolidColorRenderElement>)>,
) -> (Vec<Owned<R>>, Vec<Tiled<R>>)
where
    R: Gl,
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
        let shape = shaped.and_then(|f| Some(f(state.cut(w)?)));
        let cropped = |e, clip| {
            let e = CropRenderElement::from_element(e, scale, clip)?;
            let at = local(Element::geometry(&e, Scale::from(scale)));
            Some(match shape.filter(|s| s.meets(at)) {
                Some(shape) => Frame::from(Rounded { inner: e, shape }),
                None => Frame::from(e),
            })
        };
        let mut out = Vec::new();
        if let Some(i) = frames.iter().position(|(f, _)| f == w) {
            out.extend(frames.swap_remove(i).1.into_iter().map(Frame::from));
        }
        for (surface, at, popup) in state.popups(w) {
            let at = (at - geo.loc).to_physical_precise_round(scale);
            let k = Kind::Unspecified;
            let tree = render_elements_from_surface_tree(renderer, &surface, at, scale, 1.0, k);
            out.extend(tree.into_iter().filter_map(|e| cropped(e, rect(popup))));
        }
        let own = window_elements(renderer, w, loc, scale);
        match clip {
            Some(c) => out.extend(own.into_iter().filter_map(|e| cropped(e, rect(c)))),
            None => out.extend(own.into_iter().map(Frame::from)),
        }
        match float {
            true => {
                let owner = state.float_owner(w);
                floats.extend(out.into_iter().map(|f| (owner, f)));
            }
            false => tiles.push((state.tile_id(w), out)),
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
        assert!(at(Part::Panels) < at(Part::Tiles));
        assert!(at(Part::Panels) < at(Part::Floats), "floats below navi");
        assert!(
            at(Part::Floats) < at(Part::Tiles),
            "floats above the whole tile layer"
        );
        assert!(at(Part::Tiles) < at(Part::Lower));
    }

    #[test]
    fn static_decorations_have_no_damage_but_moves_colors_and_removal_do() {
        let mut solids = Solids::default();
        let mut tracker = OutputDamageTracker::new((100, 100), 1.0, Transform::Normal);
        let mut elements = |rects: Vec<(layout::Rect, [f32; 4])>| -> Vec<Frame<GlesRenderer>> {
            let rects = rects.into_iter().map(|(r, k)| (r, k, None)).collect();
            solids.chrome(rects, true).into_iter().flatten().collect()
        };
        let rect = layout::Rect::new(10, 10, 30, 20);
        let e = elements(vec![(rect, [1.0, 0.0, 0.0, 1.0])]);
        assert!(tracker.damage_output(1, &e).unwrap().0.is_some());
        let e = elements(vec![(rect, [1.0, 0.0, 0.0, 1.0])]);
        assert!(tracker.damage_output(1, &e).unwrap().0.is_none());
        let e = elements(vec![(rect, [0.0, 1.0, 0.0, 1.0])]);
        assert!(tracker.damage_output(1, &e).unwrap().0.is_some());
        let e = elements(vec![(layout::Rect { x: 50, ..rect }, [0.0, 1.0, 0.0, 1.0])]);
        assert!(tracker.damage_output(1, &e).unwrap().0.is_some());
        let e = elements(Vec::new());
        assert!(tracker.damage_output(1, &e).unwrap().0.is_some());
        assert!(tracker.damage_output(1, &e).unwrap().0.is_none());
    }

    /// AC5: rounded chrome built twice from one scene has no damage; a moved, recoloured,
    /// resized, added or removed piece has. A rect away from the corners stays a plain solid.
    #[test]
    fn rounded_chrome_is_stable_and_only_corners_are_shaded() {
        let mut solids = Solids::default();
        let mut tracker = OutputDamageTracker::new((300, 300), 1.0, Transform::Normal);
        let bar = layout::Rect::new(20, 20, 200, 24);
        let shape = Shape::new(layout::Rect { h: 200, ..bar }, theme::RADIUS);
        let ring = Cut::ring(shape.rect, theme::RADIUS, 2);
        let frame = |r: layout::Rect| -> [layout::Rect; 2] {
            [
                layout::Rect::new(r.x - 2, r.y - 2, r.w + 4, 2),
                layout::Rect::new(r.x - 2, r.y, 2, r.h),
            ]
        };
        let scene = |bar: layout::Rect, k: [f32; 4]| -> Vec<Piece> {
            let [top, left] = frame(shape.rect);
            vec![
                (bar, k, Some(Cut::solid(shape))),
                (top, k, Some(ring)),
                (left, k, Some(ring)),
            ]
        };
        let mut shaded = |rects: Vec<Piece>, rounded| -> Vec<Frame<GlesRenderer>> {
            solids
                .chrome(rects, rounded)
                .into_iter()
                .flatten()
                .collect()
        };
        let red = [1.0, 0.0, 0.0, 1.0];
        let e = shaded(scene(bar, red), true);
        let corners = e.iter().filter(|f| matches!(f, Frame::Corner(_))).count();
        // The bar's two top corners; the ring's top-left and top-right squares once each, by the
        // top line, and its bottom-left by the left line.
        assert_eq!(corners, 2 + 3, "{}", e.len());
        assert!(tracker.damage_output(1, &e).unwrap().0.is_some());
        let e = shaded(scene(bar, red), true);
        assert!(
            tracker.damage_output(1, &e).unwrap().0.is_none(),
            "unchanged"
        );
        let e = shaded(scene(bar, [0.0, 1.0, 0.0, 1.0]), true);
        assert!(
            tracker.damage_output(1, &e).unwrap().0.is_some(),
            "recoloured"
        );
        let e = shaded(scene(bar, red), true);
        assert!(tracker.damage_output(1, &e).unwrap().0.is_some());
        let mut moved = scene(bar, red);
        moved[0].2 = Some(Cut::solid(Shape {
            rect: layout::Rect {
                x: 21,
                ..shape.rect
            },
            ..shape
        }));
        let e = shaded(moved, true);
        assert!(
            tracker.damage_output(1, &e).unwrap().0.is_some(),
            "moved cut"
        );
        let e = shaded(scene(layout::Rect { w: 150, ..bar }, red), true);
        assert!(tracker.damage_output(1, &e).unwrap().0.is_some(), "resized");
        let e = shaded(scene(bar, red), true);
        assert!(tracker.damage_output(1, &e).unwrap().0.is_some());
        let mut more = scene(bar, red);
        more.push((
            layout::Rect::new(20, 210, 20, 10),
            red,
            Some(Cut::solid(shape)),
        ));
        let e = shaded(more, true);
        assert!(tracker.damage_output(1, &e).unwrap().0.is_some(), "added");
        let e = shaded(scene(bar, red), true);
        assert!(tracker.damage_output(1, &e).unwrap().0.is_some(), "removed");
        assert!(tracker.damage_output(1, &e).unwrap().0.is_none());

        // Between the corners, or with the programs missing: plain solids only.
        let middle = layout::Rect::new(40, 20, 100, 24);
        for (r, rounded) in [(middle, true), (bar, false)] {
            let e = shaded(vec![(r, red, Some(Cut::solid(shape)))], rounded);
            assert_eq!(e.len(), 1);
            assert!(matches!(e[0], Frame::Overlay(_)));
        }
        // Split pieces cover the rect exactly once.
        let pieces = shaded(vec![(bar, red, Some(Cut::solid(shape)))], true);
        let area: i32 = pieces
            .iter()
            .map(|f| Element::geometry(f, Scale::from(1.0)).size)
            .map(|s| s.w * s.h)
            .sum();
        assert_eq!(area, bar.w * bar.h);
    }

    /// `gl_FragCoord` maps back to the output px it was drawn from, for every output transform,
    /// with Smithay's projection (`GlesRenderer::render`) rebuilt here; nested is `Flipped180`.
    #[test]
    fn frag_coords_map_to_output_px() {
        let mul = |a: [f32; 9], b: [f32; 9]| {
            let mut m = [0.0; 9];
            for c in 0..3 {
                for r in 0..3 {
                    m[c * 3 + r] = (0..3).map(|k| a[k * 3 + r] * b[c * 3 + k]).sum();
                }
            }
            m
        };
        let apply = |m: [f32; 9], (x, y): (f32, f32)| {
            (m[0] * x + m[3] * y + m[6], m[1] * x + m[4] * y + m[7])
        };
        for t in [
            Transform::Normal,
            Transform::_90,
            Transform::_180,
            Transform::_270,
            Transform::Flipped,
            Transform::Flipped90,
            Transform::Flipped180,
            Transform::Flipped270,
        ] {
            let mode = Size::<i32, smithay::utils::Physical>::from((3440, 1440));
            let fb = t.transform_size(mode);
            let (w, h) = (fb.w as f32, fb.h as f32);
            let ortho = [2.0 / w, 0.0, 0.0, 0.0, -2.0 / h, 0.0, -1.0, 1.0, 1.0];
            let tm = t.matrix();
            let tm: &[f32; 9] = tm.as_ref();
            let flip = [1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 1.0];
            let proj = mul(mul(flip, *tm), ortho);
            let back = frag_to_px(proj);
            // Elements are in the transformed size; the viewport is the mode's.
            let (vw, vh) = (mode.w as f32, mode.h as f32);
            for p in [(0.5, 0.5), (w - 0.5, 0.5), (100.5, h - 0.5), (700.5, 720.5)] {
                let (cx, cy) = apply(proj, p);
                let frag = ((cx + 1.0) / 2.0 * vw, (cy + 1.0) / 2.0 * vh);
                assert!(frag.0 > 0.0 && frag.0 < vw && frag.1 > 0.0 && frag.1 < vh);
                let (x, y) = apply(back, frag);
                assert!(
                    (x - p.0).abs() < 1e-2 && (y - p.1).abs() < 1e-2,
                    "{t:?} {p:?}"
                );
            }
        }
    }
}
