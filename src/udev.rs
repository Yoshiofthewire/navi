//! Real hardware: a libseat session, the primary GPU's monitors through DRM/KMS, and libinput.
//! Devices are opened only through the session, and every startup failure is an error, never a
//! panic, so navi exits 1 and the display manager shows the login screen again.

use std::time::Duration;

use smithay::{
    backend::{
        allocator::{
            Fourcc,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode, NodeType,
            compositor::FrameFlags,
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        input::InputEvent,
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            gles::{GlesError, GlesFrame, GlesRenderer},
            multigpu::{self, GpuManager, MultiRenderer, gbm::GbmGlesBackend},
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu},
    },
    output::{Mode, Output, PhysicalProperties, Scale},
    reexports::{
        calloop::{
            EventLoop,
            timer::{TimeoutAction, Timer},
        },
        drm::control::{Device as _, ModeTypeFlags, connector, crtc},
        input::Libinput,
        rustix::fs::OFlags,
        wayland_server::backend::GlobalId,
    },
    utils::DeviceFd,
};
use tracing::{error, info, warn};

use crate::{
    CalloopData, cursor,
    files::xdg_dir,
    layout::Rect,
    monitors::{self, Arrangement, Mon},
    persist,
    render::{self, Arrow, Cursors, Frame, Wallpaper},
    state::{Navi, identity, set_identity},
    wallpaper,
};

type Gpu = GbmGlesBackend<GlesRenderer, DrmDeviceFd>;
type UdevRenderer<'a> = MultiRenderer<'a, 'a, Gpu, Gpu>;
type Allocator = GbmAllocator<DrmDeviceFd>;

/// The render GPU's GLES renderer and frame draw navi's rounded outlines.
impl render::Gl for UdevRenderer<'_> {
    fn gles(&mut self) -> Option<&mut GlesRenderer> {
        Some(self.as_mut())
    }
    fn on_gles<'f, 'b>(
        frame: &mut Self::Frame<'f, 'b>,
        f: impl FnOnce(&mut GlesFrame<'f, 'b>) -> Result<(), GlesError>,
    ) -> Result<(), Self::Error>
    where
        'b: 'f,
        Self: 'f,
    {
        f(frame.as_mut()).map_err(multigpu::Error::Render)
    }
}
type Exporter = GbmFramebufferExporter<DrmDeviceFd>;

/// The EDID blob read from a connector, at most.
const MAX_EDID: usize = 32 << 10;
/// Re-check for damage this often when a frame had none.
const IDLE: Duration = Duration::from_millis(16);

pub struct Udev {
    gpus: GpuManager<Gpu>,
    render_node: DrmNode,
    manager: DrmOutputManager<Allocator, Exporter, (), DrmDeviceFd>,
    screens: Vec<Screen>,
    input_devices: Vec<smithay::reexports::input::Device>,
    wallpaper_epoch: u64,
    pointer: Option<Cursors>,
    /// The session is active (not switched away).
    active: bool,
}

/// One connected monitor and what drives it.
struct Screen {
    connector: connector::Handle,
    crtc: crtc::Handle,
    output: Output,
    global: GlobalId,
    drm: DrmOutput<Allocator, Exporter, (), DrmDeviceFd>,
    wallpaper: Option<Wallpaper>,
    solids: render::Solids,
    /// A frame is queued and its vblank not yet seen.
    queued: bool,
    queued_lock: Option<u64>,
    /// A re-check for damage is timed.
    timed: bool,
    /// The last frame failed; logged once until one works.
    failing: bool,
    off: bool,
}

