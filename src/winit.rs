//! Nested backend: navi runs as a window inside another compositor, as one output.

use std::time::Duration;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ExportMem, Texture, TextureMapping,
            damage::OutputDamageTracker,
            element::{
                AsRenderElements, Id, Kind,
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                render_elements,
                solid::SolidColorRenderElement,
                surface::WaylandSurfaceRenderElement,
            },
            gles::{GlesError, GlesMapping, GlesRenderer},
            utils::CommitCounter,
        },
        winit::{self, WinitEvent},
    },
    desktop::{LayerMap, layer_map_for_output},
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Scale, Transform},
    wayland::shell::wlr_layer::Layer,
};
use tracing::warn;

use crate::{CalloopData, layout, screenshot::Screenshot, state::Navi, wallpaper};

const NAME: &str = "winit";
/// `#0B0D12`, the design's near-black: shown where there is no wallpaper.
const CLEAR: [f32; 4] = [11.0 / 255.0, 13.0 / 255.0, 18.0 / 255.0, 1.0];

render_elements! {
    /// Everything on screen, top first.
    Frame<=GlesRenderer>;
    Overlay=SolidColorRenderElement,
    Surface=WaylandSurfaceRenderElement<GlesRenderer>,
    Wallpaper=MemoryRenderBufferRenderElement<GlesRenderer>,
}

