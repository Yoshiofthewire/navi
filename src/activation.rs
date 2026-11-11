//! xdg-activation rules: who gets a token, and when one still works.

use std::time::{Duration, Instant};

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

/// How long a layer must have been on screen before a press on it can mint a press token: a
/// card revealed under a click (fullscreen or the lock ending) can't catch it.
pub const SHOWN_FOR: Duration = Duration::from_millis(500);

/// A token navi-bar asks for from a pointer press on a surface that never holds the keyboard (the
/// notification cards): only navi-bar's, never while locked, and only with the serial of the
/// latest left-button press navi delivered (`press`: serial, surface, and how long that surface
/// had been shown then), on that same surface, shown at least `SHOWN_FOR`.
pub fn may_issue_press<S: PartialEq>(
    navi_bar: bool,
    locked: bool,
    serial: Option<u32>,
    asker: Option<&S>,
    press: Option<&(u32, S, Option<Duration>)>,
) -> bool {
    navi_bar
        && !locked
        && matches!((serial, asker, press), (Some(s), Some(a), Some((p, on, shown)))
            if s == *p && a == on && shown.is_some_and(|d| d >= SHOWN_FOR))
}

/// Whether a layer is on screen: nothing is while locked, and a fullscreen window hides its
/// output's Top layer (the taskbar, the cards).
pub fn layer_shown(top: bool, fullscreen: bool, locked: bool) -> bool {
    !locked && !(top && fullscreen)
}

/// Keeps when each shown layer surface came on screen: a newly shown one starts now, a hidden
/// one is dropped, so showing it again restarts its clock.
pub fn track_shown<S: PartialEq + Clone>(
    since: &mut Vec<(S, Instant)>,
    surface: &S,
    shown: bool,
    now: Instant,
) {
    let at = since.iter().position(|(s, _)| s == surface);
    match (shown, at) {
        (true, None) => since.push((surface.clone(), now)),
        (false, Some(i)) => {
            since.remove(i);
        }
        _ => {}
    }
}

/// How long `surface` has been on screen at `now`, if it is.
pub fn shown_for<S: PartialEq>(
    since: &[(S, Instant)],
    surface: &S,
    now: Instant,
) -> Option<Duration> {
    since
        .iter()
        .find(|(s, _)| s == surface)
        .map(|(_, t)| now.saturating_duration_since(*t))
}

/// Whether focus stayed for a press token: neither the keyboard moved nor the user pressed the
/// left button again since it was minted (keyboard moves and presses, counted at mint and now),
/// or the keyboard is already on the window the token activates.
pub fn press_kept(minted_at: (u64, u64), now: (u64, u64), on_target: bool) -> bool {
    minted_at == now || on_target
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

    const CARDS: u8 = 5;

    #[test]
    fn ac1_a_card_press_mints_a_token_only_for_navi_bar() {
        let armed = Some(SHOWN_FOR);
        let press = Some((41, CARDS, armed));
        assert!(may_issue_press(
            true,
            false,
            Some(41),
            Some(&CARDS),
            press.as_ref()
        ));
        assert!(
            !may_issue_press(false, false, Some(41), Some(&CARDS), press.as_ref()),
            "not navi-bar"
        );
        assert!(
            !may_issue_press(true, false, Some(40), Some(&CARDS), press.as_ref()),
            "an older press"
        );
        assert!(
            !may_issue_press(true, false, Some(4000), Some(&CARDS), press.as_ref()),
            "made up"
        );
        assert!(
            !may_issue_press(true, false, None, Some(&CARDS), press.as_ref()),
            "no serial"
        );
        let elsewhere = Some((41, KONSOLE, armed));
        assert!(
            !may_issue_press(true, false, Some(41), Some(&CARDS), elsewhere.as_ref()),
            "the press went to another surface"
        );
        assert!(
            !may_issue_press(true, false, Some(41), None, press.as_ref()),
            "no surface"
        );
        assert!(
            !may_issue_press(true, false, Some(41), Some(&CARDS), None),
            "no press"
        );
        assert!(
            !may_issue_press(true, true, Some(41), Some(&CARDS), press.as_ref()),
            "locked"
        );
    }

    /// Round 3 (audit P2): cards hidden by a fullscreen window, or the lock, are new on screen
    /// when it ends; a click in their first 500 ms there mints nothing, however old the card.
    #[test]
    fn a_card_revealed_after_fullscreen_mints_nothing_for_500_ms() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut since = Vec::new();
        // Shown long ago, then a fullscreen window hides the Top layer for a while.
        track_shown(&mut since, &CARDS, layer_shown(true, false, false), t0);
        track_shown(
            &mut since,
            &CARDS,
            layer_shown(true, true, false),
            t0 + ms(2000),
        );
        assert_eq!(shown_for(&since, &CARDS, t0 + ms(5000)), None, "hidden");
        // Fullscreen ends at 5 s: the clock restarts.
        let shown = t0 + ms(5000);
        track_shown(&mut since, &CARDS, layer_shown(true, false, false), shown);
        let press = |at| Some((41, CARDS, shown_for(&since, &CARDS, at)));
        let mint = |at| may_issue_press(true, false, Some(41), Some(&CARDS), press(at).as_ref());
        assert!(!mint(shown), "revealed under the click");
        assert!(!mint(shown + ms(499)));
        assert!(mint(shown + ms(500)));
        // Frames while shown keep the first time.
        track_shown(&mut since, &CARDS, true, shown + ms(300));
        assert_eq!(shown_for(&since, &CARDS, shown + ms(500)), Some(ms(500)));
        // The lock hides every layer; fullscreen leaves layers other than Top alone.
        assert!(!layer_shown(false, false, true));
        assert!(layer_shown(false, true, false));
        // A press on a surface never seen shown mints nothing.
        assert!(!may_issue_press(
            true,
            false,
            Some(41),
            Some(&CARDS),
            Some((41, CARDS, None)).as_ref()
        ));
    }

    #[test]
    fn ac2_a_press_token_focuses_once_fresh_and_while_the_keyboard_stays() {
        let s = Duration::from_secs;
        // navi-bar's token raises as the user (`user_raise` with `bar`).
        let redeem = |first, age, kept| {
            may_redeem(first, age, kept).then(|| on_redeem(user_raise(true, false, 7, 7)))
        };
        let fresh = press_kept((7, 3), (7, 3), false);
        assert_eq!(redeem(true, s(1), fresh), Some(Redeem::Focus));
        assert_eq!(redeem(true, s(10), fresh), Some(Redeem::Focus));
        assert_eq!(redeem(false, s(1), fresh), None, "second use");
        assert_eq!(
            redeem(true, Duration::from_millis(10_001), fresh),
            None,
            "after TTL"
        );
        assert_eq!(
            redeem(true, s(1), press_kept((7, 3), (8, 3), false)),
            None,
            "the keyboard moved to another window"
        );
        assert_eq!(
            redeem(true, s(1), press_kept((7, 3), (7, 4), false)),
            None,
            "the user clicked again, in the window that holds the keyboard"
        );
        assert!(
            press_kept((7, 3), (8, 4), true),
            "the keyboard is already on the target"
        );
    }
}