/// Starts navi on the seat's primary GPU. Returns the cursor theme and size apps should use.
pub fn init(
    event_loop: &mut EventLoop<'static, CalloopData>,
    data: &mut CalloopData,
) -> Result<Option<(String, u32)>, String> {
    let (mut session, notifier) =
        LibSeatSession::new().map_err(|e| format!("no libseat session: {e}"))?;
    let seat = session.seat();
    let path = primary_gpu(&seat)
        .map_err(|e| format!("no primary GPU: {e}"))?
        .ok_or("no primary GPU")?;
    for other in all_gpus(&seat).unwrap_or_default() {
        if other != path {
            info!(gpu = %other.display(), "GPU ignored: navi drives one GPU");
        }
    }
    let flags = OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK;
    let fd = session
        .open(&path, flags)
        .map_err(|e| format!("the GPU {} failed to open: {e}", path.display()))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let node = DrmNode::from_file(&fd).map_err(|e| format!("the GPU failed to open: {e}"))?;
    let (drm, drm_events) =
        DrmDevice::new(fd.clone(), true).map_err(|e| format!("the GPU failed to open: {e}"))?;
    let gbm = GbmDevice::new(fd).map_err(|e| format!("the GPU failed to open: {e}"))?;
    let render_node = node
        .node_with_type(NodeType::Render)
        .and_then(Result::ok)
        .unwrap_or(node);
    let mut gpus = GpuManager::new(Gpu::default()).map_err(|e| format!("renderer: {e}"))?;
    gpus.as_mut()
        .add_node(render_node, gbm.clone())
        .map_err(|e| format!("renderer: {e}"))?;
    let render_formats = gpus
        .single_renderer(&render_node)
        .map_err(|e| format!("renderer: {e}"))?
        .as_mut()
        .egl_context()
        .dmabuf_render_formats()
        .clone();
    info!(gpu = %path.display(), %render_node, "GPU");
    let manager = DrmOutputManager::new(
        drm,
        GbmAllocator::new(
            gbm.clone(),
            GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
        ),
        GbmFramebufferExporter::new(gbm.clone(), Some(render_node)),
        Some(gbm),
        [Fourcc::Argb8888, Fourcc::Xrgb8888],
        render_formats,
    );
    data.state.session = Some(session.clone());
    data.udev = Some(Udev {
        gpus,
        render_node,
        manager,
        screens: Vec::new(),
        input_devices: Vec::new(),
        wallpaper_epoch: 0,
        pointer: None,
        active: true,
    });

    let handle = event_loop.handle();
    let mut libinput =
        Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat)
        .map_err(|()| format!("libinput: seat {seat} not assigned"))?;
    handle
        .insert_source(
            LibinputInputBackend::new(libinput.clone()),
            |mut ev, _, data| {
                match &mut ev {
                    InputEvent::DeviceAdded { device } => {
                        configure_input(device, &data.state.preferences);
                        if let Some(udev) = data.udev.as_mut() {
                            udev.input_devices.push(device.clone());
                        }
                    }
                    InputEvent::DeviceRemoved { device } => {
                        if let Some(udev) = data.udev.as_mut() {
                            udev.input_devices.retain(|d| d != device);
                        }
                    }
                    _ => {}
                }
                data.state.process_input_event(ev);
                render_all(data);
            },
        )
        .map_err(|e| format!("input: {e}"))?;
    handle
        .insert_source(notifier, move |event, _, data| {
            let Some(udev) = data.udev.as_mut() else {
                return;
            };
            match event {
                SessionEvent::PauseSession => {
                    info!("session paused");
                    libinput.suspend();
                    udev.manager.pause();
                    udev.active = false;
                }
                SessionEvent::ActivateSession => {
                    info!("session active");
                    if libinput.resume().is_err() {
                        error!("input not resumed");
                    }
                    if let Err(e) = udev.manager.activate(false) {
                        error!("GPU not re-activated: {e}");
                    }
                    udev.active = true;
                    for s in &mut udev.screens {
                        s.queued = false;
                    }
                    // Monitors may have changed while switched away.
                    if let Err(e) = scan(data) {
                        warn!("monitors not rescanned: {e}");
                    }
                    render_all(data);
                }
            }
        })
        .map_err(|e| format!("session: {e}"))?;
    handle
        .insert_source(drm_events, |event, _, data| match event {
            DrmEvent::VBlank(crtc) => vblank(data, crtc),
            DrmEvent::Error(e) => warn!("GPU: {e}"),
        })
        .map_err(|e| format!("GPU events: {e}"))?;
    let udev_events = UdevBackend::new(&seat).map_err(|e| format!("udev: {e}"))?;
    let ours = node.dev_id();
    handle
        .insert_source(udev_events, move |event, _, data| match event {
            // Without DRM master nothing can be set up; resuming rescans.
            UdevEvent::Changed { device_id }
                if device_id == ours && data.udev.as_ref().is_some_and(|u| u.active) =>
            {
                if let Err(e) = scan(data) {
                    warn!("monitors not rescanned: {e}");
                }
            }
            UdevEvent::Removed { device_id } if device_id == ours => {
                error!("the GPU was removed");
                data.state.loop_signal.stop();
            }
            _ => {}
        })
        .map_err(|e| format!("udev: {e}"))?;

    if scan(data)? == 0 {
        return Err("no connected monitor".into());
    }
    let scale = data
        .state
        .space
        .outputs()
        .map(|o| o.current_scale().fractional_scale())
        .fold(1.0, f64::max)
        .ceil();
    let theme = Cursors::from_env(scale as i32).map(|c| {
        let theme = (c.theme().to_string(), cursor::SIZE.round() as u32);
        if let Some(u) = data.udev.as_mut() {
            u.pointer = Some(c);
        }
        theme
    });
    if let Some(o) = data.state.main_output()
        && let Some(g) = data.state.space.output_geometry(&o)
    {
        let c = g.loc.to_f64() + g.size.to_f64().downscale(2.0).to_point();
        data.state.pointer_motion(c, 0);
    }
    render_all(data);
    Ok(theme)
}

