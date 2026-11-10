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
/// navi → navi-bar, unsolicited: open or close the app menu.
pub const TOGGLE_MENU: u8 = 2;

/// navi-bar's end: requests go out on `stream`, replies come back from the reader thread.
pub struct Control {
    stream: UnixStream,
    replies: mpsc::Receiver<u8>,
}

/// Starts the one reader of navi-bar's end: toggles go to `toggle`, replies to `request`.
pub fn reader(
    stream: UnixStream,
    mut toggle: impl FnMut() + Send + 'static,
) -> std::io::Result<Control> {
    let mut read = stream.try_clone()?;
    let (tx, replies) = mpsc::channel();
    std::thread::spawn(move || {
        let mut byte = [0];
        while read.read_exact(&mut byte).is_ok() {
            match byte[0] {
                TOGGLE_MENU => toggle(),
                NO | YES => {
                    if tx.send(byte[0]).is_err() {
                        break;
                    }
                }
                other => tracing::warn!(byte = other, "unknown control byte ignored"),
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
        let control = reader(bar, move || toggled.send(()).unwrap()).unwrap();
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
}
