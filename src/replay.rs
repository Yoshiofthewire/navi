//! Debug builds only: `NAVI_INPUT_REPLAY=<file>` plays a pointer script through the same handlers
//! as real input, so drags can be tested nested on a host with no input-injection tool.
//!
//! One step per line, in global logical px: `wait <ms>`, `move <x> <y>`, `press`, `release`
//! (the left button). Blank lines and `#` comments are skipped. Any other line is an error, and
//! then nothing is replayed.

use std::time::Duration;

use smithay::reexports::calloop::{
    LoopHandle,
    timer::{TimeoutAction, Timer},
};
use tracing::{error, info};

use crate::CalloopData;

const BTN_LEFT: u32 = 0x110;
const MAX_BYTES: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Step {
    Wait(u64),
    Move(i32, i32),
    Press,
    Release,
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
            ["press"] => Some(Step::Press),
            ["release"] => Some(Step::Release),
            _ => None,
        };
        steps.push(step.ok_or_else(|| format!("line {}: {line:?}", n + 1))?);
    }
    Ok(steps)
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
        "NAVI_INPUT_REPLAY: replaying pointer input"
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
                Step::Press => state.pointer_button(BTN_LEFT, true, true, time),
                Step::Release => state.pointer_button(BTN_LEFT, true, false, time),
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
        let script = "# drag a\nwait 500\n\nmove 100 2\npress\nmove -5 1439\nrelease\n";
        assert_eq!(
            parse(script),
            Ok(vec![
                Step::Wait(500),
                Step::Move(100, 2),
                Step::Press,
                Step::Move(-5, 1439),
                Step::Release,
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
            "click",
            "key a",
        ] {
            let script = format!("move 1 1\n{bad}\nrelease");
            assert_eq!(parse(&script), Err(format!("line 2: {bad:?}")), "{bad}");
        }
    }
}
