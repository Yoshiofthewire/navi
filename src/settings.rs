//! Same-user, per-Wayland-instance settings. Only bounded data crosses this socket.
use std::{
    cell::RefCell,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::PathBuf,
    rc::Rc,
    sync::mpsc,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use smithay::{
    output::{Output, Scale},
    reexports::calloop::{
        channel,
        timer::{TimeoutAction, Timer},
    },
    utils::Transform,
};

use crate::{
    CalloopData, files,
    monitors::{self, Arrangement, Entry, Mon},
    persist,
    state::identity,
};

#[derive(Clone, Debug, PartialEq)]
pub struct Preferences {
    pub idle_minutes: u64,
    pub pointer_speed: f64,
    pub natural_scroll: bool,
    pub tap_to_click: bool,
    pub repeat_rate: i32,
    pub repeat_delay: i32,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            idle_minutes: 0,
            pointer_speed: 0.0,
            natural_scroll: false,
            tap_to_click: true,
            repeat_rate: 25,
            repeat_delay: 200,
        }
    }
}
impl Preferences {
    fn idle_due(&self, elapsed: Duration, locked: bool) -> bool {
        !locked && self.idle_minutes > 0 && elapsed.as_secs() >= self.idle_minutes * 60
    }
    pub fn value(&self) -> Value {
        json!({"idle_minutes":self.idle_minutes,"pointer_speed":self.pointer_speed,
            "natural_scroll":self.natural_scroll,"tap_to_click":self.tap_to_click,
            "repeat_rate":self.repeat_rate,"repeat_delay":self.repeat_delay})
    }
    pub fn parse(v: &Value) -> Result<Self, String> {
        let bad = || "Invalid input or locking preferences".to_string();
        let p = Self {
            idle_minutes: v["idle_minutes"]
                .as_u64()
                .filter(|v| *v <= 240)
                .ok_or_else(bad)?,
            pointer_speed: v["pointer_speed"]
                .as_f64()
                .filter(|v| (-1.0..=1.0).contains(v))
                .ok_or_else(bad)?,
            natural_scroll: v["natural_scroll"].as_bool().ok_or_else(bad)?,
            tap_to_click: v["tap_to_click"].as_bool().ok_or_else(bad)?,
            repeat_rate: v["repeat_rate"]
                .as_i64()
                .filter(|v| (0..=100).contains(v))
                .ok_or_else(bad)? as i32,
            repeat_delay: v["repeat_delay"]
                .as_i64()
                .filter(|v| (100..=2000).contains(v))
                .ok_or_else(bad)? as i32,
        };
        Ok(p)
    }
    pub fn load() -> Self {
        let parsed = || {
            let text = persist::read(&config("preferences")?).ok()??;
            Self::parse(&serde_json::from_str::<Value>(&text).ok()?).ok()
        };
        parsed().unwrap_or_default()
    }
}

fn config(name: &str) -> Option<PathBuf> {
    Some(
        files::xdg_dir(
            std::env::var_os("XDG_CONFIG_HOME").as_deref(),
            std::env::var_os("HOME").as_deref(),
            ".config",
        )?
        .join("navi")
        .join(name),
    )
}

#[derive(Clone, Debug)]
pub struct OutputPreferences {
    pub enabled: bool,
    pub rotation: u16,
    pub mirror: Option<String>,
    pub vrr: bool,
}
impl Default for OutputPreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            rotation: 0,
            mirror: None,
            vrr: false,
        }
    }
}
pub fn output_preferences(output: &Output) -> OutputPreferences {
    output
        .user_data()
        .get_or_insert_threadsafe(|| std::sync::Mutex::new(OutputPreferences::default()))
        .lock()
        .unwrap()
        .clone()
}
fn transform(rotation: u16) -> Transform {
    match rotation {
        90 => Transform::_90,
        180 => Transform::_180,
        270 => Transform::_270,
        _ => Transform::Normal,
    }
}

