//! Nested backend: navi runs as a window inside another compositor, as one output.

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            ExportMem, Texture, TextureMapping,
            damage::OutputDamageTracker,
            gles::{GlesError, GlesMapping, GlesRenderer},
        },
        winit::{self, WinitEvent},
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::calloop::EventLoop,
    utils::{Rectangle, Transform},
};
use tracing::{info, warn};

use crate::{
    CalloopData,
    cursor::Cursor,
    render::{self, Arrow, Pointer, Wallpaper},
    screenshot::Screenshot,
    state::Navi,
    wallpaper,
};

const NAME: &str = "winit";

/// With a `cursor`, navi hides the host's pointer over its window and draws its own.
pub fn init(
    event_loop: &mut EventLoop<CalloopData>,
    data: &mut CalloopData,
    cursor: Option<Cursor>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut backend, winit) = winit::init::<GlesRenderer>()?;
    // Winit drops `CursorLeft`, so this arrow stays where the pointer left the window.
    let cursor = cursor.map(|c| {
        // Winit sends the host `wl_pointer.set_cursor` with no surface on every pointer enter.
        backend.window().set_cursor_visible(false);
        info!("host cursor hidden over navi's window");
        Pointer::new(&c, 1)
    });
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
    let mut wallpaper = wallpaper::open(&[(NAME, 0)])[0]
        .as_ref()
        .map(|img| Wallpaper::new(img));

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
                    let mut copy = None;
                    match backend.bind() {
                        Ok((renderer, mut framebuffer)) => {
                            let arrow = cursor.as_ref().map_or(Arrow::None, Arrow::Image);
                            let elements =
                                render::elements(renderer, state, &output, arrow, &mut wallpaper);
                            let rendered = damage_tracker.render_output(
                                renderer,
                                &mut framebuffer,
                                0,
                                &elements,
                                render::clear(),
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

                    render::send_frames(state, &output);
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

/// A copied framebuffer as RGBA, top row first.
fn upright(renderer: &mut GlesRenderer, copy: &GlesMapping) -> Result<Vec<u8>, GlesError> {
    let px = renderer.map_texture(copy)?;
    let row = (copy.width().max(1) * 4) as usize;
    Ok(match copy.flipped() {
        true => px.chunks_exact(row).rev().flatten().copied().collect(),
        false => px.to_vec(),
    })
}