pub fn init(
    event_loop: &mut EventLoop<CalloopData>,
    data: &mut CalloopData,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;
    let mode = Mode {
        size: backend.window_size(),
        refresh: 60_000,
    };
    let output = Output::new(
        NAME.to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "navi".into(),
            model: "nested".into(),
        },
    );
    let _global = output.create_global::<Navi>(&data.state.display_handle);
    output.change_current_state(
        Some(mode),
        Some(Transform::Flipped180),
        None,
        Some((0, 0).into()),
    );
    output.set_preferred(mode);
    data.state.space.map_output(&output, (0, 0));
    data.state.add_output(&output, 0);

    // Copied into the buffer, so the decoded image is freed here.
    let mut wallpaper = wallpaper::open(&[(NAME, 0)])[0].as_ref().map(|img| {
        let size = (img.w as i32, img.h as i32);
        let buf = MemoryRenderBuffer::from_slice(
            &img.rgba,
            Fourcc::Abgr8888,
            size,
            1,
            Transform::Normal,
            None,
        );
        (buf, size)
    });

    let mut damage_tracker = OutputDamageTracker::from_output(&output);
    let mut shot = Screenshot::from_env();

    event_loop
        .handle()
        .insert_source(winit, move |event, _, data| {
            let state = &mut data.state;
            match event {
                WinitEvent::Resized { size, .. } => {
                    output.change_current_state(
                        Some(Mode {
                            size,
                            refresh: 60_000,
                        }),
                        None,
                        None,
                        None,
                    );
                    state.arrange_layers(&output);
                }
                WinitEvent::Input(event) => state.process_input_event(event),
                WinitEvent::Focus(false) => state.cancel_drag(),
                WinitEvent::Redraw => {
                    let size = backend.window_size();
                    let damage = Rectangle::from_size(size);
                    // The one output sits at (0, 0) at scale 1, so global logical px are its
                    // physical px.
                    let mut elements: Vec<Frame> = layout::overlay(
                        &state.layout,
                        state.drag.as_ref(),
                        state.pointer_px(),
                        state.now_ms(),
                    )
                    .into_iter()
                    .map(|(r, color)| {
                        let geo = Rectangle::new((r.x, r.y).into(), (r.w, r.h).into());
                        let commit = CommitCounter::default();
                        Frame::from(SolidColorRenderElement::new(
                            Id::new(),
                            geo,
                            commit,
                            color,
                            Kind::Unspecified,
                        ))
                    })
                    .collect();
                    let mut copy = None;
                    match backend.bind() {
                        Ok((renderer, mut framebuffer)) => {
                            let surfaces = surfaces(renderer, state, &output);
                            elements.extend(surfaces.into_iter().map(Frame::from));
                            if let Some((buffer, img)) = &wallpaper {
                                match wallpaper_element(renderer, buffer, *img, size.into()) {
                                    Ok(e) => elements.push(Frame::from(e)),
                                    Err(e) => {
                                        warn!("wallpaper not drawn, solid from now on: {e:?}");
                                        wallpaper = None;
                                    }
                                }
                            }
                            let rendered = damage_tracker.render_output(
                                renderer,
                                &mut framebuffer,
                                0,
                                &elements,
                                CLEAR,
                            );
                            if let Err(e) = rendered {
                                warn!("render failed: {e:?}");
                            } else if shot.as_mut().is_some_and(Screenshot::due) {
                                // Copied before the swap; mapped after it, since mapping drops
                                // the window surface from the GL context.
                                let region = Rectangle::from_size((size.w, size.h).into());
                                match renderer.copy_framebuffer(
                                    &framebuffer,
                                    region,
                                    Fourcc::Abgr8888,
                                ) {
                                    Ok(c) => copy = Some(c),
                                    Err(e) => {
                                        if let Some(s) = shot.as_mut() {
                                            s.fail(format!("copy: {e:?}"));
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => warn!("bind failed: {e:?}"),
                    }
                    if let Err(e) = backend.submit(Some(&[damage])) {
                        warn!("submit failed: {e:?}");
                    }
                    if let (Some(shot), Some(copy)) = (shot.as_mut(), copy) {
                        match upright(backend.renderer(), &copy) {
                            Ok(px) => shot.save(size.w as u32, size.h as u32, &px),
                            Err(e) => shot.fail(format!("readback: {e:?}")),
                        }
                    }

                    let time = state.start_time.elapsed();
                    let on_output = |_: &_, _: &_| Some(output.clone());
                    state.space.elements().for_each(|window| {
                        window.send_frame(&output, time, Some(Duration::ZERO), on_output)
                    });
                    {
                        let mut layers = layer_map_for_output(&output);
                        for layer in layers.layers() {
                            layer.send_frame(&output, time, Some(Duration::ZERO), on_output);
                        }
                        layers.cleanup();
                    }
                    state.space.refresh();
                    state.popups.cleanup();
                    state.refresh_toplevels();
                    let _ = data.display.flush_clients();
                    backend.window().request_redraw();
                }
                WinitEvent::CloseRequested => state.loop_signal.stop(),
                _ => (),
            }
        })?;
    Ok(())
}

/// The output's layer surfaces and windows, top first. While a window is fullscreen there, the
/// Top layer (the taskbar) is left out.
fn surfaces(
    renderer: &mut GlesRenderer,
    state: &Navi,
    output: &Output,
) -> Vec<WaylandSurfaceRenderElement<GlesRenderer>> {
    let map = layer_map_for_output(output);
    let upper: &[Layer] = match state.layout.fullscreen_on(&output.name()) {
        true => &[Layer::Overlay],
        false => &[Layer::Overlay, Layer::Top],
    };
    let mut out = layers(renderer, &map, upper);
    if let Some(geo) = state.space.output_geometry(output) {
        out.extend(
            state
                .space
                .render_elements_for_region(renderer, &geo, 1.0, 1.0),
        );
    }
    out.extend(layers(renderer, &map, &[Layer::Bottom, Layer::Background]));
    out
}

/// The surfaces on `which` layers, in that order, the most recently mapped first.
fn layers(
    renderer: &mut GlesRenderer,
    map: &LayerMap,
    which: &[Layer],
) -> Vec<WaylandSurfaceRenderElement<GlesRenderer>> {
    let mut out = Vec::new();
    for &layer in which {
        for l in map.layers_on(layer).rev() {
            let Some(geo) = map.layer_geometry(l) else {
                continue;
            };
            out.extend(AsRenderElements::<GlesRenderer>::render_elements(
                l,
                renderer,
                geo.loc.to_physical(1),
                Scale::from(1.0),
                1.0,
            ));
        }
    }
    out
}

/// A copied framebuffer as RGBA, top row first.
fn upright(renderer: &mut GlesRenderer, copy: &GlesMapping) -> Result<Vec<u8>, GlesError> {
    let px = renderer.map_texture(copy)?;
    let row = (copy.width().max(1) * 4) as usize;
    Ok(match copy.flipped() {
        true => px.chunks_exact(row).rev().flatten().copied().collect(),
        false => px.to_vec(),
    })
}

/// The wallpaper filling a `w`×`h` output, scaled and cropped.
fn wallpaper_element(
    renderer: &mut GlesRenderer,
    buffer: &MemoryRenderBuffer,
    (img_w, img_h): (i32, i32),
    (w, h): (i32, i32),
) -> Result<MemoryRenderBufferRenderElement<GlesRenderer>, GlesError> {
    let src = wallpaper::cover(img_w, img_h, w, h);
    let src = Rectangle::new((src.x, src.y).into(), (src.w, src.h).into()).to_f64();
    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        (0.0, 0.0),
        buffer,
        None,
        Some(src),
        Some((w, h).into()),
        Kind::Unspecified,
    )
}