#[derive(Clone)]
struct Placement {
    output: Output,
    x: i32,
    y: i32,
    scale: f64,
    priority: u32,
    mode: smithay::output::Mode,
    preferences: OutputPreferences,
}
fn placements(data: &CalloopData) -> Vec<Placement> {
    let outputs = crate::udev::outputs(data);
    outputs
        .into_iter()
        .filter_map(|o| {
            let loc = o.current_location();
            Some(Placement {
                x: loc.x,
                y: loc.y,
                scale: o.current_scale().fractional_scale(),
                priority: data.state.priority_of(&identity(&o)),
                mode: o.current_mode()?,
                preferences: output_preferences(&o),
                output: o,
            })
        })
        .collect()
}
fn apply(data: &mut CalloopData, requested: &[Placement]) -> Result<(), String> {
    let current = placements(data);
    let topology_changed = current.len() != requested.len()
        || current
            .iter()
            .any(|p| !requested.iter().any(|q| q.output == p.output));
    let mut placements = if topology_changed {
        current
    } else {
        requested.to_vec()
    };
    if topology_changed
        || !placements
            .iter()
            .any(|p| p.preferences.enabled && p.preferences.mirror.is_none())
    {
        safe_row(&mut placements);
    }
    data.state.cancel_drag();
    for p in &mut placements {
        if data.udev.is_none() {
            p.mode = p.output.current_mode().unwrap_or(p.mode);
        }
    }
    for p in &placements {
        if !crate::udev::outputs(data).iter().any(|o| o == &p.output) {
            continue;
        }
        if p.output.current_mode() != Some(p.mode) {
            crate::udev::set_mode(data, &p.output, p.mode)?;
        }
        if data.udev.is_some() {
            crate::udev::set_vrr(data, &p.output, p.preferences.vrr)?;
        }
        let orientation = if data.udev.is_some() {
            Some(transform(p.preferences.rotation))
        } else {
            None
        };
        p.output.change_current_state(
            Some(p.mode),
            orientation,
            Some(Scale::Fractional(p.scale)),
            Some((p.x, p.y).into()),
        );
        *p.output
            .user_data()
            .get::<std::sync::Mutex<OutputPreferences>>()
            .unwrap()
            .lock()
            .unwrap() = p.preferences.clone();
        if p.preferences.enabled {
            data.state.space.map_output(&p.output, (p.x, p.y));
            let id = identity(&p.output);
            if p.preferences.mirror.is_none() && !data.state.grid.iter().any(|g| g.name == id) {
                data.state.add_output(&p.output, p.priority);
            }
            if p.preferences.mirror.is_some() {
                data.state.grid.retain(|g| g.name != id);
                data.state.layout.remove_output(&id);
            }
            data.state.arrange_layers(&p.output);
        }
    }
    for p in placements.iter().filter(|p| !p.preferences.enabled) {
        if data.state.space.outputs().any(|o| o == &p.output) {
            data.state.remove_output(&p.output);
        }
        crate::udev::output_power(data, &p.output, false)?;
    }
    let mut order: Vec<_> = placements
        .iter()
        .filter(|p| p.preferences.enabled && p.preferences.mirror.is_none())
        .cloned()
        .collect();
    order.sort_by_key(|p| p.priority);
    data.state.set_priorities(
        &order
            .iter()
            .map(|p| identity(&p.output))
            .collect::<Vec<_>>(),
    );
    data.state.place_shell();
    data.state.lock_outputs_changed();
    data.state.wallpaper_epoch += 1;
    data.state.request_redraw();
    crate::udev::redraw(data);
    Ok(())
}

fn safe_row(placements: &mut [Placement]) {
    placements.sort_by_key(|p| p.output.name());
    let mut x = 0;
    for (priority, p) in placements.iter_mut().enumerate() {
        p.preferences = OutputPreferences::default();
        p.x = x;
        p.y = 0;
        p.priority = priority as u32;
        x += monitors::logical((p.mode.size.w, p.mode.size.h), p.scale).0;
    }
}

