//! Private navi-bar ↔ compositor channel. Never inherited by launched apps. Every message is one
//! byte, so requests, replies and toggles can share the stream without framing.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::mpsc,
    time::Duration,
};

/// navi-bar → navi requests.
pub const CAN_LOCK: u8 = 0;
pub const LOCK: u8 = 1;
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
/// Slot n (1..=9) is `SLOT_1 + n - 1`.
pub const SLOT_1: u8 = 5;
const SLOT_9: u8 = SLOT_1 + 8;

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
}

impl Command {
    // navi's side; navi-bar only decodes.
    #[allow(dead_code)]
    pub fn byte(self) -> u8 {
        match self {
            Command::ToggleMenu => TOGGLE_MENU,
            Command::Dock(DockKey::Cycle { forward: true }) => CYCLE_NEXT,
            Command::Dock(DockKey::Cycle { forward: false }) => CYCLE_PREV,
            Command::Dock(DockKey::Slot(n)) => SLOT_1 + n.clamp(1, 9) - 1,
        }
    }

    pub fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            TOGGLE_MENU => Command::ToggleMenu,
            CYCLE_NEXT => Command::Dock(DockKey::Cycle { forward: true }),
            CYCLE_PREV => Command::Dock(DockKey::Cycle { forward: false }),
            SLOT_1..=SLOT_9 => Command::Dock(DockKey::Slot(byte - SLOT_1 + 1)),
            _ => return None,
        })
    }
}

/// navi-bar's end: requests go out on `stream`, replies come back from the reader thread.
pub struct Control {
    stream: UnixStream,
    replies: mpsc::Receiver<u8>,
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
                other => match Command::from_byte(other) {
                    Some(c) => command(c),
                    None => tracing::warn!(byte = other, "unknown control byte ignored"),
                },
            }
        }
    });
    Ok(Control { stream, replies })
}

/// Asks navi whether it can lock (`lock` false), or to lock now.
pub fn request(control: &Control, lock: bool) -> Result<(), String> {
    // A reply that came after an earlier request timed out isn't this one's.
    while control.replies.try_recv().is_ok() {}
    let mut stream = &control.stream;
    stream
        .write_all(&[if lock { LOCK } else { CAN_LOCK }])
        .map_err(|e| e.to_string())?;
    match control.replies.recv_timeout(Duration::from_secs(15)) {
        Ok(YES) => Ok(()),
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
        let waiting = std::thread::spawn(move || (request(&control, true), control));
        assert_eq!(answer(&mut navi, &[TOGGLE_MENU, 9], YES), LOCK);
        let (result, control) = waiting.join().unwrap();
        assert_eq!(result, Ok(()));
        toggles.recv_timeout(Duration::from_secs(5)).unwrap();
        // The reader survived the unknown byte: replies and toggles still flow.
        let waiting = std::thread::spawn(move || request(&control, false));
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
        sent.extend([CYCLE_NEXT, CYCLE_PREV, 14, 255, TOGGLE_MENU]);
        // They all arrive before the reply, and the request still gets it.
        let waiting = std::thread::spawn(move || request(&control, false));
        assert_eq!(answer(&mut navi, &sent, YES), CAN_LOCK);
        assert_eq!(waiting.join().unwrap(), Ok(()));
        let mut want: Vec<_> = (1..=9).map(|n| Command::Dock(DockKey::Slot(n))).collect();
        want.extend([
            Command::Dock(DockKey::Cycle { forward: true }),
            Command::Dock(DockKey::Cycle { forward: false }),
            Command::ToggleMenu,
        ]);
        for w in &want {
            assert_eq!(commands.recv_timeout(Duration::from_secs(5)).unwrap(), *w);
        }
        assert!(commands.try_recv().is_err(), "14 and 255 ignored");
        // navi's encoding is the reader's decoding.
        for c in want {
            assert_eq!(Command::from_byte(c.byte()), Some(c));
        }
        for b in [NO, YES, 14, 255] {
            assert_eq!(Command::from_byte(b), None);
        }
    }
}
