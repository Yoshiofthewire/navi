//! Debug builds only: `NAVI_INPUT_REPLAY=<file>` plays a pointer and keyboard script through the
//! same handlers as real input, so drags and typing can be tested nested on a host with no
//! input-injection tool.
//!
//! One step per line, in global logical px: `wait <ms>`, `move <x> <y>`, `press`, `release`
//! (the left button), `press right`, `release right`, `key <evdev keycode>` (press and
//! release), and `down <code>` / `up <code>` to hold a key, such as a modifier. Blank lines and
//! `#` comments are skipped. Any other line is an error, and then nothing is replayed.

use std::time::Duration;

use smithay::{
    backend::input::KeyState,
    reexports::calloop::{
        LoopHandle,
        timer::{TimeoutAction, Timer},
    },
};
use tracing::{error, info};

use crate::CalloopData;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const MAX_BYTES: u64 = 1 << 20;
/// evdev's KEY_MAX.
const KEY_MAX: u32 = 0x2ff;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Wait(u64),
    Move(i32, i32),
    /// A button down or up, by evdev code.
    Press(u32),
    Release(u32),
    Key(u32),
    Down(u32),
    Up(u32),
}

fn parse(script: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (n, line) in script.lines().enumerate() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let step = match words.as_slice() {
            [] => continue,
            [w, ..] if w.starts_with('#') => continue,
            ["wait", ms] => ms.parse().ok().map(Step::Wait),
            ["move", x, y] => x
                .parse()
                .ok()
                .zip(y.parse().ok())
                .map(|(x, y)| Step::Move(x, y)),
            ["press"] => Some(Step::Press(BTN_LEFT)),
            ["release"] => Some(Step::Release(BTN_LEFT)),
            ["press", "right"] => Some(Step::Press(BTN_RIGHT)),
            ["release", "right"] => Some(Step::Release(BTN_RIGHT)),
            ["key", code] => keycode(code).map(Step::Key),
            ["down", code] => keycode(code).map(Step::Down),
            ["up", code] => keycode(code).map(Step::Up),
            _ => None,
        };
        steps.push(step.ok_or_else(|| format!("line {}: {line:?}", n + 1))?);
    }
    Ok(steps)
}

fn keycode(s: &str) -> Option<u32> {
    s.parse().ok().filter(|&c| c <= KEY_MAX)
}

fn load(path: &str) -> Result<Vec<Step>, String> {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > MAX_BYTES {
        return Err(format!("{len} bytes, over {MAX_BYTES}"));
    }
    parse(&std::fs::read_to_string(path).map_err(|e| e.to_string())?)
}

/// Starts the replay if `NAVI_INPUT_REPLAY` is set.
pub fn start(handle: &LoopHandle<'static, CalloopData>) {
    let Ok(path) = std::env::var("NAVI_INPUT_REPLAY") else {
        return;
    };
    let steps = match load(&path) {
        Ok(steps) => steps,
        Err(e) => {
            error!(path, "NAVI_INPUT_REPLAY not replayed: {e}");
            return;
        }
    };
    info!(
        path,
        steps = steps.len(),
        "NAVI_INPUT_REPLAY: replaying input"
    );
    let mut next = steps.into_iter();
    let timer = handle.insert_source(Timer::immediate(), move |_, _, data| {
        let state = &mut data.state;
        for step in next.by_ref() {
            info!(?step, "replay");
            let time = state.now_ms() as u32;
            match step {
                Step::Wait(ms) => return TimeoutAction::ToDuration(Duration::from_millis(ms)),
                Step::Move(x, y) => state.pointer_motion((f64::from(x), f64::from(y)).into(), time),
                Step::Press(button) => state.pointer_button(button, true, true, time),
                Step::Release(button) => state.pointer_button(button, true, false, time),
                // xkb keycodes are evdev's plus 8.
                Step::Key(code) => {
                    for key in [KeyState::Pressed, KeyState::Released] {
                        state.keyboard_key((code + 8).into(), key, time);
                    }
                }
                Step::Down(code) => state.keyboard_key((code + 8).into(), KeyState::Pressed, time),
                Step::Up(code) => state.keyboard_key((code + 8).into(), KeyState::Released, time),
            }
        }
        info!("replay done");
        TimeoutAction::Drop
    });
    if let Err(e) = timer {
        error!("NAVI_INPUT_REPLAY not replayed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_four_steps() {
        let script = "# drag a\nwait 500\n\nmove 100 2\npress\nmove -5 1439\nrelease\nkey 46\n\
                      down 29\nup 29\npress right\nrelease right\n";
        assert_eq!(
            parse(script),
            Ok(vec![
                Step::Wait(500),
                Step::Move(100, 2),
                Step::Press(BTN_LEFT),
                Step::Move(-5, 1439),
                Step::Release(BTN_LEFT),
                Step::Key(46),
                Step::Down(29),
                Step::Up(29),
                Step::Press(BTN_RIGHT),
                Step::Release(BTN_RIGHT),
            ])
        );
    }

    #[test]
    fn any_bad_line_rejects_the_script() {
        for bad in [
            "wait",
            "wait -1",
            "wait 1.5",
            "move 1",
            "move 1 2 3",
            "move x 2",
            "press 1",
            "press middle",
            "release middle",
            "press right 1",
            "press left",
            "click",
            "key",
            "key x",
            "key -1",
            "key 768",
            "key 1 2",
            "down",
            "down x",
            "down 768",
            "up",
            "up -1",
            "down 29 1",
        ] {
            let script = format!("move 1 1\n{bad}\nrelease");
            assert_eq!(parse(&script), Err(format!("line 2: {bad:?}")), "{bad}");
        }
    }
}
