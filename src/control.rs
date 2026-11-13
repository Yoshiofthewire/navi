//! Private navi-bar ↔ compositor channel. Never inherited by launched apps. Every message is one
//! byte, so requests, replies and toggles can share the stream without framing. The two
//! exceptions, `FORCE_CLOSE` (bar → navi) and `FORCE_OFFER` (navi → bar), are the byte, then a
//! window-list handle's protocol object ID as a little-endian `u32`.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::{Condvar, Mutex, MutexGuard, PoisonError, mpsc},
    time::Duration,
};

/// navi-bar → navi requests.
pub const CAN_LOCK: u8 = 0;
pub const LOCK: u8 = 1;
pub const FORCE_CLOSE: u8 = 2;
/// navi → navi-bar replies.
pub const NO: u8 = 0;
pub const YES: u8 = 1;
/// navi → navi-bar, unsolicited: open or close the app menu, step through or pick a dock app.
pub const TOGGLE_MENU: u8 = 2;
pub const CYCLE_NEXT: u8 = 3;
pub const CYCLE_PREV: u8 = 4;
/// navi → navi-bar, in a window-list handle's states: a dialog of that window waits for the user.
/// Outside the protocol's values, as only navi-bar sees the window list.
#[allow(dead_code)]
pub const ATTENTION: u32 = 0x6e61_7601;
/// navi → navi-bar, in a window-list handle's states: a user close went unanswered for
/// `FORCE_AFTER`, so the window may be force closed.
#[allow(dead_code)]
pub const CLOSE_OVERDUE: u32 = 0x6e61_7602;
/// Slot n (1..=9) is `SLOT_1 + n - 1`.
pub const SLOT_1: u8 = 5;
const SLOT_9: u8 = SLOT_1 + 8;
/// navi → navi-bar, unsolicited: navi closed a client's toplevel past `CLIENT_TOPLEVELS`.
pub const WINDOW_LIMIT: u8 = SLOT_9 + 1;
/// navi → navi-bar, unsolicited, then a handle ID: a ✕ on an overdue window; offer force close.
pub const FORCE_OFFER: u8 = WINDOW_LIMIT + 1;
/// How many toplevels (dialogs included) one client may have open at once; more are closed.
pub const CLIENT_TOPLEVELS: usize = 256;

/// A dock key: Super+1..9 or Super+(Shift+)Tab.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DockKey {
    Slot(u8),
    Cycle { forward: bool },
}

/// What an unsolicited navi → navi-bar byte asks for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    ToggleMenu,
    Dock(DockKey),
    WindowLimit,
    /// The window whose window-list handle has this object ID on navi-bar's connection.
    ForceOffer(u32),
}

impl Command {
    // navi's side; navi-bar only decodes.
    #[allow(dead_code)]
    pub fn bytes(self) -> Vec<u8> {
        match self {
            Command::ForceOffer(id) => [&[FORCE_OFFER][..], &id.to_le_bytes()].concat(),
            c => vec![c.byte()],
        }
    }

    /// The first (for every command but `ForceOffer`, the only) byte.
    #[allow(dead_code)]
    pub fn byte(self) -> u8 {
        match self {
            Command::ToggleMenu => TOGGLE_MENU,
            Command::Dock(DockKey::Cycle { forward: true }) => CYCLE_NEXT,
            Command::Dock(DockKey::Cycle { forward: false }) => CYCLE_PREV,
            Command::Dock(DockKey::Slot(n)) => SLOT_1 + n.clamp(1, 9) - 1,
            Command::WindowLimit => WINDOW_LIMIT,
            Command::ForceOffer(_) => FORCE_OFFER,
        }
    }

    /// A one-byte command; `FORCE_OFFER` needs its ID, so `reader` decodes it.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            TOGGLE_MENU => Command::ToggleMenu,
            CYCLE_NEXT => Command::Dock(DockKey::Cycle { forward: true }),
            CYCLE_PREV => Command::Dock(DockKey::Cycle { forward: false }),
            SLOT_1..=SLOT_9 => Command::Dock(DockKey::Slot(byte - SLOT_1 + 1)),
            WINDOW_LIMIT => Command::WindowLimit,
            _ => return None,
        })
    }
}

