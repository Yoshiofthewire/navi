//! Keyboard routing for native surfaces and X11 windows. X11 needs X input focus as well as
//! Wayland events; Smithay's X11Surface implements both parts.

use crate::state::Navi;
use smithay::{
    backend::input::KeyState,
    input::{
        Seat,
        keyboard::{KeyboardTarget, KeysymHandle, ModifiersState},
    },
    reexports::wayland_server::{Resource, protocol::wl_surface::WlSurface},
    utils::{IsAlive, Serial},
    wayland::seat::WaylandFocus,
    xwayland::X11Surface,
};
use std::borrow::Cow;

#[derive(Clone, Debug, PartialEq)]
pub enum Focus {
    Wayland(WlSurface),
    X11(X11Surface),
}

impl Focus {
    pub fn surface(&self) -> Option<WlSurface> {
        self.wl_surface().map(Cow::into_owned)
    }
}
impl IsAlive for Focus {
    fn alive(&self) -> bool {
        match self {
            Self::Wayland(s) => s.is_alive(),
            Self::X11(s) => s.alive(),
        }
    }
}
impl WaylandFocus for Focus {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            Self::Wayland(s) => Some(Cow::Borrowed(s)),
            Self::X11(s) => s.wl_surface().map(Cow::Owned),
        }
    }
}
impl KeyboardTarget<Navi> for Focus {
    fn enter(
        &self,
        seat: &Seat<Navi>,
        data: &mut Navi,
        keys: Vec<KeysymHandle<'_>>,
        serial: Serial,
    ) {
        match self {
            Self::Wayland(s) => KeyboardTarget::enter(s, seat, data, keys, serial),
            Self::X11(s) => KeyboardTarget::enter(s, seat, data, keys, serial),
        }
    }
    fn leave(&self, seat: &Seat<Navi>, data: &mut Navi, serial: Serial) {
        match self {
            Self::Wayland(s) => KeyboardTarget::leave(s, seat, data, serial),
            Self::X11(s) => KeyboardTarget::leave(s, seat, data, serial),
        }
    }
    fn key(
        &self,
        seat: &Seat<Navi>,
        data: &mut Navi,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        match self {
            Self::Wayland(s) => KeyboardTarget::key(s, seat, data, key, state, serial, time),
            Self::X11(s) => KeyboardTarget::key(s, seat, data, key, state, serial, time),
        }
    }
    fn modifiers(&self, seat: &Seat<Navi>, data: &mut Navi, mods: ModifiersState, serial: Serial) {
        match self {
            Self::Wayland(s) => KeyboardTarget::modifiers(s, seat, data, mods, serial),
            Self::X11(s) => KeyboardTarget::modifiers(s, seat, data, mods, serial),
        }
    }
}

/// What a layer's change of keyboard interactivity does to focus.
#[derive(Debug, PartialEq)]
pub enum Handoff<W> {
    /// It asked for `Exclusive`: give it the keyboard.
    Take,
    /// It stopped asking while holding the keyboard: give it back to this window, or nobody.
    Return(Option<W>),
}

/// The layers whose last commit asked for `Exclusive`, each with the window that held the
/// keyboard before. Only a change acts, so a stale commit can't grab the keyboard again.
#[derive(Debug)]
pub struct Exclusive<L, W> {
    held: Vec<(L, Option<W>)>,
}

impl<L, W> Default for Exclusive<L, W> {
    fn default() -> Self {
        Self { held: Vec::new() }
    }
}

impl<L: PartialEq, W> Exclusive<L, W> {
    /// A commit of `layer`. `holds`: it has the keyboard; `focused`: the toplevel that has it;
    /// `shown`: a window is still mapped and visible. Locked, nothing moves.
    pub fn commit(
        &mut self,
        layer: L,
        exclusive: bool,
        locked: bool,
        holds: bool,
        focused: Option<W>,
        shown: impl Fn(&W) -> bool,
    ) -> Option<Handoff<W>> {
        let at = self.held.iter().position(|(l, _)| *l == layer);
        match (at, exclusive) {
            (None, true) => {
                self.held.push((layer, focused.filter(|_| !locked)));
                (!locked).then_some(Handoff::Take)
            }
            (Some(i), false) => {
                let (_, back) = self.held.swap_remove(i);
                (holds && !locked).then(|| Handoff::Return(back.filter(shown)))
            }
            _ => None,
        }
    }

