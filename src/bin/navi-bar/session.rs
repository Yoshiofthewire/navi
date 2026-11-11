//! Session menu and native system-bus actions. No shell commands or host actions when nested.

use std::time::Duration;

use zbus::blocking::{Connection, Proxy, connection::Builder};

use crate::{
    layout::{IconOf, Item, Rect},
    status::Battery,
    theme::{ACCENT, BAR_BG, DIM, INK, INSET, PANEL_EDGE, SELECTED},
};

pub const BAND_H: i32 = 32;
/// The band's layer margins, top, right, bottom, left: it floats `INSET` from the screen edges.
pub const BAND_MARGIN: [i32; 4] = [INSET, INSET, 0, INSET];
pub const BUTTON: Rect = Rect {
    x: 4,
    y: 0,
    w: 100,
    h: BAND_H,
};
const WIDTH: i32 = 300;
const ROW_H: i32 = 40;
const TOP: i32 = BAND_H + INSET;
/// Inside the panel, as in the launcher.
const PAD: i32 = 12;
const HEADING_H: i32 = 32;
const LINE_H: i32 = 24;
const LIST_Y: i32 = TOP + PAD + HEADING_H + 4;
const LOGIN: &str = "org.freedesktop.login1";
const LOGIN_PATH: &str = "/org/freedesktop/login1";
const MANAGER: &str = "org.freedesktop.login1.Manager";
const DM: &str = "org.freedesktop.DisplayManager";
const SEAT: &str = "org.freedesktop.DisplayManager.Seat";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Sleep,
    Lock,
    SwitchUser,
    Logout,
    Restart,
    Shutdown,
}