fn snapshot(data: &CalloopData, nested: bool, pending: bool) -> Value {
    let outputs: Vec<_> = placements(data)
        .iter()
        .map(|p| {
            let mode = p.output.current_mode();
            json!({"id":identity(&p.output),"name":p.output.name(),"x":p.x,"y":p.y,
            "scale":p.scale,"priority":p.priority,"enabled":p.preferences.enabled,
            "rotation":p.preferences.rotation,"mirror":p.preferences.mirror,"vrr":p.preferences.vrr,
            "vrr_supported":crate::udev::vrr_supported(data,&p.output),
            "modes":p.output.modes().iter().map(|m| [m.size.w,m.size.h,m.refresh]).collect::<Vec<_>>(),
            "width":mode.map_or(1,|m|m.size.w),"height":mode.map_or(1,|m|m.size.h),
            "refresh":mode.map_or(0,|m|m.refresh)})
        })
        .collect();
    json!({"ok":true,"nested":nested,"pending":pending,"outputs":outputs,
        "desktop":data.state.desktop,
        "preferences":data.state.preferences.value(),"input_capabilities":crate::udev::input_capabilities(data),"version":env!("CARGO_PKG_VERSION")})
}

fn preview(data: &CalloopData, value: &Value) -> Result<Vec<Placement>, String> {
    preview_for_backend(data, value, data.udev.is_some())
}
fn preview_for_backend(
    data: &CalloopData,
    value: &Value,
    physical: bool,
) -> Result<Vec<Placement>, String> {
    let text = value["arrangement"]
        .as_str()
        .ok_or("Missing display arrangement")?;
    let a = Arrangement::parse(text)?;
    let old = placements(data);
    if a.entries.len() != old.len()
        || old
            .iter()
            .any(|p| !a.entries.iter().any(|e| e.id == identity(&p.output)))
    {
        return Err("Displays changed; refresh and try again".into());
    }
    let mut mons = Vec::new();
    let mut next = Vec::new();
    for p in old {
        let e = a
            .entries
            .iter()
            .find(|e| e.id == identity(&p.output))
            .unwrap();
        let mode = if let Some(&(w, h, r)) = a.modes.get(&e.id) {
            p.output
                .modes()
                .into_iter()
                .find(|m| (m.size.w, m.size.h, m.refresh) == (w, h, r))
                .ok_or("Choose a supported display mode")?
        } else {
            p.mode
        };
        if !physical && mode != p.mode {
            return Err("The host controls nested resolution".into());
        }
        let rotation = a
            .rotations
            .get(&e.id)
            .copied()
            .unwrap_or(p.preferences.rotation);
        if !physical
            && (rotation != 0
                || a.disabled.contains(&e.id)
                || a.mirrors.contains_key(&e.id)
                || a.vrr.contains(&e.id))
        {
            return Err(
                "Rotation, enable/disable, mirroring and VRR require a full Navi session".into(),
            );
        }
        let preferences = OutputPreferences {
            enabled: !a.disabled.contains(&e.id),
            rotation,
            mirror: a.mirrors.get(&e.id).cloned(),
            vrr: a.vrr.contains(&e.id),
        };
        if preferences.vrr && !crate::udev::vrr_supported(data, &p.output) {
            return Err("VRR is unavailable on this output".into());
        }
        let size = if rotation == 90 || rotation == 270 {
            (mode.size.h, mode.size.w)
        } else {
            (mode.size.w, mode.size.h)
        };
        let (w, h) = monitors::logical(size, e.scale);
        mons.push(Mon {
            connector: p.output.name(),
            id: e.id.clone(),
            size: (mode.size.w, mode.size.h),
            rect: crate::layout::Rect {
                x: e.x,
                y: e.y,
                w,
                h,
            },
        });
        next.push(Placement {
            output: p.output,
            x: e.x,
            y: e.y,
            scale: e.scale,
            priority: 0,
            mode,
            preferences,
        });
    }
    let active: Vec<_> = next
        .iter()
        .enumerate()
        .filter(|(_, p)| p.preferences.enabled && p.preferences.mirror.is_none())
        .map(|(i, _)| i)
        .collect();
    if active.is_empty() {
        return Err("Keep at least one independent display enabled".into());
    }
    for &i in &active {
        if active.iter().copied().take_while(|j| *j != i).any(|j| {
            let (a, b) = (&mons[i].rect, &mons[j].rect);
            a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
        }) {
            return Err("Displays overlap. Move them apart before applying.".into());
        }
    }
    for i in 0..next.len() {
        if let Some(source) = &next[i].preferences.mirror {
            let j = active
                .iter()
                .copied()
                .find(|&j| identity(&next[j].output) == *source)
                .ok_or("Mirror source must be an enabled independent display")?;
            if next[i].mode.size != next[j].mode.size
                || next[i].scale != next[j].scale
                || next[i].preferences.rotation != next[j].preferences.rotation
            {
                return Err("Mirroring requires matching resolution, rotation and scale".into());
            }
            next[i].x = next[j].x;
            next[i].y = next[j].y;
        }
    }
    let active_mons: Vec<_> = active.iter().map(|&i| mons[i].clone()).collect();
    if a.main
        .as_ref()
        .is_some_and(|id| !active_mons.iter().any(|m| &m.id == id))
    {
        return Err("Primary display must be enabled and independent".into());
    }
    let main = monitors::main(&active_mons, a.main.as_deref());
    if a.main
        .as_ref()
        .is_some_and(|id| &active_mons[main].id != id)
    {
        return Err("Choose an external primary display while one is connected".into());
    }
    for p in &mut next {
        p.priority = u32::MAX;
    }
    for (priority, index) in monitors::priority(&active_mons, main)
        .into_iter()
        .enumerate()
    {
        next[active[index]].priority = priority as u32;
    }
    Ok(next)
}