/// A connected connector.
struct Conn {
    handle: connector::Handle,
    name: String,
    info: connector::Info,
    edid: Option<Vec<u8>>,
}

fn connected(drm: &DrmDevice) -> Result<Vec<Conn>, String> {
    let res = drm.resource_handles().map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for &handle in res.connectors() {
        let Ok(info) = drm.get_connector(handle, true) else {
            continue;
        };
        if info.state() != connector::State::Connected || info.modes().is_empty() {
            continue;
        }
        let name = format!("{}-{}", info.interface().as_str(), info.interface_id());
        out.push(Conn {
            handle,
            name,
            edid: edid(drm, handle),
            info,
        });
    }
    Ok(out)
}

/// The connector's `EDID` property blob, if it has one of at most `MAX_EDID` bytes.
fn edid(drm: &DrmDevice, handle: connector::Handle) -> Option<Vec<u8>> {
    let props = drm.get_properties(handle).ok()?;
    let (ids, values) = props.as_props_and_values();
    let at = ids.iter().position(|&id| {
        drm.get_property(id)
            .is_ok_and(|p| p.name().to_bytes() == b"EDID")
    })?;
    let blob = values[at];
    if blob == 0 {
        return None;
    }
    drm.get_property_blob(blob)
        .ok()
        .filter(|b| b.len() <= MAX_EDID)
}

/// A free CRTC that can drive `info`.
fn crtc_for(
    drm: &DrmDevice,
    info: &connector::Info,
    taken: &[crtc::Handle],
) -> Option<crtc::Handle> {
    let res = drm.resource_handles().ok()?;
    info.encoders()
        .iter()
        .filter_map(|&e| drm.get_encoder(e).ok())
        .flat_map(|e| res.filter_crtcs(e.possible_crtcs()))
        .find(|c| !taken.contains(c))
}

