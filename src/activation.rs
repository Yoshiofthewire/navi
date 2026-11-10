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

/// Whether focus stayed for a token from `asker` that activates `target`: the keyboard (`focus`)
/// is still on either of them. One exception: a launch from the menu closes it, and navi hands
/// the keyboard back before the app redeems. `handback` is navi's last return of the keyboard
/// from a layer to a window, kept only until focus next moves; the token works if it came from
/// that layer and the keyboard is still on that window.
pub fn focus_kept<S: PartialEq>(
    focus: Option<&S>,
    asker: Option<&S>,
    target: &S,
    handback: Option<&(S, S)>,
) -> bool {
    let Some(focus) = focus else {
        return false;
    };
    asker == Some(focus)
        || focus == target
        || handback.is_some_and(|(layer, window)| asker == Some(layer) && focus == window)
}

/// Keeps the hand-back only while focus stays on the window it went to.
pub fn after_focus_change<S: PartialEq>(
    handback: Option<(S, S)>,
    focus: Option<&S>,
) -> Option<(S, S)> {
    handback.filter(|(_, window)| focus == Some(window))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAR: u8 = 1;
    const KONSOLE: u8 = 2;
    const NEW: u8 = 3;
    const OTHER: u8 = 4;

    #[test]
    fn a_dock_click_that_focuses_the_taskbar_can_get_a_token() {
        // The click gives the taskbar the keyboard (enter serial 40); navi then sends the button
        // press with the next serial, which the bar's token request carries.
        assert!(may_issue(Some(41), Some(40), 41));
        // A press serial made before the enter, as navi used to send, would be refused.
        assert!(!may_issue(Some(39), Some(40), 41));
    }

    #[test]
    fn a_menu_launch_survives_navis_own_hand_back() {
        let back = after_focus_change(Some((BAR, KONSOLE)), Some(&KONSOLE));
        assert!(focus_kept(Some(&KONSOLE), Some(&BAR), &NEW, back.as_ref()));
        // Without a hand-back, the same redeem is refused, as before.
        assert!(!focus_kept(Some(&KONSOLE), Some(&BAR), &NEW, None));
    }

    #[test]
    fn any_other_focus_change_ends_the_hand_back() {
        // The user clicked another window, even if they clicked back.
        let back = after_focus_change(Some((BAR, KONSOLE)), Some(&OTHER));
        assert_eq!(back, None);
        let back = after_focus_change(back, Some(&KONSOLE));
        assert!(!focus_kept(Some(&KONSOLE), Some(&BAR), &NEW, back.as_ref()));
        // The keyboard went to nobody.
        assert_eq!(after_focus_change(Some((BAR, KONSOLE)), None), None);
        // A token from another surface, or the keyboard elsewhere, isn't covered.
        let back = Some((BAR, KONSOLE));
        assert!(!focus_kept(
            Some(&KONSOLE),
            Some(&OTHER),
            &NEW,
            back.as_ref()
        ));
        assert!(!focus_kept(Some(&OTHER), Some(&BAR), &NEW, back.as_ref()));
        assert!(!focus_kept(None, Some(&BAR), &NEW, back.as_ref()));
    }

    #[test]
    fn focus_is_kept_on_the_asker_or_the_target() {
        assert!(focus_kept(Some(&BAR), Some(&BAR), &NEW, None));
        assert!(focus_kept(Some(&NEW), Some(&BAR), &NEW, None));
        assert!(!focus_kept(Some(&OTHER), Some(&BAR), &NEW, None));
        assert!(!focus_kept(None, None, &NEW, None));
        assert!(!focus_kept(Some(&OTHER), None, &NEW, None));
    }

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
