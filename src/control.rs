//! Private navi-bar → compositor lock channel. Never inherited by launched apps.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    time::Duration,
};

/// Fixed-size messages keep the private protocol bounded. No paths or commands cross it.
pub fn request(stream: &UnixStream, lock: bool) -> Result<(), String> {
    let mut stream = stream;
    stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(&[u8::from(lock)])
        .map_err(|e| e.to_string())?;
    let mut reply = [0];
    stream.read_exact(&mut reply).map_err(|e| e.to_string())?;
    match reply[0] {
        1 => Ok(()),
        _ => Err("Screen lock unavailable. Install swaylock and check the Navi log.".into()),
    }
}