struct Pending {
    token: u64,
    original: Vec<Placement>,
    deadline: Instant,
}
#[derive(Default)]
struct Session {
    pending: Option<Pending>,
    serial: u64,
}
impl Session {
    fn revert(&mut self, data: &mut CalloopData) {
        if let Some(p) = self.pending.take()
            && let Err(e) = apply(data, &p.original)
        {
            tracing::warn!("display restore failed: {e}");
        }
    }
    fn request(
        &mut self,
        data: &mut CalloopData,
        value: &Value,
        nested: bool,
    ) -> Result<Value, String> {
        let op = value["op"].as_str().ok_or("Missing operation")?;
        if data.state.lock.active {
            return Err("Unlock Navi to change settings".into());
        }
        match op {
            "get" => Ok(snapshot(data, nested, self.pending.is_some())),
            "preview" => {
                if self.pending.is_some() {
                    return Err("Keep or revert the current display preview first".into());
                }
                let next = preview(data, value)?;
                let original = placements(data);
                self.serial += 1;
                self.pending = Some(Pending {
                    token: self.serial,
                    original,
                    deadline: Instant::now() + Duration::from_secs(20),
                });
                if let Err(e) = apply(data, &next) {
                    self.revert(data);
                    return Err(e);
                }
                Ok(json!({"ok":true,"token":self.serial,"seconds":20}))
            }
            "keep" | "revert" => {
                let p = self
                    .pending
                    .as_ref()
                    .ok_or("Display preview expired; previous settings restored")?;
                if value["token"].as_u64() != Some(p.token) {
                    return Err("Display preview belongs to another request".into());
                }
                if op == "keep"
                    && (placements(data).len() != p.original.len()
                        || p.original
                            .iter()
                            .any(|p| !crate::udev::outputs(data).iter().any(|o| o == &p.output)))
                {
                    self.revert(data);
                    return Err("Displays changed during preview; refresh and try again".into());
                }
                if op == "revert" {
                    self.revert(data);
                } else {
                    if Instant::now() >= p.deadline {
                        self.revert(data);
                        return Err("Preview expired".into());
                    }
                    if !nested {
                        let path = config("monitors").ok_or("No configuration directory")?;
                        let mut a = match persist::read(&path)? {
                            Some(text) => Arrangement::parse(&text)?,
                            None => Arrangement::default(),
                        };
                        for p in placements(data) {
                            let id = identity(&p.output);
                            if p.priority == 0 {
                                a.main = Some(id.clone());
                            }
                            a.entries.retain(|e| e.id != id);
                            a.entries.push(Entry {
                                id: id.clone(),
                                x: p.x,
                                y: p.y,
                                scale: p.scale,
                            });
                            a.rotations.insert(id.clone(), p.preferences.rotation);
                            if p.preferences.enabled {
                                a.disabled.remove(&id);
                            } else {
                                a.disabled.insert(id.clone());
                            }
                            if let Some(source) = p.preferences.mirror {
                                a.mirrors.insert(id.clone(), source);
                            } else {
                                a.mirrors.remove(&id);
                            }
                            if p.preferences.vrr {
                                a.vrr.insert(id.clone());
                            } else {
                                a.vrr.remove(&id);
                            }
                            a.modes
                                .insert(id, (p.mode.size.w, p.mode.size.h, p.mode.refresh));
                        }
                        persist::write(&path, &a.text()).map_err(|e| e.to_string())?;
                    }
                    self.pending = None;
                }
                Ok(snapshot(data, nested, false))
            }
            "preferences" => {
                let p = Preferences::parse(&value["preferences"])?;
                if p.idle_minutes > 0 && !crate::locking::executable().is_file() {
                    return Err("Install swaylock before enabling automatic locking".into());
                }
                let path = config("preferences").ok_or("No configuration directory")?;
                if let Some(text) = persist::read(&path)? {
                    Preferences::parse(
                        &serde_json::from_str::<Value>(&text).map_err(|e| e.to_string())?,
                    )?;
                }
                persist::write(&path, &p.value().to_string()).map_err(|e| e.to_string())?;
                data.state.preferences = p;
                if let Some(kb) = data.state.seat.get_keyboard() {
                    kb.change_repeat_info(
                        data.state.preferences.repeat_rate,
                        data.state.preferences.repeat_delay,
                    );
                }
                crate::udev::apply_input_preferences(data);
                Ok(snapshot(data, nested, self.pending.is_some()))
            }
            "wallpaper" => {
                if self.pending.is_some() {
                    return Err("Finish the display preview first".into());
                }
                let index = value["priority"]
                    .as_u64()
                    .filter(|v| *v <= 255)
                    .ok_or("Invalid display")?;
                if !placements(data)
                    .iter()
                    .any(|p| u64::from(p.priority) == index)
                {
                    return Err("Display disconnected".into());
                }
                let path = value["path"]
                    .as_str()
                    .ok_or("Choose a PNG or JPEG wallpaper")?;
                let file = std::path::Path::new(path);
                if !file.is_absolute() || path.chars().any(char::is_control) {
                    return Err("Choose a valid absolute image path".into());
                }
                let ext = file
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if !["png", "jpg", "jpeg"].contains(&ext.as_str()) {
                    return Err("Choose a PNG or JPEG image".into());
                }
                let target = config("wallpaper").ok_or("No configuration directory")?;
                let mut entries = match persist::read(&target)? {
                    Some(text) => {
                        let (entries, bad) =
                            crate::wallpaper::parse(&text).map_err(str::to_string)?;
                        if bad > 0 {
                            return Err("Repair the existing wallpaper configuration first".into());
                        }
                        entries
                    }
                    None => Vec::new(),
                };
                entries.retain(|(i, _)| u64::from(*i) != index);
                entries.push((index as u8, file.to_path_buf()));
                persist::write(&target, &crate::wallpaper::serialize(&entries))
                    .map_err(|e| e.to_string())?;
                data.state.wallpaper_epoch += 1;
                data.state.request_redraw();
                Ok(json!({"ok":true}))
            }
            "desktop" => {
                let desktop = crate::desktop_config::parse(&value["settings"])?;
                data.state.release_sticky();
                crate::desktop_config::load()?; // Preserve a malformed existing configuration.
                if let Some(kb) = data.state.seat.get_keyboard() {
                    kb.set_xkb_config(&mut data.state, xkb_config(&desktop))
                        .map_err(|_| "Keyboard layout could not be loaded")?;
                }
                let path = crate::desktop_config::path().ok_or("No configuration directory")?;
                if let Err(e) = persist::write(&path, &desktop.to_string()) {
                    if let Some(kb) = data.state.seat.get_keyboard() {
                        let previous = data.state.desktop.clone();
                        let _ = kb.set_xkb_config(&mut data.state, xkb_config(&previous));
                    }
                    return Err(e.to_string());
                }
                crate::theme::configure(&desktop);
                data.state.desktop = desktop;
                crate::udev::apply_input_preferences(data);
                data.state.request_redraw();
                Ok(snapshot(data, nested, self.pending.is_some()))
            }
            "lock" => {
                data.state.request_lock(None);
                Ok(json!({"ok":true}))
            }
            _ => Err("Unknown settings operation".into()),
        }
    }
}