    /// `layer` holds the keyboard but its output now shows a fullscreen window, which hides it:
    /// the window to give the keyboard back to, as on close. The layer still counts as asking,
    /// so its next `Exclusive` commit can't take the keyboard again.
    pub fn hidden(&mut self, layer: &L, shown: impl Fn(&W) -> bool) -> Option<Option<W>> {
        let (_, back) = self.held.iter_mut().find(|(l, _)| l == layer)?;
        Some(back.take().filter(shown))
    }

    pub fn forget(&mut self, layer: &L) {
        self.held.retain(|(l, _)| l != layer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BAR: u8 = 1;
    const SHOWN: fn(&u32) -> bool = |_| true;

    #[test]
    fn exclusive_takes_the_keyboard_and_gives_it_back() {
        let mut x = Exclusive::default();
        assert_eq!(
            x.commit(BAR, true, false, false, Some(7), SHOWN),
            Some(Handoff::Take)
        );
        // Repeated Exclusive commits do nothing.
        assert_eq!(x.commit(BAR, true, false, true, Some(9), SHOWN), None);
        assert_eq!(
            x.commit(BAR, false, false, true, None, SHOWN),
            Some(Handoff::Return(Some(7)))
        );
        assert_eq!(x.commit(BAR, false, false, true, None, SHOWN), None);
    }

    #[test]
    fn a_gone_or_hidden_window_gets_nothing_back() {
        let mut x = Exclusive::default();
        x.commit(BAR, true, false, false, Some(7), SHOWN);
        assert_eq!(
            x.commit(BAR, false, false, true, None, |_| false),
            Some(Handoff::Return(None))
        );
        // Nobody had the keyboard before.
        x.commit(BAR, true, false, false, None, SHOWN);
        assert_eq!(
            x.commit(BAR, false, false, true, None, SHOWN),
            Some(Handoff::Return(None))
        );
    }

    #[test]
    fn focus_moved_elsewhere_changes_nothing() {
        let mut x = Exclusive::default();
        x.commit(BAR, true, false, false, Some(7), SHOWN);
        // The user clicked a window: the layer no longer holds the keyboard.
        assert_eq!(x.commit(BAR, false, false, false, None, SHOWN), None);
        // And the next open starts afresh.
        assert_eq!(
            x.commit(BAR, true, false, false, Some(8), SHOWN),
            Some(Handoff::Take)
        );
        assert_eq!(
            x.commit(BAR, false, false, true, None, SHOWN),
            Some(Handoff::Return(Some(8)))
        );
    }

    #[test]
    fn fullscreen_under_the_open_menu_gives_the_keyboard_back_once() {
        let mut x = Exclusive::default();
        assert_eq!(
            x.hidden(&BAR, SHOWN),
            None,
            "not asking: nothing to give back"
        );
        x.commit(BAR, true, false, false, Some(7), SHOWN);
        assert_eq!(x.hidden(&BAR, SHOWN), Some(Some(7)));
        // The bar's commits before it closes the menu never take the keyboard again.
        assert_eq!(x.commit(BAR, true, false, false, None, SHOWN), None);
        // It closes on the keyboard leave; it no longer holds the keyboard.
        assert_eq!(x.commit(BAR, false, false, false, None, SHOWN), None);
        // A window gone by then gets nothing.
        x.commit(BAR, true, false, false, Some(7), SHOWN);
        assert_eq!(x.hidden(&BAR, |_| false), Some(None));
    }

    #[test]
    fn locked_nothing_moves() {
        let mut x = Exclusive::default();
        assert_eq!(x.commit(BAR, true, true, false, Some(7), SHOWN), None);
        assert_eq!(x.commit(BAR, false, true, true, None, SHOWN), None);
        // Opened before the lock, closed while locked.
        x.commit(BAR, true, false, false, Some(7), SHOWN);
        assert_eq!(x.commit(BAR, false, true, true, None, SHOWN), None);
        // A layer that's gone is forgotten.
        x.commit(BAR, true, false, false, Some(7), SHOWN);
        x.forget(&BAR);
        assert_eq!(
            x.commit(BAR, true, false, false, None, SHOWN),
            Some(Handoff::Take)
        );
    }
}