/// Brings the outputs in line with the connected monitors, then arranges them. Returns how many
/// are connected.
fn scan(data: &mut CalloopData) -> Result<usize, String> {
    let Some(udev) = data.udev.as_mut() else {
        return Ok(0);
    };
    let state = &mut data.state;
    let conns = connected(udev.manager.device())?;
    let (gone, kept): (Vec<Screen>, Vec<Screen>) = std::mem::take(&mut udev.screens)
        .into_iter()
        .partition(|s| !conns.iter().any(|c| c.handle == s.connector));
    udev.screens = kept;
    for s in gone {
        state.remove_output(&s.output);
        state.display_handle.remove_global::<Navi>(s.global);
    }

    // New monitors get identities that the ones still connected don't hold.
    let new: Vec<&Conn> = conns
        .iter()
        .filter(|c| !udev.screens.iter().any(|s| s.connector == c.handle))
        .collect();
    let names: Vec<(&str, Option<&[u8]>)> = new
        .iter()
        .map(|c| (c.name.as_str(), c.edid.as_deref()))
        .collect();
    let held: Vec<String> = udev.screens.iter().map(|s| identity(&s.output)).collect();
    let ids = monitors::identities(&names, &held);

    let saved = crate::settings::saved_arrangement();
    let mut added = Vec::new();
    for (c, (id, fallback)) in new.into_iter().zip(&ids) {
        if *fallback {
            warn!(
                connector = c.name,
                "no valid EDID: the monitor is known by its connector"
            );
        }
        let taken: Vec<crtc::Handle> = udev.screens.iter().map(|s| s.crtc).collect();
        let Some(crtc) = crtc_for(udev.manager.device(), &c.info, &taken) else {
            warn!(connector = c.name, "no free CRTC: monitor not used");
            continue;
        };
        let modes = c.info.modes();
        let wanted = saved.as_ref().and_then(|a| a.modes.get(id));
        let mode = modes
            .iter()
            .find(|m| {
                let m = Mode::from(**m);
                wanted == Some(&(m.size.w, m.size.h, m.refresh))
            })
            .or_else(|| {
                modes
                    .iter()
                    .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            })
            .unwrap_or(&modes[0]);
        let (make, model) = c
            .edid
            .as_deref()
            .and_then(monitors::make_model)
            .unwrap_or_else(|| ("Unknown".into(), c.name.clone()));
        let (mm_w, mm_h) = c.info.size().unwrap_or((0, 0));
        // Clients see the connector, never the EDID's serial and date.
        let output = Output::new(
            c.name.clone(),
            PhysicalProperties {
                size: (mm_w as i32, mm_h as i32).into(),
                subpixel: c.info.subpixel().into(),
                make,
                model,
            },
        );
        set_identity(&output, id.clone());
        for mode in modes {
            output.add_mode(Mode::from(*mode));
        }
        let preferred = modes
            .iter()
            .find(|m| m.mode_type().contains(ModeTypeFlags::PREFERRED))
            .unwrap_or(&modes[0]);
        let mut wl_mode = Mode::from(*mode);
        output.set_preferred(Mode::from(*preferred));
        output.change_current_state(Some(wl_mode), None, None, None);
        let mut renderer = udev
            .gpus
            .single_renderer(&udev.render_node)
            .map_err(|e| format!("renderer: {e}"))?;
        let drm = udev
            .manager
            .initialize_output::<_, Frame<UdevRenderer<'_>>>(
                crtc,
                *mode,
                &[c.handle],
                &output,
                None,
                &mut renderer,
                &DrmOutputRenderElements::default(),
            );
        let drm = drm.or_else(|error| {
            if mode == preferred {
                return Err(error);
            }
            warn!(
                connector = c.name,
                "saved mode failed; trying preferred mode"
            );
            wl_mode = Mode::from(*preferred);
            output.change_current_state(Some(wl_mode), None, None, None);
            udev.manager
                .initialize_output::<_, Frame<UdevRenderer<'_>>>(
                    crtc,
                    *preferred,
                    &[c.handle],
                    &output,
                    None,
                    &mut renderer,
                    &DrmOutputRenderElements::default(),
                )
        });
        let drm = match drm {
            Ok(d) => d,
            Err(e) => {
                warn!(connector = c.name, "monitor not used: {e}");
                continue;
            }
        };
        let global = output.create_global::<Navi>(&state.display_handle);
        let (w, h) = (wl_mode.size.w, wl_mode.size.h);
        info!(
            output = id,
            connector = c.name,
            w,
            h,
            refresh = wl_mode.refresh,
            "monitor"
        );
        added.push(output.clone());
        udev.screens.push(Screen {
            connector: c.handle,
            crtc,
            output,
            global,
            drm,
            wallpaper: None,
            solids: render::Solids::default(),
            queued: false,
            queued_lock: None,
            timed: false,
            failing: false,
            off: false,
        });
    }

    let mons: Vec<Mon> = udev
        .screens
        .iter()
        .map(|s| {
            let size = s
                .output
                .current_mode()
                .map_or((1, 1), |m| (m.size.w, m.size.h));
            let connector = conns
                .iter()
                .find(|c| c.handle == s.connector)
                .map_or_else(String::new, |c| c.name.clone());
            let rect = Rect {
                x: 0,
                y: 0,
                w: size.0,
                h: size.1,
            };
            Mon {
                connector,
                id: identity(&s.output),
                size,
                rect,
            }
        })
        .collect();
    let (placed, pinned) = arrangement(&mons);
    let mut mons = mons;
    for ((m, s), (rect, scale)) in mons.iter_mut().zip(&udev.screens).zip(&placed) {
        m.rect = *rect;
        let pos = (rect.x, rect.y);
        s.output.change_current_state(
            None,
            None,
            Some(Scale::Fractional(*scale)),
            Some(pos.into()),
        );
        state.space.map_output(&s.output, pos);
        info!(output = m.id, ?rect, scale, "monitor placed");
    }
    let main = monitors::main(&mons, pinned.as_deref());
    let order: Vec<String> = monitors::priority(&mons, main)
        .into_iter()
        .map(|i| mons[i].id.clone())
        .collect();
    info!(?order, "monitor priority");
    for o in &added {
        state.add_output(o, u32::MAX);
    }
    for s in &udev.screens {
        state.arrange_layers(&s.output);
    }
    state.set_priorities(&order);
    state.place_shell();

    let outputs: Vec<(String, u32)> = udev
        .screens
        .iter()
        .map(|s| (identity(&s.output), state.priority_of(&identity(&s.output))))
        .collect();
    let named: Vec<(&str, u32)> = outputs.iter().map(|(n, p)| (n.as_str(), *p)).collect();
    for (s, img) in udev.screens.iter_mut().zip(wallpaper::open(&named)) {
        s.wallpaper = img.map(|i| Wallpaper::new(&i));
    }
    let n = udev.screens.len();
    crate::settings::restore_outputs(data);
    if n > 0 {
        render_all(data);
    }
    Ok(n)
}