pub struct Socket(PathBuf);
impl Drop for Socket {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
pub fn start(
    data: &mut CalloopData,
    display: &std::ffi::OsStr,
    nested: bool,
) -> Result<Socket, Box<dyn std::error::Error>> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").ok_or("No runtime directory")?);
    let path = dir.join(format!("navi-{}.settings", display.to_string_lossy()));
    use std::os::unix::fs::FileTypeExt;
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_socket())
        && std::os::unix::net::UnixStream::connect(&path)
            .is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused)
    {
        fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let session = Rc::new(RefCell::new(Session::default()));
    let (tx, rx) = channel::channel::<(Value, mpsc::Sender<Value>)>();
    let state = session.clone();
    data.state
        .loop_handle
        .insert_source(rx, move |event, _, data| {
            if let channel::Event::Msg((v, reply)) = event {
                let result = state
                    .borrow_mut()
                    .request(data, &v, nested)
                    .unwrap_or_else(|error| json!({"ok":false,"error":error}));
                let _ = reply.send(result);
            }
        })?;
    data.state.loop_handle.insert_source(
        Timer::from_duration(Duration::from_secs(1)),
        move |_, _, data| {
            let mut session = session.borrow_mut();
            if session
                .pending
                .as_ref()
                .is_some_and(|p| Instant::now() >= p.deadline)
            {
                session.revert(data);
            }
            let state = &mut data.state;
            let elapsed = state.last_activity.elapsed().as_secs();
            let off = state.desktop["screen_off_minutes"].as_u64().unwrap_or(0);
            if !nested && off > 0 && elapsed >= off * 60 && !state.screen_off {
                // Lock first; the display may wake through an external event.
                state.request_lock(None);
                state.screen_off = state.lock_outputs_covered();
            }
            let sleep = state.desktop["sleep_minutes"].as_u64().unwrap_or(0);
            if !nested && sleep > 0 && elapsed >= sleep * 60 && !state.sleep_requested {
                request_sleep(state);
            }
            if state
                .preferences
                .idle_due(state.last_activity.elapsed(), state.lock.active)
            {
                state.request_lock(None);
            }
            crate::udev::redraw(data);
            TimeoutAction::ToDuration(Duration::from_secs(1))
        },
    )?;
    std::thread::spawn(move || {
        // ponytail: one worker serializes requests, with two-second I/O deadlines.
        // No per-client threads or blocking work in the compositor's event loop.
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                break;
            };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let mut line = Vec::new();
            use std::io::Read;
            if BufReader::new((&stream).take(16385))
                .read_until(b'\n', &mut line)
                .is_err()
                || line.len() > 16384
            {
                continue;
            }
            let Ok(v) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            if v["op"] == "wallpaper" {
                let valid = || -> Result<(), String> {
                    let path = v["path"].as_str().ok_or("No wallpaper selected")?;
                    let bytes = files::read_bytes(std::path::Path::new(path), 32 << 20)?
                        .ok_or("Wallpaper file is missing")?;
                    crate::image::decode(&bytes, crate::image::MAX_SIDE)?;
                    Ok(())
                };
                if let Err(error) = valid() {
                    let _ = writeln!(stream, "{}", json!({"ok":false,"error":error}));
                    continue;
                }
            }
            let (reply, result) = mpsc::channel();
            if tx.send((v, reply)).is_err() {
                break;
            }
            let result = result
                .recv_timeout(Duration::from_secs(3))
                .unwrap_or_else(|_| json!({"ok":false,"error":"Navi did not respond"}));
            let reply = result.to_string();
            if reply.len() > 65536 {
                let _ = writeln!(
                    stream,
                    "{}",
                    json!({"ok":false,"error":"Display inventory exceeds settings response limit"})
                );
            } else {
                let _ = writeln!(stream, "{reply}");
            }
        }
    });
    Ok(Socket(path))
}

