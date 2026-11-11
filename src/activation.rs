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
/// serial is refused. The enter serial itself counts, so a window navi focused on its own (a new
/// one) can mint a token without input; `minted_by_user` keeps that from raising as the user.
pub fn may_issue(serial: Option<u32>, focus_since: Option<u32>, now: u32) -> bool {
    match (serial, focus_since) {
        (Some(s), Some(since)) => no_older(s, since) && no_older(now, s),
        _ => false,
    }
}

/// Whether a token's activation raises as the user would: it came from navi-bar (the dock, the
/// launcher), or from a window that holds the keyboard because the user gave it (a click, the
/// dock, Super+N, a user activation). A window that took the keyboard on its own (new on map) or
/// by navi's refocus mints tokens that only mark the window (`on_redeem`).
pub fn minted_by_user(navi_bar: bool, focus_by_user: bool) -> bool {
    navi_bar || focus_by_user
}

/// Whether redeeming a token raises as the user would: navi-bar's always; another only if it was
/// minted from a focus the user gave and the keyboard hasn't moved since (`moves`, counted at
/// mint and now). A stale user token, redeemed after the user went elsewhere, still activates
/// but raises nothing.
pub fn user_raise(bar: bool, user: bool, minted_at: u64, now: u64) -> bool {
    bar || user && minted_at == now
}

/// What redeeming a valid token does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Redeem {
    /// The user's: restore, raise and focus the window.
    Focus,
    /// Anyone else's: leave the keyboard and the stacking order alone; mark the window's dock
    /// entry and tab until the user switches to it.
    Mark,
}

/// A user token focuses; any other only marks.
pub fn on_redeem(user: bool) -> Redeem {
    match user {
        true => Redeem::Focus,
        false => Redeem::Mark,
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

    /// Round 6 security (s17-act): a window that got the keyboard by mapping can mint a token with
    /// its enter serial, but that token is not a user's; the dock's and a clicked window's are.
    #[test]
    fn only_a_users_focus_mints_a_user_token() {
        // The new window's own enter serial is accepted ...
        assert!(may_issue(Some(29), Some(29), 30));
        // ... but it isn't the user's.
        assert!(!minted_by_user(false, false));
        assert!(minted_by_user(false, true), "a window the user clicked");
        assert!(minted_by_user(true, false), "navi-bar");
    }

    /// Round 7 security (s18-stale): a user token redeemed after the keyboard moved (the user went
    /// to another app, a new window took it) raises nothing; navi-bar's always does.
    #[test]
    fn a_user_token_raises_only_while_its_focus_holds() {
        assert!(
            user_raise(false, true, 5, 5),
            "asker still holds the user's focus"
        );
        assert!(!user_raise(false, true, 5, 6), "the keyboard moved since");
        assert!(
            !user_raise(false, true, 5, 9),
            "moved to the target, a new window"
        );
        assert!(!user_raise(false, false, 5, 5), "never the user's");
        assert!(user_raise(true, false, 5, 9), "navi-bar");
    }

    /// Round 11 (Yoshi's decision): an activation that isn't the user's neither raises nor takes
    /// the keyboard; it only marks the window. A user token focuses.
    #[test]
    fn only_a_user_activation_takes_the_keyboard() {
        assert_eq!(on_redeem(user_raise(false, true, 5, 5)), Redeem::Focus);
        assert_eq!(
            on_redeem(user_raise(true, false, 5, 9)),
            Redeem::Focus,
            "navi-bar"
        );
        assert_eq!(
            on_redeem(user_raise(false, true, 5, 6)),
            Redeem::Mark,
            "stale"
        );
        assert_eq!(
            on_redeem(user_raise(false, false, 5, 5)),
            Redeem::Mark,
            "self-focused"
        );
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