/// Where each monitor goes, and the pinned main monitor: from the monitors file, else imported
/// from KWin on first run, else one row. A bad file is ignored and never rewritten.
fn arrangement(mons: &[Mon]) -> (Vec<(Rect, f64)>, Option<String>) {
    let var = std::env::var_os;
    let Some(dir) = xdg_dir(
        var("XDG_CONFIG_HOME").as_deref(),
        var("HOME").as_deref(),
        ".config",
    ) else {
        warn!("no absolute XDG_CONFIG_HOME or HOME: monitors in one row, not saved");
        return (monitors::auto(mons).place(mons).0, None);
    };
    let file = dir.join("navi/monitors");
    let shown = file.display();
    let (mut a, save) = match persist::read(&file) {
        Ok(Some(text)) => match Arrangement::parse(&text) {
            Ok(a) => (a, false),
            Err(e) => {
                warn!(path = %shown, "monitors file ignored, monitors in one row: {e}");
                let mut a = monitors::auto(mons);
                return (a.place(mons).0, None);
            }
        },
        Ok(None) => {
            let kwin = dir.join("kwinoutputconfig.json");
            let imported = match persist::read(&kwin) {
                Ok(Some(json)) => monitors::import(&json, mons),
                Ok(None) => None,
                Err(e) => {
                    warn!(path = %kwin.display(), "KWin layout not read: {e}");
                    None
                }
            };
            match imported.as_deref().map(Arrangement::parse) {
                Some(Ok(a)) => {
                    info!(path = %kwin.display(), "monitors imported from KWin");
                    (a, true)
                }
                _ => {
                    info!("no KWin layout for these monitors: one row");
                    (monitors::auto(mons), true)
                }
            }
        }
        Err(e) => {
            warn!(path = %shown, "monitors file ignored, monitors in one row: {e}");
            let mut a = monitors::auto(mons);
            return (a.place(mons).0, None);
        }
    };
    let (placed, changed) = a.place(mons);
    if save || changed {
        match persist::write(&file, &a.text()) {
            Ok(()) => info!(path = %shown, "monitors saved"),
            Err(e) => warn!(path = %shown, "monitors not saved: {e}"),
        }
    }
    (placed, a.main)
}

fn render_all(data: &mut CalloopData) {
    let crtcs: Vec<crtc::Handle> = data
        .udev
        .as_ref()
        .map(|u| u.screens.iter().map(|s| s.crtc).collect())
        .unwrap_or_default();
    for crtc in crtcs {
        render(data, crtc);
    }
}

fn vblank(data: &mut CalloopData, crtc: crtc::Handle) {
    let Some(s) = data
        .udev
        .as_mut()
        .and_then(|u| u.screens.iter_mut().find(|s| s.crtc == crtc))
    else {
        return;
    };
    s.queued = false;
    match s.drm.frame_submitted() {
        Ok(_) => {
            if let Some(epoch) = s.queued_lock.take() {
                data.state.lock_presented(&s.output, epoch);
            }
        }
        Err(e) => {
            s.queued_lock = None;
            warn!(output = identity(&s.output), "frame not submitted: {e}");
        }
    }
    render(data, crtc);
}