pub fn saved_arrangement() -> Option<Arrangement> {
    Arrangement::parse(&persist::read(&config("monitors")?).ok()??).ok()
}

pub fn xkb_config(v: &Value) -> smithay::input::keyboard::XkbConfig<'_> {
    smithay::input::keyboard::XkbConfig {
        layout: v["keyboard_layout"].as_str().unwrap_or("us"),
        variant: v["keyboard_variant"].as_str().unwrap_or(""),
        options: Some(v["keyboard_options"].as_str().unwrap_or("").to_string()),
        ..Default::default()
    }
}

/// Reapply saved advanced output preferences after connector discovery, retaining safe defaults on error.
pub fn restore_outputs(data: &mut CalloopData) {
    let Some(saved) = saved_arrangement() else {
        return;
    };
    let mut connected = saved.clone();
    let old = placements(data);
    connected
        .entries
        .retain(|e| old.iter().any(|p| identity(&p.output) == e.id));
    for p in &old {
        let id = identity(&p.output);
        if !connected.entries.iter().any(|e| e.id == id) {
            connected.entries.push(Entry {
                id,
                x: p.x,
                y: p.y,
                scale: p.scale,
            });
        }
    }
    if connected
        .main
        .as_ref()
        .is_some_and(|id| !connected.entries.iter().any(|e| &e.id == id))
    {
        connected.main = None;
    }
    let next = match preview(data, &json!({"arrangement":connected.text()})) {
        Ok(next) => next,
        Err(_) => {
            let mut safe = placements(data);
            safe_row(&mut safe);
            safe
        }
    };
    if let Err(e) = apply(data, &next) {
        tracing::warn!("saved output preferences could not be restored: {e}");
    }
}

