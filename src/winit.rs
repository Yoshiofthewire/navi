//! Nested backend: navi runs as a window inside another compositor, as one output.

use std::time::Duration;

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ExportMem, Texture, TextureMapping,
            damage::OutputDamageTracker,
            element::{Id, Kind, solid::SolidColorRenderElement},
            gles::{GlesError, GlesMapping, GlesRenderer},
            utils::CommitCounter,
        },
        winit::{self, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Transform},
};
use tracing::warn;

use crate::{
    CalloopData,
    layout::{self, Rect},
    screenshot::Screenshot,
    state::Navi,
};

const NAME: &str = "winit";

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
    set_grid(&mut data.state, mode.size.w, mode.size.h);

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
                    set_grid(state, size.w, size.h);
                    state.apply_layout();
                }
                WinitEvent::Input(event) => state.process_input_event(event),
                WinitEvent::Focus(false) => state.cancel_drag(),
                WinitEvent::Redraw => {
                    let size = backend.window_size();
                    let damage = Rectangle::from_size(size);
                    // The one output sits at (0, 0) at scale 1, so global logical px are its
                    // physical px.
                    let overlay: Vec<_> = layout::overlay(
                        &state.layout,
                        state.drag.as_ref(),
                        state.pointer_px(),
                        state.now_ms(),
                    )
                    .into_iter()
                    .map(|(r, color)| {
                        let geo = Rectangle::new((r.x, r.y).into(), (r.w, r.h).into());
                        let commit = CommitCounter::default();
                        SolidColorRenderElement::new(
                            Id::new(),
                            geo,
                            commit,
                            color,
                            Kind::Unspecified,
                        )
                    })
                    .collect();
                    let mut copy = None;
                    match backend.bind() {
                        Ok((renderer, mut framebuffer)) => {
                            if let Err(e) = smithay::desktop::space::render_output(
                                &output,
                                renderer,
                                &mut framebuffer,
                                1.0,
                                0,
                                [&state.space],
                                &overlay,
                                &mut damage_tracker,
                                [0.05, 0.05, 0.08, 1.0],
                            ) {
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

                    state.space.elements().for_each(|window| {
                        window.send_frame(
                            &output,
                            state.start_time.elapsed(),
                            Some(Duration::ZERO),
                            |_, _| Some(output.clone()),
                        )
                    });
                    state.space.refresh();
                    state.popups.cleanup();
                    let _ = data.display.flush_clients();
                    backend.window().request_redraw();
                }
                WinitEvent::CloseRequested => state.loop_signal.stop(),
                _ => (),
            }
        })?;
    Ok(())
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

fn set_grid(state: &mut Navi, w: i32, h: i32) {
    state.layout.set_outputs(vec![layout::Output {
        name: NAME.into(),
        area: Rect { x: 0, y: 0, w, h },
        priority: 0,
    }]);
}