/// A navi-bar → navi request.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Request {
    CanLock,
    Lock,
    /// End the window whose window-list handle has this object ID on navi-bar's connection.
    ForceClose(u32),
}

impl Request {
    pub fn bytes(self) -> Vec<u8> {
        match self {
            Request::CanLock => vec![CAN_LOCK],
            Request::Lock => vec![LOCK],
            Request::ForceClose(id) => [&[FORCE_CLOSE][..], &id.to_le_bytes()].concat(),
        }
    }

    /// navi's side: the next request, or `None` on EOF, a short read or an unknown byte.
    // navi-bar only writes requests.
    #[allow(dead_code)]
    pub fn read(r: &mut impl Read) -> Option<Self> {
        let mut byte = [0];
        r.read_exact(&mut byte).ok()?;
        Some(match byte[0] {
            CAN_LOCK => Request::CanLock,
            LOCK => Request::Lock,
            FORCE_CLOSE => {
                let mut id = [0; 4];
                r.read_exact(&mut id).ok()?;
                Request::ForceClose(u32::from_le_bytes(id))
            }
            _ => return None,
        })
    }
}

/// navi-bar's end: requests go out on `stream`, replies come back from the reader thread. One
/// request at a time, and a lock request takes the next turn ahead of waiting force closes.
pub struct Control {
    stream: UnixStream,
    replies: Mutex<mpsc::Receiver<u8>>,
    turn: Mutex<Turn>,
    next: Condvar,
}

#[derive(Default)]
struct Turn {
    busy: bool,
    /// Lock requests waiting for the stream.
    urgent: usize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The stream is the holder's until dropped.
struct Held<'a>(&'a Control);

impl Drop for Held<'_> {
    fn drop(&mut self) {
        lock(&self.0.turn).busy = false;
        self.0.next.notify_all();
    }
}

impl Control {
    fn take(&self, urgent: bool) -> Held<'_> {
        let mut t = lock(&self.turn);
        t.urgent += usize::from(urgent);
        while t.busy || (!urgent && t.urgent > 0) {
            t = self.next.wait(t).unwrap_or_else(PoisonError::into_inner);
        }
        t.urgent -= usize::from(urgent);
        t.busy = true;
        Held(self)
    }
}

/// Starts the one reader of navi-bar's end: commands go to `command`, replies to `request`.
pub fn reader(
    stream: UnixStream,
    mut command: impl FnMut(Command) + Send + 'static,
) -> std::io::Result<Control> {
    let mut read = stream.try_clone()?;
    let (tx, replies) = mpsc::channel();
    std::thread::spawn(move || {
        let mut byte = [0];
        while read.read_exact(&mut byte).is_ok() {
            match byte[0] {
                NO | YES => {
                    if tx.send(byte[0]).is_err() {
                        break;
                    }
                }
                FORCE_OFFER => {
                    let mut id = [0; 4];
                    if read.read_exact(&mut id).is_err() {
                        break;
                    }
                    command(Command::ForceOffer(u32::from_le_bytes(id)));
                }
                other => match Command::from_byte(other) {
                    Some(c) => command(c),
                    None => tracing::warn!(byte = other, "unknown control byte ignored"),
                },
            }
        }
    });
    Ok(Control {
        stream,
        replies: Mutex::new(replies),
        turn: Mutex::default(),
        next: Condvar::new(),
    })
}