/// Renders `crtc`'s output if its last frame is on screen. No damage, or an error, re-checks
/// after `IDLE`.
fn render(data: &mut CalloopData, crtc: crtc::Handle) {
    let Some(udev) = data.udev.as_mut() else {
        return;
    };
    if !udev.active {
        return;
    }
    if udev.wallpaper_epoch != data.state.wallpaper_epoch {
        let outputs: Vec<_> = udev
            .screens
            .iter()
            .map(|s| {
                (
                    identity(&s.output),
                    data.state.priority_of(
                        crate::settings::output_preferences(&s.output)
                            .mirror
                            .as_deref()
                            .unwrap_or(&identity(&s.output)),
                    ),
                )
            })
            .collect();
        let named: Vec<_> = outputs.iter().map(|(n, p)| (n.as_str(), *p)).collect();
        for (screen, image) in udev.screens.iter_mut().zip(wallpaper::open(&named)) {
            screen.wallpaper = image.map(|i| Wallpaper::new(&i));
        }
        udev.wallpaper_epoch = data.state.wallpaper_epoch;
    }
    let Some(s) = udev.screens.iter_mut().find(|s| s.crtc == crtc) else {
        return;
    };
    if data.state.screen_off || !crate::settings::output_preferences(&s.output).enabled {
        if s.queued {
            s.queued_lock = None;
        }
        if !s.off && s.drm.with_compositor(|c| c.surface().clear()).is_ok() {
            s.off = true;
        }
        s.queued = false;
        return;
    }
    if s.off {
        s.off = false;
        s.drm.reset_buffers();
        s.queued = false;
    }
    if s.queued {
        return;
    }
    if data.state.lock_needs_present(&s.output) {
        // Force presentation even if the new protected frame has identical pixels.
        s.drm.reset_buffers();
    }
    let mut renderer = match udev.gpus.single_renderer(&udev.render_node) {
        Ok(r) => r,
        Err(e) => {
            warn!("renderer: {e}");
            return;
        }
    };
    let shape = data.state.cursor_shape();
    let arrow = udev
        .pointer
        .as_mut()
        .map_or(Arrow::Square, |c| c.arrow(shape));
    let mirror = crate::settings::output_preferences(&s.output).mirror;
    let source = if data.state.lock.active {
        s.output.clone()
    } else {
        mirror
            .and_then(|id| {
                data.state
                    .space
                    .outputs()
                    .find(|o| identity(o) == id)
                    .cloned()
            })
            .unwrap_or_else(|| s.output.clone())
    };
    let elements = render::elements(
        &mut renderer,
        &data.state,
        &source,
        arrow,
        &mut s.wallpaper,
        &mut s.solids,
    );
    let frame = s
        .drm
        .render_frame(
            &mut renderer,
            &elements,
            render::clear(),
            FrameFlags::DEFAULT,
        )
        .map_err(|e| e.to_string())
        .and_then(|r| match r.is_empty {
            true => Ok(false),
            false => s
                .drm
                .queue_frame(())
                .map(|()| true)
                .map_err(|e| e.to_string()),
        });
    let output = s.output.clone();
    match frame {
        Ok(queued) => {
            s.queued = queued;
            s.queued_lock = (queued && data.state.lock.active).then_some(data.state.lock.epoch);
            s.failing = false;
        }
        Err(e) => {
            if !s.failing {
                warn!(output = identity(&output), "frame failed: {e}");
            }
            s.failing = true;
        }
    }
    if !s.queued && !s.timed {
        s.timed = true;
        let timer =
            data.state
                .loop_handle
                .insert_source(Timer::from_duration(IDLE), move |_, _, data| {
                    if let Some(s) = data
                        .udev
                        .as_mut()
                        .and_then(|u| u.screens.iter_mut().find(|s| s.crtc == crtc))
                    {
                        s.timed = false;
                    }
                    render(data, crtc);
                    TimeoutAction::Drop
                });
        if let Err(e) = timer {
            warn!("frame timer: {e}");
        }
    }
    render::send_frames(&mut data.state, &output);
    data.state.space.refresh();
    data.state.popups.cleanup();
    data.state.refresh_toplevels();
    let _ = data.display.flush_clients();
}

