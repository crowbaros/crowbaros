mod compositor;
pub mod layer_shell;
mod xdg_shell;

use std::sync::Arc;

use crate::{
    State,
    policy::{self, Principal},
    state::ClientState,
};
use smithay::reexports::wayland_server::{
    self, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, backend::ClientId,
};
use smithay::wayland::{Dispatch2, GlobalDispatch2};
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::wayland::compositor::with_states;
use smithay::wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier};
use smithay::wayland::fractional_scale::{FractionalScaleHandler, with_fractional_scale};
use smithay::wayland::security_context::{
    SecurityContext, SecurityContextHandler, SecurityContextListenerSource,
};

//
// Wl Seat
//

use smithay::input::dnd::{DnDGrab, DndGrabHandler, GrabType, Source};
use smithay::input::pointer::Focus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::Resource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::Serial;
use smithay::wayland::output::OutputHandler;
use smithay::wayland::pointer_constraints::PointerConstraintsHandler;
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus,
};

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: smithay::input::pointer::CursorImageStatus) {}

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client);
    }
}

impl PointerConstraintsHandler for State {}

//
// Wl Data Device
//

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl DndGrabHandler for State {}
impl WaylandDndGrabHandler for State {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        match type_ {
            GrabType::Pointer => {
                let ptr = seat.get_pointer().unwrap();
                let start_data = ptr.grab_start_data().unwrap();

                // create a dnd grab to start the operation
                let grab = DnDGrab::new_pointer(&self.display_handle, start_data, source, seat);
                ptr.set_grab(self, grab, serial, Focus::Keep);
            }
            GrabType::Touch => {
                // crowbar-compositor lacks touch handling
                source.cancel();
            }
        }
    }
}

//
// Wl Output & Xdg Output
//

impl OutputHandler for State {}

//
// Fractional scale
//

impl FractionalScaleHandler for State {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // The surface's output is not known yet. Start from the first output;
        // `State::update_surface_scales` corrects it once the surface is placed.
        let scale = self
            .outputs
            .first()
            .map(|o| o.current_scale().fractional_scale())
            .unwrap_or(1.0);
        with_states(&surface, |states| {
            with_fractional_scale(states, |fractional| fractional.set_preferred_scale(scale));
        });
    }
}

//
// Dmabuf
//

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        let imported = self.dmabuf_import.as_mut().is_some_and(|import| import(&dmabuf));
        if imported {
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}

//
// Security context
//

impl SecurityContextHandler for State {
    fn context_created(&mut self, source: SecurityContextListenerSource, _context: SecurityContext) {
        self.loop_handle
            .insert_source(source, move |client_stream, _, state| {
                let _ = state.display_handle.insert_client(
                    client_stream,
                    Arc::new(ClientState::new(Principal::Sandboxed)),
                );
            })
            .expect("security context listener");
    }
}

//
// Dispatch
//
// These two impls are what `smithay::delegate_dispatch2!` expands to, with one
// change: `can_view` asks `policy` first. Every global is therefore filtered
// by the one table in `policy.rs`, including globals whose Smithay
// constructor takes no filter.

impl<I, UserData> Dispatch<I, UserData> for State
where
    I: wayland_server::Resource,
    UserData: Dispatch2<I, State>,
{
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &I,
        request: <I as wayland_server::Resource>::Request,
        data: &UserData,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        data.request(state, client, resource, request, dhandle, data_init);
    }

    fn destroyed(state: &mut Self, client: ClientId, resource: &I, data: &UserData) {
        data.destroyed(state, client, resource);
    }
}

impl<I, UserData> GlobalDispatch<I, UserData> for State
where
    I: wayland_server::Resource,
    UserData: GlobalDispatch2<I, State>,
{
    fn bind(
        state: &mut Self,
        dhandle: &DisplayHandle,
        client: &Client,
        resource: New<I>,
        data: &UserData,
        data_init: &mut DataInit<'_, Self>,
    ) {
        data.bind(state, dhandle, client, resource, data_init);
    }

    fn can_view(client: Client, data: &UserData) -> bool {
        policy::client_may_see(&client, I::interface().name) && data.can_view(&client)
    }
}