pub fn request_sleep(state: &mut crate::state::Navi) {
    if state.session.is_none() || state.sleep_requested {
        return;
    }
    state.sleep_requested = true;
    let (ready, result) = std::sync::mpsc::channel();
    state.request_lock(Some(ready));
    std::thread::spawn(move || {
        if !result
            .recv_timeout(Duration::from_secs(12))
            .unwrap_or(false)
        {
            tracing::warn!("automatic sleep refused: lock not confirmed");
            return;
        }
        let sleep = || -> zbus::Result<()> {
            let c = zbus::blocking::connection::Builder::system()?
                .method_timeout(Duration::from_secs(5))
                .build()?;
            let p = zbus::blocking::Proxy::new(
                &c,
                "org.freedesktop.login1",
                "/org/freedesktop/login1",
                "org.freedesktop.login1.Manager",
            )?;
            if p.call::<_, _, String>("CanSuspend", &())? != "yes" {
                return Err(zbus::Error::Failure("Sleep unavailable".into()));
            }
            p.call("Suspend", &(false,))
        };
        if sleep().is_err() {
            tracing::warn!("automatic sleep unavailable; session remains locked");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_validate_before_saving() {
        let defaults = Preferences::default();
        assert!(!defaults.idle_due(Duration::from_secs(99999), false));
        let enabled = Preferences {
            idle_minutes: 1,
            ..Default::default()
        };
        assert!(!enabled.idle_due(Duration::from_secs(59), false));
        assert!(enabled.idle_due(Duration::from_secs(60), false));
        assert!(!enabled.idle_due(Duration::from_secs(60), true));
        assert_eq!(Preferences::parse(&defaults.value()).unwrap(), defaults);
        for (key, bad) in [
            ("idle_minutes", json!(241)),
            ("pointer_speed", json!(2)),
            ("natural_scroll", json!("yes")),
            ("repeat_rate", json!(-1)),
            ("repeat_delay", json!(0)),
        ] {
            let mut v = defaults.value();
            v[key] = bad;
            assert!(Preferences::parse(&v).is_err());
        }
    }

    #[test]
    fn advanced_display_plans_reject_blackouts_and_invalid_mirrors() {
        let mut h = crate::locking::tests::Harness::new();
        h.output("left", 0);
        h.output("right", 800);
        h.data
            .state
            .set_priorities(&["left".into(), "right".into()]);
        let plan = |lines: &str| json!({"arrangement":format!("navi-monitors 1\nleft 0 0 1\nright 800 0 1\n{lines}")});
        assert!(
            preview_for_backend(&h.data, &plan("disabled left\ndisabled right\n"), true).is_err()
        );
        assert!(preview_for_backend(&h.data, &plan("disabled left\nmain left\n"), true).is_err());
        assert!(
            preview_for_backend(
                &h.data,
                &plan("mirror left right\nmirror right left\n"),
                true
            )
            .is_err()
        );
        assert!(
            preview_for_backend(
                &h.data,
                &plan("rotation right 90\nmirror right left\n"),
                true
            )
            .is_err()
        );
        let mirror =
            preview_for_backend(&h.data, &plan("mirror right left\nmain left\n"), true).unwrap();
        assert_eq!(mirror[1].x, mirror[0].x);
        assert!(preview(&h.data, &plan("rotation left 90\n")).is_err());
        let before = snapshot(&h.data, true, false);
        let original = placements(&h.data);
        let right = h
            .data
            .state
            .space
            .outputs()
            .find(|o| o.name() == "right")
            .unwrap()
            .clone();
        h.data.state.remove_output(&right);
        apply(&mut h.data, &original).unwrap();
        assert!(h.data.state.space.outputs().next().is_some());
        assert!(h.data.state.grid.iter().any(|o| o.name == "left"));
        assert_ne!(snapshot(&h.data, true, false), before);
    }

    #[test]
    fn display_preview_rejects_overlap_restores_and_guards_lock() {
        let mut h = crate::locking::tests::Harness::new();
        h.output("left", 0);
        h.output("right", 800);
        h.data
            .state
            .set_priorities(&["left".into(), "right".into()]);
        let before = snapshot(&h.data, true, false);
        let mut session = Session::default();
        let bad =
            json!({"op":"preview","arrangement":"navi-monitors 1\nleft 0 0 1\nright 799 0 1\n"});
        assert!(session.request(&mut h.data, &bad, true).is_err());
        assert_eq!(snapshot(&h.data, true, false), before);
        let good = json!({"op":"preview","arrangement":"navi-monitors 1\nleft 0 0 2\nright 400 0 1\nmain right\n"});
        let reply = session.request(&mut h.data, &good, true).unwrap();
        assert!(session.request(&mut h.data, &good, true).is_err());
        assert!(
            session
                .request(&mut h.data, &json!({"op":"keep","token":99}), true)
                .is_err()
        );
        session
            .request(
                &mut h.data,
                &json!({"op":"revert","token":reply["token"]}),
                true,
            )
            .unwrap();
        assert_eq!(snapshot(&h.data, true, false), before);
        session.request(&mut h.data, &good, true).unwrap();
        session.pending.as_mut().unwrap().deadline = Instant::now();
        let token = session.pending.as_ref().unwrap().token;
        assert!(
            session
                .request(&mut h.data, &json!({"op":"keep","token":token}), true)
                .is_err()
        );
        assert_eq!(snapshot(&h.data, true, false), before);
        h.data.state.lock.active = true;
        for op in [
            "get",
            "preview",
            "keep",
            "revert",
            "preferences",
            "desktop",
            "wallpaper",
            "lock",
        ] {
            assert!(
                session
                    .request(&mut h.data, &json!({"op":op}), true)
                    .is_err()
            );
        }
    }
}
