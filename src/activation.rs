//! xdg-activation rules: who gets a token, and when one still works.

use std::time::Duration;

/// How long a token works after it was made.
pub const TTL: Duration = Duration::from_secs(10);

/// `a` is `b` or later, with serials wrapping around.
fn no_older(a: u32, b: u32) -> bool {
    a.wrapping_sub(b) < 1 << 31
}

/// A token is made only for a request from the surface holding keyboard focus, carrying a serial
/// from that focus period: no older than the focus's enter (`focus_since`, `None` when the
/// surface isn't focused) and not newer than `now`, the latest serial navi has made. A made-up
/// serial is refused, so a focused app can't mint tokens without an input event.
pub fn may_issue(serial: Option<u32>, focus_since: Option<u32>, now: u32) -> bool {
    match (serial, focus_since) {
        (Some(s), Some(since)) => no_older(s, since) && no_older(now, s),
        _ => false,
    }
}

/// A token works once, up to `TTL` after it was made, and only while focus hasn't moved away:
/// the surface that asked for it, or the one it activates, still holds the keyboard.
pub fn may_redeem(first_use: bool, age: Duration, focus_kept: bool) -> bool {
    first_use && age <= TTL && focus_kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac9_tokens_need_a_real_serial_from_the_focus() {
        // Focus entered at serial 100; navi has made serials up to 120.
        assert!(may_issue(Some(110), Some(100), 120));
        assert!(may_issue(Some(100), Some(100), 120));
        assert!(!may_issue(None, Some(100), 120), "no serial");
        assert!(!may_issue(Some(110), None, 120), "not focused");
        assert!(
            !may_issue(Some(0), Some(100), 120),
            "made up, before the focus"
        );
        assert!(
            !may_issue(Some(99), Some(100), 120),
            "from an earlier focus"
        );
        assert!(!may_issue(Some(5000), Some(100), 120), "not made yet");
        // Across the wrap.
        assert!(may_issue(Some(3), Some(u32::MAX - 2), 10));
        assert!(!may_issue(Some(u32::MAX - 5), Some(u32::MAX - 2), 10));
    }

    #[test]
    fn ac9_tokens_are_single_use_fresh_and_need_focus_kept() {
        let s = Duration::from_secs;
        assert!(may_redeem(true, Duration::ZERO, true));
        assert!(may_redeem(true, s(10), true));
        assert!(!may_redeem(true, Duration::from_millis(10_001), true));
        assert!(!may_redeem(false, Duration::ZERO, true), "second use");
        // Issued while focused, then focus moved to another surface; redeemed at 1 s.
        assert!(!may_redeem(true, s(1), false), "focus moved");
    }
}
