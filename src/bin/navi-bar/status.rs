//! The status corner's text: the battery, then the local time.

use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use jiff::Zoned;
use tracing::debug;

/// Seconds between refreshes. A multiple divides 60, so each minute starts with one.
const TICK_S: i64 = 30;
/// A sysfs value is a word or a number; anything longer is not one.
const MAX_VALUE: u64 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Battery {
    pub percent: u32,
    pub charging: bool,
}

/// `BAT 87% +  Tue 04 Nov 14:05`, or the clock alone without a battery.
pub fn format(now: &Zoned, battery: Option<Battery>) -> String {
    let clock = now.strftime("%a %d %b %H:%M").to_string();
    match battery {
        Some(b) => {
            let plus = if b.charging { " +" } else { "" };
            format!("BAT {}%{plus}  {clock}", b.percent)
        }
        None => clock,
    }
}

/// Until the next half-minute: the clock changes on the minute, the battery is read every 30 s.
pub fn next_tick(now: &Zoned) -> Duration {
    let into =
        i64::from(now.second()) % TICK_S * 1_000_000_000 + i64::from(now.subsec_nanosecond());
    Duration::from_nanos((TICK_S * 1_000_000_000 - into) as u64)
}

/// The system's batteries in `dir` (`/sys/class/power_supply`), not a mouse's or headset's: the
/// mean capacity, and charging if any is.
pub fn battery(dir: &Path) -> Option<Battery> {
    let mut found: Vec<(u32, bool)> = Vec::new();
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if value(&p, "type").as_deref() != Some("Battery")
            || value(&p, "scope").as_deref() == Some("Device")
        {
            continue;
        }
        match value(&p, "capacity").and_then(|c| c.parse::<u32>().ok()) {
            Some(c) if c <= 100 => {
                found.push((c, value(&p, "status").as_deref() == Some("Charging")))
            }
            _ => debug!(path = %p.display(), "battery skipped: no capacity"),
        }
    }
    let n = found.len() as u32;
    let sum: u32 = found.iter().map(|b| b.0).sum();
    (n > 0).then(|| Battery {
        percent: (2 * sum + n) / (2 * n),
        charging: found.iter().any(|b| b.1),
    })
}

/// `dir/name`, trimmed, if it has at most `MAX_VALUE` bytes.
fn value(dir: &Path, name: &str) -> Option<String> {
    let mut s = String::new();
    File::open(dir.join(name))
        .ok()?
        .take(MAX_VALUE + 1)
        .read_to_string(&mut s)
        .ok()?;
    (s.len() as u64 <= MAX_VALUE).then(|| s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Tmp;
    use jiff::{
        Timestamp,
        tz::{Offset, TimeZone},
    };

    /// Tue 04 Nov 2025 14:05:00 at +01:00, plus `ms`.
    fn at(ms: i64) -> Zoned {
        let t = Timestamp::from_millisecond(1_762_261_500_000 + ms).unwrap();
        t.to_zoned(TimeZone::fixed(Offset::constant(1)))
    }

    fn supply(root: &Path, name: &str, files: &[(&str, &str)]) {
        let d = root.join(name);
        fs::create_dir_all(&d).unwrap();
        for (f, v) in files {
            fs::write(d.join(f), v).unwrap();
        }
    }

    #[test]
    fn ac6_clock_and_tick() {
        assert_eq!(format(&at(0), None), "Tue 04 Nov 14:05");
        assert_eq!(next_tick(&at(59_500)), Duration::from_millis(500));
        assert_eq!(next_tick(&at(10_000)), Duration::from_secs(20));
        assert_eq!(next_tick(&at(0)), Duration::from_secs(30));
    }

    #[test]
    fn ac6_batteries() {
        let tmp = Tmp::new("power");
        let root = &tmp.0;
        let line = |root: &Path| format(&at(0), battery(root));
        assert_eq!(line(root), "Tue 04 Nov 14:05", "no battery");
        supply(root, "AC", &[("type", "Mains\n"), ("online", "1\n")]);
        supply(
            root,
            "hid-mouse",
            &[
                ("type", "Battery\n"),
                ("scope", "Device\n"),
                ("capacity", "5\n"),
            ],
        );
        assert_eq!(
            line(root),
            "Tue 04 Nov 14:05",
            "mains and a mouse don't count"
        );
        supply(
            root,
            "BAT0",
            &[
                ("type", "Battery\n"),
                ("capacity", "87\n"),
                ("status", "Discharging\n"),
            ],
        );
        assert_eq!(line(root), "BAT 87%  Tue 04 Nov 14:05");

        let tmp = Tmp::new("power2");
        let root = &tmp.0;
        supply(
            root,
            "BAT0",
            &[
                ("type", "Battery\n"),
                ("capacity", "80\n"),
                ("status", "Discharging\n"),
            ],
        );
        supply(
            root,
            "BAT1",
            &[
                ("type", "Battery\n"),
                ("scope", "System\n"),
                ("capacity", "91\n"),
                ("status", "Charging\n"),
            ],
        );
        supply(
            root,
            "BAT2",
            &[
                ("type", "Battery\n"),
                ("capacity", "abc\n"),
                ("status", "Charging\n"),
            ],
        );
        let long = format!("{}50", "0".repeat(68));
        supply(root, "BAT3", &[("type", "Battery\n"), ("capacity", &long)]);
        assert_eq!(long.len(), 70);
        assert_eq!(line(root), "BAT 86% +  Tue 04 Nov 14:05");
        assert_eq!(battery(&root.join("missing")), None);
    }
}