pub const ACTIONS: [Action; 6] = [
    Action::Sleep,
    Action::Lock,
    Action::SwitchUser,
    Action::Logout,
    Action::Restart,
    Action::Shutdown,
];

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sleep => "Sleep",
            Self::Lock => "Lock Screen",
            Self::SwitchUser => "Switch User",
            Self::Logout => "Log Out",
            Self::Restart => "Restart",
            Self::Shutdown => "Shutdown",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Self::Sleep => "system-suspend",
            Self::Lock => "system-lock-screen",
            Self::SwitchUser => "system-switch-user",
            Self::Logout => "system-log-out",
            Self::Restart => "system-reboot",
            Self::Shutdown => "system-shutdown",
        }
    }
    fn method(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Sleep => Some(("CanSuspend", "Suspend")),
            Self::Restart => Some(("CanReboot", "Reboot")),
            Self::Shutdown => Some(("CanPowerOff", "PowerOff")),
            _ => None,
        }
    }
    fn warning(self, full: bool) -> &'static str {
        match self {
            Self::Sleep => "Lock the screen, then put Navi to sleep.",
            Self::SwitchUser => "Lock Navi, then open the login screen.",
            Self::Lock => "Enter your password to unlock Navi.",
            Self::Logout if !full => "Close this nested Navi window.",
            _ => "Open apps may lose unsaved work.",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Capabilities {
    pub available: [bool; 6],
    /// The display manager's seat for this exact login session, never an arbitrary seat.
    seat: Option<String>,
}
impl Default for Capabilities {
    fn default() -> Self {
        Self {
            available: [false, false, false, true, false, false],
            seat: None,
        }
    }
}

#[derive(Default)]
pub struct Menu {
    pub open: bool,
    pub selected: usize,
    pub confirm: Option<Action>,
    pub busy: bool,
    pub message: String,
    pub capabilities: Capabilities,
    /// A full session, not nested: decides which actions can be offered.
    pub full: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Row(usize),
    Cancel,
    Confirm,
}

impl Menu {
    pub fn close(&mut self) {
        self.open = false;
        self.confirm = None;
        self.selected = 0;
        self.message.clear();
    }
    /// Content plus `PAD` on every side; the hint below the list is one or two lines.
    pub fn panel(&self) -> Rect {
        let lines = self.message().lines().take(2).count() as i32;
        Rect {
            x: 0,
            y: TOP,
            w: WIDTH,
            h: if self.confirm.is_some() {
                PAD + HEADING_H + 2 * HEADING_H + 8 + 36 + PAD
            } else {
                LIST_Y - TOP + ACTIONS.len() as i32 * ROW_H + 4 + lines * LINE_H + PAD
            },
        }
    }
    /// The hint under the list.
    fn message(&self) -> &str {
        if !self.message.is_empty() {
            self.message.as_str()
        } else if self.busy {
            "Checking session services…"
        } else if self.selected == 1 && !self.capabilities.available[1] {
            "Install swaylock to enable\nscreen locking."
        } else if !self.full && self.selected != 3 && self.selected != 1 {
            "Available in a full Navi session."
        } else if !self.capabilities.available[self.selected] {
            "Not available from the\nsession services."
        } else {
            "Esc closes · Enter selects"
        }
    }
    pub fn height(&self) -> i32 {
        if self.open {
            let p = self.panel();
            p.y + p.h
        } else {
            BAND_H
        }
    }
    /// The band's input region: the Navi button, and the menu while it's open.
    pub fn input(&self) -> Vec<Rect> {
        let mut r = vec![BUTTON];
        r.extend(self.open.then(|| self.panel()));
        r
    }
    fn row(&self, i: usize) -> Rect {
        Rect {
            x: PAD,
            y: LIST_Y + i as i32 * ROW_H,
            w: WIDTH - 2 * PAD,
            h: ROW_H,
        }
    }
    fn cancel(&self) -> Rect {
        let p = self.panel();
        Rect {
            x: p.x + PAD,
            y: p.y + p.h - PAD - 36,
            w: 128,
            h: 36,
        }
    }
    fn accept(&self) -> Rect {
        Rect {
            x: self.cancel().x + 140,
            ..self.cancel()
        }
    }
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if !self.open {
            return None;
        }
        if self.confirm.is_some() {
            if self.cancel().contains(x, y) {
                return Some(Hit::Cancel);
            }
            return self.accept().contains(x, y).then_some(Hit::Confirm);
        }
        (0..ACTIONS.len())
            .find(|&i| self.row(i).contains(x, y))
            .map(Hit::Row)
    }
    pub fn step(&mut self, by: i32) {
        let n = if self.confirm.is_some() {
            2
        } else {
            ACTIONS.len()
        };
        self.selected = (self.selected as i32 + by).rem_euclid(n as i32) as usize;
    }
    pub fn selected_hit(&self) -> Hit {
        if self.confirm.is_some() {
            if self.selected == 0 {
                Hit::Cancel
            } else {
                Hit::Confirm
            }
        } else {
            Hit::Row(self.selected)
        }
    }
    /// The only path to an action is an enabled row followed by explicit confirmation.
    pub fn activate(&mut self, hit: Hit) -> Option<Action> {
        if !self.open || self.busy {
            return None;
        }
        match hit {
            Hit::Row(i) if self.confirm.is_none() && i < ACTIONS.len() => {
                if self.capabilities.available[i] {
                    self.confirm = Some(ACTIONS[i]);
                    self.selected = 0; // Enter defaults to Cancel, including after a key repeat.
                    self.message.clear();
                }
            }
            Hit::Cancel => {
                self.confirm = None;
                self.selected = 0;
            }
            Hit::Confirm => return self.confirm.take(),
            _ => {}
        }
        None
    }
    pub fn draw(&self) -> Vec<Item> {
        if !self.open {
            return Vec::new();
        }
        let p = self.panel();
        let mut out = vec![Item::Panel(p, PANEL_EDGE, 1)];
        let (x, w) = (p.x + PAD, WIDTH - 2 * PAD);
        let text = |label: &str, y, h, color| Item::Text(label.into(), Rect { x, y, w, h }, color);
        if let Some(action) = self.confirm {
            let y = p.y + PAD;
            out.push(text(&format!("{}?", action.label()), y, HEADING_H, INK));
            out.push(text(
                action.warning(self.full),
                y + HEADING_H,
                HEADING_H,
                INK,
            ));
            let hint = "Cancel returns to the session menu.";
            out.push(text(hint, y + 2 * HEADING_H, HEADING_H, DIM));
            for (i, (r, label)) in [(self.cancel(), "Cancel"), (self.accept(), action.label())]
                .into_iter()
                .enumerate()
            {
                out.push(Item::Round(
                    r,
                    8,
                    if self.selected == i { SELECTED } else { BAR_BG },
                ));
                let at = Rect {
                    x: r.x + 12,
                    w: r.w - 24,
                    ..r
                };
                out.push(Item::Text(label.into(), at, INK));
            }
        } else {
            out.push(text("Session", p.y + PAD, HEADING_H, INK));
            for (i, action) in ACTIONS.into_iter().enumerate() {
                let r = self.row(i);
                let enabled = self.capabilities.available[i] && !self.busy;
                if self.selected == i {
                    out.push(Item::Round(r, 8, SELECTED));
                }
                out.push(Item::Icon(
                    IconOf::Session(action.icon()),
                    Rect {
                        x: r.x + 8,
                        y: r.y + 8,
                        w: 24,
                        h: 24,
                    },
                    if enabled { 1.0 } else { 0.45 },
                ));
                let label = Rect {
                    x: r.x + 44,
                    w: r.w - 52,
                    ..r
                };
                let color = if enabled { INK } else { DIM };
                out.push(Item::Text(action.label().into(), label, color));
            }
            let list_end = self.row(ACTIONS.len() - 1).y + ROW_H + 4;
            for (i, line) in self.message().lines().take(2).enumerate() {
                out.push(text(line, list_end + i as i32 * LINE_H, LINE_H, DIM));
            }
        }
        out
    }
}

