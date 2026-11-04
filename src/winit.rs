//! Nested backend: navi runs as a window inside another compositor, as one output.

use std::time::Duration;

use smithay::{
    backend::{
        renderer::{
            damage::OutputDamageTracker, element::surface::WaylandSurfaceRenderElement,
            gles::GlesRenderer,
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
                WinitEvent::Redraw => {
                    let size = backend.window_size();
                    let damage = Rectangle::from_size(size);
                    match backend.bind() {
                        Ok((renderer, mut framebuffer)) => {
                            if let Err(e) = smithay::desktop::space::render_output::<
                                _,
                                WaylandSurfaceRenderElement<GlesRenderer>,
                                _,
                                _,
                            >(
                                &output,
                                renderer,
                                &mut framebuffer,
                                1.0,
                                0,
                                [&state.space],
                                &[],
                                &mut damage_tracker,
                                [0.05, 0.05, 0.08, 1.0],
                            ) {
                                warn!("render failed: {e:?}");
                            }
                        }
                        Err(e) => warn!("bind failed: {e:?}"),
                    }
                    if let Err(e) = backend.submit(Some(&[damage])) {
                        warn!("submit failed: {e:?}");
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

fn set_grid(state: &mut Navi, w: i32, h: i32) {
    state.layout.set_outputs(vec![layout::Output {
        name: NAME.into(),
        area: Rect { x: 0, y: 0, w, h },
        priority: 0,
    }]);
}
