//! Server side of wlr-foreign-toplevel-management v3: the window list and its controls, for
//! navi-bar only (the global is filtered to privileged clients).

use smithay::{
    output::Output,
    reexports::{
        wayland_protocols_wlr::foreign_toplevel::v1::server::{
            zwlr_foreign_toplevel_handle_v1::{self as handle, ZwlrForeignToplevelHandleV1},
            zwlr_foreign_toplevel_manager_v1::{self as manager, ZwlrForeignToplevelManagerV1},
        },
        wayland_server::{
            Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
        },
    },
};
use tracing::info;

use crate::layout::WinId;
use crate::state::{ClientState, Navi};

/// What a handle was last told about its window.
#[derive(Clone, Default, PartialEq)]
pub struct Info {
    pub title: String,
    pub app_id: String,
    pub output: Option<Output>,
    pub states: Vec<handle::State>,
}

struct Sent {
    manager: ZwlrForeignToplevelManagerV1,
    handle: ZwlrForeignToplevelHandleV1,
    info: Info,
}

#[derive(Default)]
pub struct Toplevels {
    managers: Vec<ZwlrForeignToplevelManagerV1>,
    /// Tiled windows in open order, with a handle per manager.
    windows: Vec<(WinId, Vec<Sent>)>,
}

impl Toplevels {
    /// Brings every manager up to date with `now`, the tiled windows in open order: announces
    /// new ones, sends what changed and `done`, and `closed` for the ones gone.
    pub fn refresh(&mut self, dh: &DisplayHandle, now: Vec<(WinId, Info)>) {
        self.managers.retain(|m| m.is_alive());
        self.windows.retain_mut(|(id, sent)| {
            let alive = now.iter().any(|(n, _)| n == id);
            if !alive {
                sent.iter().for_each(|s| s.handle.closed());
            }
            alive
        });
        for (id, info) in now {
            let at = match self.windows.iter().position(|(w, _)| *w == id) {
                Some(at) => at,
                None => {
                    self.windows.push((id, Vec::new()));
                    self.windows.len() - 1
                }
            };
            let sent = &mut self.windows[at].1;
            sent.retain(|s| s.handle.is_alive() && self.managers.contains(&s.manager));
            for m in &self.managers {
                if sent.iter().any(|s| &s.manager == m) {
                    continue;
                }
                let Some(client) = m.client() else { continue };
                let Ok(handle) = client
                    .create_resource::<ZwlrForeignToplevelHandleV1, WinId, Navi>(
                        dh,
                        m.version(),
                        id,
                    )
                else {
                    continue;
                };
                m.toplevel(&handle);
                sent.push(Sent {
                    manager: m.clone(),
                    handle,
                    info: Info::default(),
                });
            }
            for s in sent.iter_mut() {
                update(dh, s, &info);
            }
        }
    }
}

/// Sends `s` what differs from `info`, then `done`.
fn update(dh: &DisplayHandle, s: &mut Sent, info: &Info) {
    if s.info == *info {
        return;
    }
    let h = &s.handle;
    if s.info.title != info.title {
        h.title(info.title.clone());
    }
    if s.info.app_id != info.app_id {
        h.app_id(info.app_id.clone());
    }
    if s.info.output != info.output
        && let Ok(client) = dh.get_client(h.id())
    {
        if let Some(o) = &s.info.output {
            o.client_outputs(&client).for_each(|w| h.output_leave(&w));
        }
        if let Some(o) = &info.output {
            o.client_outputs(&client).for_each(|w| h.output_enter(&w));
        }
    }
    if s.info.states != info.states {
        let fullscreen = h.version() >= 2;
        let states: Vec<u8> = info
            .states
            .iter()
            .filter(|&&st| fullscreen || st != handle::State::Fullscreen)
            .flat_map(|&st| u32::from(st).to_ne_bytes())
            .collect();
        h.state(states);
    }
    h.done();
    s.info = info.clone();
}

/// Only navi's own shell client may see the global.
pub struct Filter;

impl GlobalDispatch<ZwlrForeignToplevelManagerV1, Filter> for Navi {
    fn bind(
        state: &mut Self,
        _: &DisplayHandle,
        _: &Client,
        resource: New<ZwlrForeignToplevelManagerV1>,
        _: &Filter,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let m = data_init.init(resource, ());
        state.toplevels.managers.push(m);
        state.refresh_toplevels();
    }

    fn can_view(client: Client, _: &Filter) -> bool {
        crate::state::privileged(client.get_data::<ClientState>())
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Navi {
    fn request(
        state: &mut Self,
        _: &Client,
        m: &ZwlrForeignToplevelManagerV1,
        request: manager::Request,
        _: &(),
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if let manager::Request::Stop = request {
            state.toplevels.managers.retain(|o| o != m);
            m.finished();
        }
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, WinId> for Navi {
    fn request(
        state: &mut Self,
        _: &Client,
        _: &ZwlrForeignToplevelHandleV1,
        request: handle::Request,
        &id: &WinId,
        _: &DisplayHandle,
        _: &mut DataInit<'_, Self>,
    ) {
        if state.lock.active {
            return;
        }
        match request {
            handle::Request::Activate { .. } => {
                info!(id, by = "bar", "activate");
                state.focus_or_restore(id);
            }
            handle::Request::Close => {
                info!(id, by = "bar", "close");
                state.close_window(id);
            }
            // Maximize, minimize, fullscreen and rectangles are navi's or the app's call.
            _ => {}
        }
    }
}
