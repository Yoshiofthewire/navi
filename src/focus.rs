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