fn configure_input(
    device: &mut smithay::reexports::input::Device,
    p: &crate::settings::Preferences,
) {
    let desktop =
        crate::desktop_config::load().unwrap_or_else(|_| crate::desktop_config::defaults());
    let _ = device.config_left_handed_set(desktop["left_handed"] == true);
    let _ = device.config_dwt_set_enabled(desktop["disable_while_typing"] == true);
    let _ = device.config_accel_set_profile(if desktop["flat_acceleration"] == true {
        smithay::reexports::input::AccelProfile::Flat
    } else {
        smithay::reexports::input::AccelProfile::Adaptive
    });
    let _ = device.config_accel_set_speed(p.pointer_speed);
    let _ = device.config_scroll_set_natural_scroll_enabled(p.natural_scroll);
    if device.config_tap_finger_count() > 0 {
        let _ = device.config_tap_set_enabled(p.tap_to_click);
    }
}
pub fn apply_input_preferences(data: &mut CalloopData) {
    let scale = data
        .state
        .space
        .outputs()
        .map(|o| o.current_scale().fractional_scale())
        .fold(1.0, f64::max)
        .ceil();
    let pointer = Cursors::from_env(scale as i32);
    if let Some(udev) = data.udev.as_mut() {
        udev.pointer = pointer;
        for device in &mut udev.input_devices {
            configure_input(device, &data.state.preferences);
        }
    }
}

/// Apply only a connector-advertised KMS mode. The settings transaction owns rollback.
pub fn set_mode(data: &mut CalloopData, output: &Output, mode: Mode) -> Result<(), String> {
    let Some(udev) = data.udev.as_mut() else {
        return Err("The host controls nested resolution".into());
    };
    let screen = udev
        .screens
        .iter_mut()
        .find(|s| s.output == *output)
        .ok_or("Display disconnected")?;
    let info = udev
        .manager
        .device()
        .get_connector(screen.connector, false)
        .map_err(|e| e.to_string())?;
    let native = info
        .modes()
        .iter()
        .find(|m| Mode::from(**m) == mode)
        .ok_or("Mode is no longer supported")?;
    let mut renderer = udev
        .gpus
        .single_renderer(&udev.render_node)
        .map_err(|e| e.to_string())?;
    screen
        .drm
        .use_mode::<_, Frame<UdevRenderer<'_>>>(
            *native,
            &mut renderer,
            &DrmOutputRenderElements::default(),
        )
        .map_err(|e| e.to_string())?;
    screen.drm.reset_buffers();
    screen.queued = false;
    screen.queued_lock = None;
    Ok(())
}

pub fn input_capabilities(data: &CalloopData) -> serde_json::Value {
    let devices = data
        .udev
        .as_ref()
        .map(|u| u.input_devices.as_slice())
        .unwrap_or_default();
    serde_json::json!({"pointer_speed": devices.iter().any(|d| d.config_accel_is_available()),
        "natural_scroll": devices.iter().any(|d| d.config_scroll_has_natural_scroll()),
        "tap_to_click": devices.iter().any(|d| d.config_tap_finger_count()>0)})
}

pub fn redraw(data: &mut CalloopData) {
    render_all(data);
}

pub fn outputs(data: &CalloopData) -> Vec<Output> {
    data.udev
        .as_ref()
        .map(|u| u.screens.iter().map(|s| s.output.clone()).collect())
        .unwrap_or_else(|| data.state.space.outputs().cloned().collect())
}
pub fn output_power(data: &mut CalloopData, output: &Output, enabled: bool) -> Result<(), String> {
    let Some(screen) = data
        .udev
        .as_mut()
        .and_then(|u| u.screens.iter_mut().find(|s| &s.output == output))
    else {
        return Ok(());
    };
    if !enabled {
        screen.off = true;
        screen
            .drm
            .with_compositor(|c| c.surface().clear())
            .map_err(|_| "Display could not be powered off")?;
    }
    screen.queued = false;
    screen.queued_lock = None;
    screen.drm.reset_buffers();
    Ok(())
}
pub fn vrr_supported(data: &CalloopData, output: &Output) -> bool {
    data.udev
        .as_ref()
        .and_then(|u| u.screens.iter().find(|s| &s.output == output))
        .is_some_and(|s| {
            s.drm.with_compositor(|c| {
                c.vrr_supported(s.connector)
                    .is_ok_and(|v| !matches!(v, smithay::backend::drm::VrrSupport::NotSupported))
            })
        })
}
pub fn set_vrr(data: &mut CalloopData, output: &Output, enabled: bool) -> Result<(), String> {
    if let Some(s) = data
        .udev
        .as_mut()
        .and_then(|u| u.screens.iter_mut().find(|s| &s.output == output))
        && s.drm.with_compositor(|c| c.vrr_enabled()) != enabled
    {
        s.drm
            .with_compositor(|c| c.use_vrr(enabled))
            .map_err(|_| "VRR could not be applied")?;
        s.drm.reset_buffers();
    }
    Ok(())
}