/// The status band, `w` wide: the Navi button (`lit` when hovered or open), the clock and
/// the battery, each text with its width in px.
pub fn band(
    w: i32,
    lit: bool,
    clock: (String, i32),
    battery: Option<(String, i32, Battery)>,
) -> Vec<Item> {
    let text = |t, x, w| {
        Item::Text(
            t,
            Rect {
                x,
                y: 0,
                w,
                h: BAND_H,
            },
            INK,
        )
    };
    let mut out = vec![Item::Panel(
        Rect {
            x: 0,
            y: 0,
            w,
            h: BAND_H,
        },
        PANEL_EDGE,
        1,
    )];
    if lit {
        out.push(Item::Round(
            Rect {
                y: 4,
                h: BAND_H - 8,
                ..BUTTON
            },
            8,
            SELECTED,
        ));
    }
    out.push(Item::Icon(
        IconOf::Button,
        Rect {
            x: 12,
            y: 8,
            w: 16,
            h: 16,
        },
        1.0,
    ));
    out.push(text("Navi".into(), 36, 64));
    let (clock, clock_w) = clock;
    out.push(text(clock, (w - clock_w) / 2, clock_w + 1));
    if let Some((label, label_w, b)) = battery {
        let x = w - 12 - label_w;
        out.push(text(label, x, label_w + 1));
        let r = Rect {
            x: x - 27,
            y: 11,
            w: 18,
            h: 10,
        };
        for edge in [
            Rect { h: 1, ..r },
            Rect {
                y: r.y + r.h - 1,
                h: 1,
                ..r
            },
            Rect { w: 1, ..r },
            Rect {
                x: r.x + r.w - 1,
                w: 1,
                ..r
            },
            Rect {
                x: r.x + r.w,
                y: r.y + 3,
                w: 2,
                h: 4,
            },
        ] {
            out.push(Item::Fill(edge, DIM));
        }
        let charge = Rect {
            x: r.x + 2,
            y: r.y + 2,
            w: (14 * b.percent as i32 / 100).max(1),
            h: 6,
        };
        out.push(Item::Fill(charge, if b.charging { ACCENT } else { INK }));
    }
    out
}

