//! Smithay's protocol handlers, and the dispatch that puts every global
//! through the table.

mod layer_shell;
mod xdg_shell;

use smithay::{
    backend::{allocator::dmabuf::Dmabuf, renderer::utils::on_commit_buffer_handler},
    input::{Seat, SeatHandler, SeatState, pointer::CursorImageStatus},
    reexports::wayland_server::{
        self, Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New,
        backend::ClientId,
        protocol::{wl_buffer::WlBuffer, wl_surface::WlSurface},
    },
    wayland::{
        Dispatch2, GlobalDispatch2,
        buffer::BufferHandler,
        compositor::{CompositorClientState, CompositorHandler, CompositorState, is_sync_subsurface},
        dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
        output::OutputHandler,
        pointer_constraints::PointerConstraintsHandler,
        shm::{ShmHandler, ShmState},
    },
};

use crate::{State, identity::ClientState, policy, windows::root_surface};

//
// Dispatch
//
// These two impls are what `smithay::delegate_dispatch2!` expands to, with one
// change: `can_view` asks the table first. They cover every interface, so no
// global reaches a client without passing the table, including a global whose
// Smithay constructor takes no filter.

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
        policy::offered_to(&client, I::interface().name) && data.can_view(&client)
    }
}

//
// Surfaces and buffers
//

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // Every client is added by `connections.rs`, with a `ClientState`.
        &client
            .get_data::<ClientState>()
            .expect("every client has an identity")
            .compositor
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface)
            && let Some(window) = self.window_for(&root_surface(surface))
        {
            window.on_commit();
        }
        self.xdg_commit(surface);
        self.layer_commit(surface);
        self.queue_redraw_for(surface);
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        if self.import_dmabuf(&dmabuf) {
            let _ = notifier.successful::<State>();
        } else {
            notifier.failed();
        }
    }
}

//
// The seat and the outputs
//

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.seat_state
    }

    // A client's cursor image is not drawn yet (issue 78).
    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}
}

// Smithay needs this to deliver pointer events to a surface. The global is
// never created: pointer constraints are offered to nobody.
impl PointerConstraintsHandler for State {}

impl OutputHandler for State {}