/// Sends `request` and waits for navi's answer: `Ok` for `YES`. Waits for its turn first: a
/// lock or lock check goes ahead of force closes not yet sent.
pub fn request(control: &Control, request: Request) -> Result<(), String> {
    let _held = control.take(!matches!(request, Request::ForceClose(_)));
    let replies = lock(&control.replies);
    // A reply that came after an earlier request timed out isn't this one's.
    while replies.try_recv().is_ok() {}
    let mut stream = &control.stream;
    stream
        .write_all(&request.bytes())
        .map_err(|e| e.to_string())?;
    match replies.recv_timeout(Duration::from_secs(15)) {
        Ok(YES) => Ok(()),
        Ok(_) if matches!(request, Request::ForceClose(_)) => Err("refused by navi".into()),
        Ok(_) => Err("Screen lock unavailable. Install swaylock and check the Navi log.".into()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// navi's side of one request: reads it, then sends `reply`, after `before` if any.
    fn answer(navi: &mut UnixStream, before: &[u8], reply: u8) -> u8 {
        let mut asked = [9];
        navi.read_exact(&mut asked).unwrap();
        navi.write_all(before).unwrap();
        navi.write_all(&[reply]).unwrap();
        asked[0]
    }

    #[test]
    fn reader_routes_toggles_and_replies() {
        let (bar, mut navi) = UnixStream::pair().unwrap();
        let (toggled, toggles) = mpsc::channel();
        // Byte 9 below is now slot 5; this callback still counts toggles only.
        let control = reader(bar, move |c| {
            if c == Command::ToggleMenu {
                toggled.send(()).unwrap()
            }
        })
        .unwrap();
        // A toggle and an unknown byte arrive while a request waits for its reply.
        let waiting = std::thread::spawn(move || (request(&control, Request::Lock), control));
        assert_eq!(answer(&mut navi, &[TOGGLE_MENU, 9], YES), LOCK);
        let (result, control) = waiting.join().unwrap();
        assert_eq!(result, Ok(()));
        toggles.recv_timeout(Duration::from_secs(5)).unwrap();
        // The reader survived the unknown byte: replies and toggles still flow.
        let waiting = std::thread::spawn(move || request(&control, Request::CanLock));
        assert_eq!(answer(&mut navi, &[], NO), CAN_LOCK);
        assert!(waiting.join().unwrap().is_err());
        navi.write_all(&[TOGGLE_MENU]).unwrap();
        toggles.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(toggles.try_recv().is_err(), "one toggle per byte");
    }

    #[test]
    fn reader_routes_dock_keys_and_skips_unknown_bytes() {
        let (bar, mut navi) = UnixStream::pair().unwrap();
        let (tx, commands) = mpsc::channel();
        let control = reader(bar, move |c| tx.send(c).unwrap()).unwrap();
        let mut sent: Vec<u8> = (SLOT_1..=SLOT_1 + 8).collect();
        sent.extend([CYCLE_NEXT, CYCLE_PREV, WINDOW_LIMIT, 16, 255, TOGGLE_MENU]);
        // They all arrive before the reply, and the request still gets it.
        let waiting = std::thread::spawn(move || request(&control, Request::CanLock));
        assert_eq!(answer(&mut navi, &sent, YES), CAN_LOCK);
        assert_eq!(waiting.join().unwrap(), Ok(()));
        let mut want: Vec<_> = (1..=9).map(|n| Command::Dock(DockKey::Slot(n))).collect();
        want.extend([
            Command::Dock(DockKey::Cycle { forward: true }),
            Command::Dock(DockKey::Cycle { forward: false }),
            Command::WindowLimit,
            Command::ToggleMenu,
        ]);
        for w in &want {
            assert_eq!(commands.recv_timeout(Duration::from_secs(5)).unwrap(), *w);
        }
        assert!(commands.try_recv().is_err(), "16 and 255 ignored");
        // navi's encoding is the reader's decoding.
        for c in want {
            assert_eq!(Command::from_byte(c.byte()), Some(c));
        }
        for b in [NO, YES, FORCE_OFFER, 16, 255] {
            assert_eq!(Command::from_byte(b), None);
        }
    }

    /// AC11: the one framed request, both ways.
    #[test]
    fn ac11_force_close_is_five_bytes() {
        let req = Request::ForceClose(0xFF00_0001);
        assert_eq!(req.bytes(), [FORCE_CLOSE, 1, 0, 0, 0xFF]);
        assert_eq!(Request::read(&mut &req.bytes()[..]), Some(req));
        for r in [Request::CanLock, Request::Lock] {
            assert_eq!(Request::read(&mut &r.bytes()[..]), Some(r));
        }
        // A short read, EOF and an unknown byte all end the reader.
        for bad in [&[FORCE_CLOSE, 1, 0][..], &[], &[3], &[255, 0, 0, 0, 0]] {
            assert_eq!(Request::read(&mut &bad[..]), None, "{bad:?}");
        }
    }

    /// AC11: navi-bar's side of a force close: five bytes out, `YES` or `NO` back.
    #[test]
    fn ac11_force_close_request_gets_its_reply() {
        let (bar, mut navi) = UnixStream::pair().unwrap();
        let control = reader(bar, |_| {}).unwrap();
        let waiting = std::thread::spawn(move || {
            let r = request(&control, Request::ForceClose(0xFF00_0001));
            (r, control)
        });
        assert_eq!(
            Request::read(&mut navi),
            Some(Request::ForceClose(0xFF00_0001))
        );
        navi.write_all(&[NO]).unwrap();
        let (result, control) = waiting.join().unwrap();
        assert!(result.is_err());
        let waiting = std::thread::spawn(move || request(&control, Request::Lock));
        assert_eq!(Request::read(&mut navi), Some(Request::Lock));
        navi.write_all(&[YES]).unwrap();
        assert_eq!(waiting.join().unwrap(), Ok(()));
    }

    /// Job 040 AC1: `FORCE_OFFER` and its ID, between one-byte commands and a pending
    /// request's reply.
    #[test]
    fn ac1_force_offer_round_trips_between_commands_and_replies() {
        let (bar, mut navi) = UnixStream::pair().unwrap();
        let (tx, commands) = mpsc::channel();
        let control = reader(bar, move |c| tx.send(c).unwrap()).unwrap();
        let sent = [
            Command::ToggleMenu,
            Command::ForceOffer(0xFF00_0001),
            Command::Dock(DockKey::Slot(3)),
            Command::ForceOffer(7),
            Command::WindowLimit,
        ];
        let before: Vec<u8> = sent.iter().flat_map(|c| c.bytes()).collect();
        assert_eq!(before.len(), 3 + 2 * 5);
        let waiting = std::thread::spawn(move || (request(&control, Request::Lock), control));
        assert_eq!(answer(&mut navi, &before, YES), LOCK);
        let (result, control) = waiting.join().unwrap();
        assert_eq!(result, Ok(()));
        // A NO between two offers is the next request's reply, not part of an ID.
        let waiting = std::thread::spawn(move || request(&control, Request::ForceClose(9)));
        assert_eq!(Request::read(&mut navi), Some(Request::ForceClose(9)));
        let mut after = Command::ForceOffer(0x0102_0304).bytes();
        after.push(NO);
        after.extend(Command::ForceOffer(NO.into()).bytes());
        navi.write_all(&after).unwrap();
        assert!(waiting.join().unwrap().is_err(), "NO");
        let want = sent
            .into_iter()
            .chain([Command::ForceOffer(0x0102_0304), Command::ForceOffer(0)]);
        for w in want {
            assert_eq!(commands.recv_timeout(Duration::from_secs(5)).unwrap(), w);
        }
        assert!(commands.try_recv().is_err());
        assert_eq!(
            Command::ForceOffer(0xFF00_0001).bytes(),
            [FORCE_OFFER, 1, 0, 0, 0xFF]
        );
    }

    /// Job 040 AC1: a truncated offer then EOF delivers nothing.
    #[test]
    fn ac1_a_truncated_force_offer_delivers_nothing() {
        for cut in 1..5 {
            let (bar, mut navi) = UnixStream::pair().unwrap();
            let (tx, commands) = mpsc::channel();
            let _control = reader(bar, move |c| tx.send(c).unwrap()).unwrap();
            let frame = Command::ForceOffer(0xFF00_0001).bytes();
            navi.write_all(&frame[..cut]).unwrap();
            drop(navi);
            // The reader ends at EOF and drops its sender.
            assert_eq!(
                commands.recv_timeout(Duration::from_secs(5)),
                Err(mpsc::RecvTimeoutError::Disconnected),
                "{cut} bytes"
            );
        }
    }

    /// Job 040 AC1: every other command is still the one byte it was.
    #[test]
    fn ac1_one_byte_commands_are_unchanged() {
        let mut all: Vec<_> = (1..=9).map(|n| Command::Dock(DockKey::Slot(n))).collect();
        all.extend([
            Command::ToggleMenu,
            Command::Dock(DockKey::Cycle { forward: true }),
            Command::Dock(DockKey::Cycle { forward: false }),
            Command::WindowLimit,
        ]);
        let bytes: Vec<u8> = all.iter().map(|c| c.byte()).collect();
        assert_eq!(bytes, [5, 6, 7, 8, 9, 10, 11, 12, 13, 2, 3, 4, 14]);
        for c in all {
            assert_eq!(c.bytes(), [c.byte()]);
            assert_eq!(Command::from_byte(c.byte()), Some(c));
        }
    }
}