pub enum Reply {
    Capabilities(Capabilities),
    Done(Result<(), String>),
}

fn connection() -> Result<Connection, String> {
    Builder::system()
        .and_then(|b| b.method_timeout(Duration::from_secs(5)).build())
        .map_err(|e| e.to_string())
}

pub fn capabilities(full: bool, seat: Option<String>, can_lock: bool) -> Capabilities {
    let mut caps = Capabilities::default();
    caps.available[1] = can_lock;
    if !full {
        return caps;
    }
    let Ok(conn) = connection() else {
        return caps;
    };
    if let Ok(proxy) = Proxy::new(&conn, LOGIN, LOGIN_PATH, MANAGER) {
        for (i, action) in ACTIONS.into_iter().enumerate() {
            if let Some((can, _)) = action.method() {
                caps.available[i] = (action != Action::Sleep || can_lock)
                    && proxy
                        .call::<_, _, String>(can, &())
                        .is_ok_and(|v| v == "yes");
            }
        }
    }
    if let Some(seat) = seat
        && let Ok(proxy) = Proxy::new(&conn, DM, seat.as_str(), SEAT)
        && proxy.get_property::<bool>("CanSwitch").unwrap_or(false)
    {
        caps.available[2] = can_lock;
        caps.seat = Some(seat.clone());
    }
    caps
}

/// Guard the operation before connecting to any system service. Tests inject an inert call.
fn guarded(
    action: Action,
    full: bool,
    call: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if !full {
        return Err("Available in a full Navi session only".into());
    }
    if action == Action::Lock || action == Action::Logout {
        return Err("Unsupported system action".into());
    }
    call()
}

/// The lock callback completes only after every output has presented protected content.
/// A failed lock must never invoke a power or display-manager action.
pub fn perform_locked(
    action: Action,
    full: bool,
    caps: &Capabilities,
    lock: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if matches!(action, Action::Lock | Action::Sleep | Action::SwitchUser) {
        if !caps.available[1] {
            return Err("Screen locking unavailable".into());
        }
        if action != Action::Lock && !full {
            return Err("Full session required".into());
        }
        lock()?;
        if action == Action::Lock {
            return Ok(());
        }
    }
    perform(action, full, caps)
}

