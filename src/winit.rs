//! Nested backend: navi runs as a window inside another compositor, as one output.

use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

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
    reexports::calloop::{
        EventLoop,
        ping::make_ping,
        timer::{TimeoutAction, Timer},
    },
    utils::{Rectangle, Transform},
};
use tracing::{debug, info, warn};

use crate::{
    CalloopData,
    cursor::Cursor,
    render::{self, Arrow, Pointer, Wallpaper},
    screenshot::Screenshot,
    state::Navi,
    wallpaper,
};

const NAME: &str = "winit";
const FRAME_TIME: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// Coalesce change notifications; no timer exists when the desktop is idle.
struct FrameClock {
    last: Instant,
    pending: bool,
}
impl FrameClock {
    fn request(&mut self, now: Instant) -> Option<Duration> {
        if self.pending {
            return None;
        }
        self.pending = true;
        Some((self.last + FRAME_TIME).saturating_duration_since(now))
    }
}

/// With a `cursor`, navi hides the host's pointer over its window and draws its own.
pub fn init(
    event_loop: &mut EventLoop<CalloopData>,
    data: &mut CalloopData,
    cursor: Option<Cursor>,
) -> Result<(), Box<dyn std::error::Error>> {
    let (backend, winit) = winit::init::<GlesRenderer>()?;
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

    let mut solids = render::Solids::default();
    let backend = Rc::new(RefCell::new(backend));
    let clock = Rc::new(RefCell::new(FrameClock {
        last: Instant::now() - FRAME_TIME,
        pending: false,
    }));
    let (ping, source) = make_ping()?;
    data.state.redraw = Some(ping.clone());
    let (draw, pacing, handle) = (backend.clone(), clock.clone(), event_loop.handle());
    event_loop.handle().insert_source(source, move |_, _, _| {
        let Some(delay) = pacing.borrow_mut().request(Instant::now()) else {
            return;
        };
        let (draw, timed_clock) = (draw.clone(), pacing.clone());
        if let Err(e) = handle.insert_source(Timer::from_duration(delay), move |_, _, _| {
            timed_clock.borrow_mut().pending = false;
            draw.borrow().window().request_redraw();
            TimeoutAction::Drop
        }) {
            pacing.borrow_mut().pending = false;
            warn!("redraw timer: {e}");
        }
    })?;
    // Capture instrumentation can request a fresh image each second; ordinary runs don't.
    if shot.is_some() {
        event_loop.handle().insert_source(
            Timer::from_duration(Duration::from_secs(1)),
            move |_, _, _| {
                ping.ping();
                TimeoutAction::ToDuration(Duration::from_secs(1))
            },
        )?;
    }

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
                    debug!("nested frame");
                    clock.borrow_mut().last = Instant::now();
                    let mut backend = backend.borrow_mut();
                    let size = backend.window_size();
                    let damage = Rectangle::from_size(size);
                    let mut copy = None;
                    let mut failed = false;
                    match backend.bind() {
                        Ok((renderer, mut framebuffer)) => {
                            let arrow = cursor.as_ref().map_or(Arrow::None, Arrow::Image);
                            let elements = render::elements(
                                renderer,
                                state,
                                &output,
                                arrow,
                                &mut wallpaper,
                                &mut solids,
                            );
                            let rendered = damage_tracker.render_output(
                                renderer,
                                &mut framebuffer,
                                0,
                                &elements,
                                render::clear(),
                            );
                            if let Err(e) = rendered {
                                failed = true;
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
                        Err(e) => {
                            failed = true;
                            warn!("bind failed: {e:?}");
                        }
                    }
                    if let Err(e) = backend.submit(Some(&[damage])) {
                        failed = true;
                        warn!("submit failed: {e:?}");
                    }
                    if let (Some(shot), Some(copy)) = (shot.as_mut(), copy) {
                        match upright(backend.renderer(), &copy) {
                            Ok(px) => shot.save(size.w as u32, size.h as u32, &px),
                            Err(e) => shot.fail(format!("readback: {e:?}")),
                        }
                    }

                    render::send_frames(state, &output);
                    let _ = data.display.flush_clients();
                    if failed || state.drag.is_some() {
                        state.request_redraw();
                    }
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changes_coalesce_and_callbacks_cannot_spin_faster_than_the_output() {
        let now = Instant::now();
        let mut clock = FrameClock {
            last: now,
            pending: false,
        };
        assert_eq!(clock.request(now), Some(FRAME_TIME));
        assert_eq!(clock.request(now + FRAME_TIME), None);
        clock.pending = false;
        assert_eq!(clock.request(now + FRAME_TIME * 2), Some(Duration::ZERO));
        clock.last = now + FRAME_TIME * 2;
        clock.pending = false;
        assert_eq!(clock.request(clock.last), Some(FRAME_TIME));
    }
}
