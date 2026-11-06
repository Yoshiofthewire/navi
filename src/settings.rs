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

#[derive(Clone)]
struct Placement {
    output: Output,
    x: i32,
    y: i32,
    scale: f64,
    priority: u32,
    mode: smithay::output::Mode,
}
fn placements(data: &CalloopData) -> Vec<Placement> {
    data.state
        .space
        .outputs()
        .filter_map(|o| {
            let g = data.state.space.output_geometry(o)?;
            Some(Placement {
                output: o.clone(),
                x: g.loc.x,
                y: g.loc.y,
                scale: o.current_scale().fractional_scale(),
                priority: data.state.priority_of(&identity(o)),
                mode: o.current_mode()?,
            })
        })
        .collect()
}
fn apply(data: &mut CalloopData, placements: &[Placement]) -> Result<(), String> {
    data.state.cancel_drag();
    for p in placements {
        if !data.state.space.outputs().any(|o| o == &p.output) {
            continue;
        }
        if p.output.current_mode() != Some(p.mode) {
            crate::udev::set_mode(data, &p.output, p.mode)?;
        }
        p.output.change_current_state(
            Some(p.mode),
            None,
            Some(Scale::Fractional(p.scale)),
            Some((p.x, p.y).into()),
        );
        data.state.space.map_output(&p.output, (p.x, p.y));
        data.state.arrange_layers(&p.output);
    }
    let mut order = placements.to_vec();
    order.sort_by_key(|p| p.priority);
    data.state.set_priorities(
        &order
            .iter()
            .map(|p| identity(&p.output))
            .collect::<Vec<_>>(),
    );
    data.state.place_shell();
    data.state.wallpaper_epoch += 1;
    data.state.request_redraw();
    crate::udev::redraw(data);
    Ok(())
}

fn snapshot(data: &CalloopData, nested: bool, pending: bool) -> Value {
    let outputs: Vec<_> = placements(data)
        .iter()
        .map(|p| {
            let mode = p.output.current_mode();
            json!({"id":identity(&p.output),"name":p.output.name(),"x":p.x,"y":p.y,
            "scale":p.scale,"priority":p.priority,
            "modes":p.output.modes().iter().map(|m| [m.size.w,m.size.h,m.refresh]).collect::<Vec<_>>(),
            "width":mode.map_or(1,|m|m.size.w),"height":mode.map_or(1,|m|m.size.h),
            "refresh":mode.map_or(0,|m|m.refresh)})
        })
        .collect();
    json!({"ok":true,"nested":nested,"pending":pending,"outputs":outputs,
        "preferences":data.state.preferences.value(),"input_capabilities":crate::udev::input_capabilities(data),"version":env!("CARGO_PKG_VERSION")})
}

fn preview(data: &CalloopData, value: &Value) -> Result<Vec<Placement>, String> {
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
        if data.udev.is_none() && mode != p.mode {
            return Err("The host controls nested resolution".into());
        }
        let (w, h) = monitors::logical((mode.size.w, mode.size.h), e.scale);
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
        });
    }
    for (i, a) in mons.iter().enumerate() {
        if mons[..i].iter().any(|b| {
            a.rect.x < b.rect.x + b.rect.w
                && b.rect.x < a.rect.x + a.rect.w
                && a.rect.y < b.rect.y + b.rect.h
                && b.rect.y < a.rect.y + a.rect.h
        }) {
            return Err("Displays overlap. Move them apart before applying.".into());
        }
    }
    if a.main
        .as_ref()
        .is_some_and(|id| !mons.iter().any(|m| &m.id == id))
    {
        return Err("Primary display is disconnected".into());
    }
    let main = monitors::main(&mons, a.main.as_deref());
    if a.main.as_ref().is_some_and(|id| &mons[main].id != id) {
        return Err("Choose an external primary display while one is connected".into());
    }
    for (priority, index) in monitors::priority(&mons, main).into_iter().enumerate() {
        next[index].priority = priority as u32;
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
                            .any(|p| !data.state.space.outputs().any(|o| o == &p.output)))
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
            if state
                .preferences
                .idle_due(state.last_activity.elapsed(), state.lock.active)
            {
                state.last_activity = Instant::now();
                state.request_lock(None);
            }
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