fn perform(action: Action, full: bool, caps: &Capabilities) -> Result<(), String> {
    guarded(action, full, || {
        let i = ACTIONS
            .iter()
            .position(|a| *a == action)
            .ok_or("Unknown action")?;
        if !caps.available[i] {
            return Err("This action is unavailable".into());
        }
        let conn = connection()?;
        if let Some((_, method)) = action.method() {
            // No interactive authorization or force flag: login1 retains inhibitor checks.
            Proxy::new(&conn, LOGIN, LOGIN_PATH, MANAGER)
                .and_then(|p| p.call::<_, _, ()>(method, &(false,)))
                .map_err(|e| e.to_string())
        } else {
            let seat = caps
                .seat
                .as_deref()
                .ok_or("No display-manager seat for this session")?;
            Proxy::new(&conn, DM, seat, SEAT)
                .and_then(|p| p.call::<_, _, ()>("SwitchToGreeter", &()))
                .map_err(|e| e.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nested_actions_never_reach_the_system_and_logout_is_available() {
        assert_eq!(
            capabilities(false, None, false).available,
            [false, false, false, true, false, false]
        );
        for action in ACTIONS {
            assert!(guarded(action, false, || panic!("host action attempted")).is_err());
        }
        for action in [Action::Lock, Action::Logout] {
            assert!(guarded(action, true, || panic!("unsupported action attempted")).is_err());
        }
    }
    #[test]
    fn confirmation_defaults_to_cancel_and_disabled_actions_do_nothing() {
        let mut m = Menu {
            open: true,
            ..Default::default()
        };
        assert_eq!(m.activate(Hit::Row(0)), None);
        assert_eq!(m.confirm, None);
        assert_eq!(m.activate(Hit::Row(3)), None);
        assert_eq!(m.confirm, Some(Action::Logout));
        assert_eq!(m.selected_hit(), Hit::Cancel);
        assert_eq!(m.activate(m.selected_hit()), None);
        assert_eq!(m.confirm, None);
        m.activate(Hit::Row(3));
        m.step(1);
        assert_eq!(m.activate(m.selected_hit()), Some(Action::Logout));
        assert_eq!(m.activate(Hit::Confirm), None);
        m.busy = true;
        assert_eq!(m.activate(Hit::Row(3)), None);
    }
    #[test]
    fn failed_lock_aborts_sleep_and_switch_before_any_system_call() {
        let caps = Capabilities {
            available: [true; 6],
            seat: None,
        };
        for action in [Action::Lock, Action::Sleep, Action::SwitchUser] {
            assert_eq!(
                perform_locked(action, true, &caps, || Err("injected lock failure".into())),
                Err("injected lock failure".into())
            );
        }
        assert!(perform_locked(Action::Lock, false, &caps, || Ok(())).is_ok());
        assert!(
            perform_locked(Action::Sleep, false, &caps, || panic!(
                "nested power action"
            ))
            .is_err()
        );
        assert_eq!(
            capabilities(false, None, true).available,
            [false, true, false, true, false, false]
        );
    }
    #[test]
    fn ac7_band_and_menu_float_inset() {
        assert_eq!(BAND_MARGIN, [12, 12, 0, 12]);
        let mut m = Menu::default();
        assert_eq!(m.input(), [BUTTON]);
        m.open = true;
        let band = band(800, false, ("12:00".into(), 40), None);
        let Some(&Item::Panel(b, ..)) = band.first() else {
            panic!("the band is a panel")
        };
        for confirm in [None, Some(Action::Logout)] {
            m.confirm = confirm;
            let p = m.panel();
            assert_eq!((p.x, p.y), (b.x, b.y + b.h + INSET));
            assert_eq!(m.input(), [BUTTON, p]);
            assert_eq!(m.height(), p.y + p.h);
        }
    }

    #[test]
    fn menu_padding_matches_the_launcher() {
        let mut m = Menu {
            open: true,
            ..Default::default()
        };
        for message in ["", "Two\nlines"] {
            m.message = message.into();
            let p = m.panel();
            let items = m.draw();
            let texts: Vec<Rect> = items
                .iter()
                .filter_map(|i| match i {
                    Item::Text(_, r, _) => Some(*r),
                    _ => None,
                })
                .collect();
            assert_eq!((texts[0].x, texts[0].y), (p.x + PAD, p.y + PAD), "heading");
            assert_eq!(m.row(0).x, p.x + PAD);
            assert_eq!(m.row(0).x + m.row(0).w, p.x + p.w - PAD);
            let last = texts.last().unwrap();
            assert_eq!(last.y + last.h, p.y + p.h - PAD, "{message:?}");
        }
        m.activate(Hit::Row(3));
        let p = m.panel();
        assert_eq!(m.cancel().y + m.cancel().h, p.y + p.h - PAD);
    }

    #[test]
    fn menu_geometry_keyboard_and_hit_targets_match() {
        let mut m = Menu::default();
        assert_eq!(m.height(), BAND_H);
        assert_eq!(m.hit(30, LIST_Y + 1), None);
        m.open = true;
        for i in 0..ACTIONS.len() {
            let r = m.row(i);
            assert_eq!(m.hit(r.x, r.y), Some(Hit::Row(i)));
            assert_eq!(m.hit(r.x + r.w, r.y), None);
            assert!(r.y + r.h < m.height());
        }
        m.step(-1);
        assert_eq!(m.selected, ACTIONS.len() - 1);
        m.activate(Hit::Row(3));
        assert_eq!(m.hit(m.cancel().x, m.cancel().y), Some(Hit::Cancel));
        assert_eq!(m.hit(m.accept().x, m.accept().y), Some(Hit::Confirm));
        m.close();
        assert!(!m.open);
        assert_eq!(m.confirm, None);
    }
}
