//! xdg-activation rules: who gets a token, and when one still works.

use std::time::Duration;

/// How long a token works after it was made.
pub const TTL: Duration = Duration::from_secs(10);

/// A token is made only for a request that carries a seat serial and comes from the surface that
/// holds keyboard focus, so a background app can't mint one to steal focus.
pub fn may_issue(serial: bool, focused: bool) -> bool {
    serial && focused
}

/// A token works once, up to `TTL` after it was made.
pub fn may_redeem(first_use: bool, age: Duration) -> bool {
    first_use && age <= TTL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac9_tokens_need_serial_focus_and_freshness() {
        assert!(may_issue(true, true));
        assert!(!may_issue(false, true), "no serial");
        assert!(!may_issue(true, false), "not focused");
        assert!(may_redeem(true, Duration::ZERO));
        assert!(may_redeem(true, Duration::from_secs(10)));
        assert!(!may_redeem(true, Duration::from_millis(10_001)));
        assert!(!may_redeem(false, Duration::ZERO), "second use");
    }
}
