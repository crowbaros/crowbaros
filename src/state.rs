//! The compositor: its event loop, its clients, and how each request reaches
//! the code that carries it out.

use std::{borrow::Cow, collections::HashMap, io, os::fd::AsFd, sync::Weak, time::Duration};

use fulcrum_wire::{
    ArgReader, ConnectionError, DecodeError, Incoming,
    protocol::{
        cursor_shape_v1::{wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1},
        fractional_scale_v1::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1},
        idle_inhibit_unstable_v1::{zwp_idle_inhibit_manager_v1, zwp_idle_inhibitor_v1},
        linux_dmabuf_v1::{zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1, zwp_linux_dmabuf_v1},
        linux_drm_syncobj_v1::{
            wp_linux_drm_syncobj_manager_v1, wp_linux_drm_syncobj_surface_v1, wp_linux_drm_syncobj_timeline_v1,
        },
        presentation_time::{wp_presentation, wp_presentation_feedback},
        primary_selection_unstable_v1::{
            zwp_primary_selection_device_manager_v1, zwp_primary_selection_device_v1, zwp_primary_selection_offer_v1,
            zwp_primary_selection_source_v1,
        },
        security_context_v1::{wp_security_context_manager_v1, wp_security_context_v1},
        viewporter::{wp_viewport, wp_viewporter},
        wayland::{
            wl_buffer, wl_callback, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer,
            wl_data_source, wl_display, wl_keyboard, wl_output, wl_pointer, wl_region, wl_registry, wl_seat, wl_shm,
            wl_shm_pool, wl_subcompositor, wl_subsurface, wl_surface,
        },
        wlr_layer_shell_unstable_v1::{zwlr_layer_shell_v1, zwlr_layer_surface_v1},
        xdg_shell::{xdg_popup, xdg_positioner, xdg_surface, xdg_toplevel, xdg_wm_base},
    },
};

use crate::{
    backend::Backend,
    client::{Client, ClientId, ObjectKind, ProtocolError},
    connections::{CompartmentListener, TrustedListener},
    event_loop::{EventLoop, Readiness, Remote, TimerKey, Token},
    geometry::Region,
    globals::{GlobalKind, Globals},
    identity::Compartment,
    layer::LayerSurface,
    log,
    output::Output,
    shm::{Buffer, BufferId, Pool, PoolId},
    surface::{RegionId, Surface, SurfaceId},
    windows::Window,
    xdg::{Positioner, PositionerId, XdgSurface},
};

/// How many times one client's socket is read in one turn of the loop,
/// before other work gets its turn.
const READS_PER_TURN: usize = 8;

/// The compositor.
pub struct State {
    pub(crate) event_loop: EventLoop,
    remote: Remote<State>,
    running: bool,
    pub(crate) clients: HashMap<ClientId, Client>,
    pub(crate) next_client: ClientId,
    pub(crate) globals: Globals,
    pub(crate) trusted: Vec<TrustedListener>,
    pub(crate) listeners: HashMap<u64, CompartmentListener>,
    pub(crate) next_listener: u64,
    /// The compartments whose listeners the Warden registered. An entry is
    /// dead once its listener is closed and its last client is gone.
    pub(crate) compartment_ids: Vec<Weak<Compartment>>,
    pub(crate) outputs: Vec<Output>,
    pub(crate) next_output: u64,
    pub(crate) planted_binds: usize,
    /// A callback's serial: the event loop's turns, as libwayland does.
    serial: u32,
    pub(crate) surfaces: HashMap<SurfaceId, Surface>,
    pub(crate) regions: HashMap<RegionId, Region>,
    pub(crate) pools: HashMap<PoolId, Pool>,
    pub(crate) buffers: HashMap<BufferId, Buffer>,
    /// The number given to the last surface, region, pool, or buffer.
    pub(crate) next_resource: u64,
    /// The descriptors this process may hold.
    pub(crate) descriptor_limit: usize,
    /// Bytes of pixels one client's surfaces may hold.
    pub(crate) pixel_budget: usize,
    /// How many times a listener could not accept.
    pub(crate) failed_accepts: u64,
    pub(crate) xdg: HashMap<SurfaceId, XdgSurface>,
    pub(crate) positioners: HashMap<PositionerId, Positioner>,
    /// Mapped windows, bottom to top.
    pub(crate) windows: Vec<Window>,
    /// Popups, in the order they were made.
    pub(crate) popups: Vec<SurfaceId>,
    pub(crate) serial_counter: u32,
    /// Where the pointer is on the desktop.
    pub(crate) pointer: (f64, f64),
    pub(crate) backend: Option<Box<dyn Backend>>,
    /// Outputs whose draw is queued for the end of this turn.
    pub(crate) draws_due: Vec<u64>,
    pub(crate) frames_drawn: u64,
    pub(crate) draw_passes: u64,
    pub(crate) start_time: std::time::Instant,
    /// The shell's layer surfaces, in the order they were made.
    pub(crate) layers: Vec<LayerSurface>,
    /// Each output's usable area, after its layers reserved their room.
    pub(crate) usable: HashMap<u64, crate::geometry::Rect>,
    pub(crate) seat: crate::seat::Seat,
    /// The workspace showing.
    pub(crate) workspace: u32,
    pub(crate) controls: crate::control::Controls,
    pub(crate) explicit_sync: crate::explicit_sync::ExplicitSync,
    /// Each monitor's mode, place, and scale, as the person chose them.
    pub(crate) layout: crate::layout::Layout,
    pub(crate) primary: crate::primary_selection::PrimarySelections,
    /// When the shell last said the person launched an app, until a window
    /// takes the keyboard for it.
    pub(crate) launching: Option<std::time::Instant>,
    pub(crate) idle_inhibitors: crate::idle_inhibit::IdleInhibitors,
    /// The system's cursor images, for named shapes and the compositor's
    /// own pointer.
    pub(crate) cursor_theme: crate::xcursor::CursorTheme,
    /// What the pointer is doing to a window, if anything.
    pub(crate) grab: Option<crate::grabs::Grab>,
    /// Commits by window and layer, and when they were last reported.
    pub(crate) commit_counts: std::collections::BTreeMap<String, u64>,
    pub(crate) last_report: Option<std::time::Instant>,
    pub(crate) data: crate::data_device::DataDevices,
    pub(crate) dmabufs: crate::dmabuf::Dmabufs,
}

impl State {
    /// Makes the compositor. It accepts no client until [`State::listen`].
    pub fn new() -> io::Result<Self> {
        let event_loop = EventLoop::new()?;
        let remote = Remote::new(&event_loop);
        let mut state = State {
            event_loop,
            remote,
            running: true,
            clients: HashMap::new(),
            next_client: 0,
            globals: Globals::default(),
            trusted: Vec::new(),
            listeners: HashMap::new(),
            next_listener: 0,
            compartment_ids: Vec::new(),
            outputs: Vec::new(),
            next_output: 0,
            planted_binds: 0,
            serial: 0,
            surfaces: HashMap::new(),
            regions: HashMap::new(),
            pools: HashMap::new(),
            buffers: HashMap::new(),
            next_resource: 0,
            descriptor_limit: crate::limits::descriptor_limit(),
            pixel_budget: crate::limits::CLIENT_PIXEL_BYTES,
            failed_accepts: 0,
            xdg: HashMap::new(),
            positioners: HashMap::new(),
            windows: Vec::new(),
            popups: Vec::new(),
            serial_counter: 0,
            pointer: (0.0, 0.0),
            backend: None,
            draws_due: Vec::new(),
            frames_drawn: 0,
            draw_passes: 0,
            start_time: std::time::Instant::now(),
            layers: Vec::new(),
            usable: HashMap::new(),
            seat: crate::seat::Seat::new(),
            workspace: 1,
            controls: crate::control::Controls::default(),
            explicit_sync: crate::explicit_sync::ExplicitSync::default(),
            layout: crate::layout::Layout::default(),
            primary: crate::primary_selection::PrimarySelections::default(),
            launching: None,
            idle_inhibitors: crate::idle_inhibit::IdleInhibitors::default(),
            cursor_theme: crate::xcursor::CursorTheme::none(),
            grab: None,
            commit_counts: std::collections::BTreeMap::new(),
            last_report: None,
            data: crate::data_device::DataDevices::default(),
            dmabufs: crate::dmabuf::Dmabufs::default(),
        };
        state.add_table_globals();
        Ok(state)
    }

    /// The globals of ADR 0020's table that every compositor has. Each is
    /// advertised now so that what a client is offered can be checked. Their
    /// requests are carried out as Milestones 1 and 2 build them; until then
    /// a request on one is a protocol error.
    fn add_table_globals(&mut self) {
        use fulcrum_wire::protocol::{viewporter, wayland, wlr_layer_shell_unstable_v1 as layer, xdg_shell};
        self.add_global(&wayland::wl_compositor::INFO, 6, GlobalKind::Compositor);
        self.add_global(&wayland::wl_subcompositor::INFO, 1, GlobalKind::Subcompositor);
        self.add_global(&wayland::wl_shm::INFO, 2, GlobalKind::Shm);
        self.add_global(
            &xdg_shell::xdg_wm_base::INFO,
            crate::xdg::XDG_WM_BASE_VERSION,
            GlobalKind::XdgWmBase,
        );
        self.add_global(
            &layer::zwlr_layer_shell_v1::INFO,
            crate::layer::LAYER_SHELL_VERSION,
            GlobalKind::LayerShell,
        );
        self.add_global(&wayland::wl_seat::INFO, crate::seat::SEAT_VERSION, GlobalKind::Seat);
        self.add_global(
            &wayland::wl_data_device_manager::INFO,
            crate::data_device::DATA_DEVICE_MANAGER_VERSION,
            GlobalKind::DataDeviceManager,
        );
        self.add_global(
            &fulcrum_wire::protocol::presentation_time::wp_presentation::INFO,
            crate::presentation::PRESENTATION_VERSION,
            GlobalKind::Presentation,
        );
        self.add_global(
            &fulcrum_wire::protocol::fractional_scale_v1::wp_fractional_scale_manager_v1::INFO,
            crate::fractional_scale::FRACTIONAL_SCALE_VERSION,
            GlobalKind::FractionalScaleManager,
        );
        self.add_global(
            &viewporter::wp_viewporter::INFO,
            crate::viewporter::VIEWPORTER_VERSION,
            GlobalKind::Viewporter,
        );
        self.add_global(
            &zwp_primary_selection_device_manager_v1::INFO,
            crate::primary_selection::PRIMARY_SELECTION_VERSION,
            GlobalKind::PrimarySelection,
        );
        self.add_global(
            &zwp_idle_inhibit_manager_v1::INFO,
            crate::idle_inhibit::IDLE_INHIBIT_VERSION,
            GlobalKind::IdleInhibitManager,
        );
        self.add_global(
            &wp_cursor_shape_manager_v1::INFO,
            crate::cursor_shape::CURSOR_SHAPE_VERSION,
            GlobalKind::CursorShapeManager,
        );
    }

    /// Hands work to the compositor from another thread.
    pub fn remote(&self) -> Remote<State> {
        self.remote.clone()
    }

    /// Ends [`State::run`] after the current turn.
    pub fn stop(&mut self) {
        self.running = false;
    }

    /// Runs until [`State::stop`].
    pub fn run(&mut self) -> io::Result<()> {
        while self.running {
            self.turn(None)?;
        }
        Ok(())
    }

    /// One turn of the loop: waits for at most `limit`, or until something
    /// is ready, handles it, and sends what is queued.
    pub fn turn(&mut self, limit: Option<Duration>) -> io::Result<()> {
        let (ready, timers) = self.event_loop.wait(limit)?;
        self.serial = self.serial.wrapping_add(1);
        for (token, readiness) in ready {
            match token {
                Token::TrustedListener(index) => self.accept_trusted(index),
                Token::Client(id) => self.client_ready(id, readiness),
                Token::CompartmentListener(number) => self.accept_compartment(number),
                Token::CompartmentClose(number) => self.close_compartment(number),
                Token::Waker => {
                    for job in self.remote.take() {
                        job(self);
                    }
                }
                Token::ControlListener => self.accept_control(),
                Token::Control(id) => self.control_ready(id, readiness),
                Token::Held(surface) => self.release_held(surface),
                Token::Backend(source) => {
                    if let Some(mut backend) = self.backend.take() {
                        backend.ready(self, source);
                        if self.backend.is_none() {
                            self.backend = Some(backend);
                        }
                    }
                }
            }
        }
        for timer in timers {
            match timer {
                TimerKey::RetryTrusted(index) => self.retry_trusted(index),
                TimerKey::RetryCompartment(number) => self.retry_compartment(number),
                TimerKey::FrameWait(output, draw) => self.wait_over(output, Some(draw)),
            }
        }
        self.run_due_draws();
        // Flushing lets go of clients that are done, and a client that goes
        // can take a window with it: draw that before the loop sleeps.
        self.flush_clients();
        if !self.draws_due.is_empty() {
            self.run_due_draws();
            self.flush_clients();
        }
        Ok(())
    }

    /// How many clients are connected.
    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Watches a descriptor for the backend. When it is readable, the
    /// backend's `ready` is called with `source`.
    pub fn watch(&mut self, fd: std::os::fd::BorrowedFd<'_>, source: u64) -> io::Result<()> {
        self.event_loop.add(fd, Token::Backend(source), false)
    }

    /// Where the pointer is on the desktop.
    pub fn pointer_position(&self) -> (f64, f64) {
        self.pointer
    }

    /// Sets where frames go. Without a backend nothing is drawn.
    pub fn set_backend(&mut self, backend: impl Backend + 'static) {
        self.backend = Some(Box::new(backend));
        self.queue_redraw_all();
    }

    /// Takes a lower descriptor limit than the process has, as a test does
    /// to reach the reserve without filling the process.
    #[doc(hidden)]
    pub fn set_descriptor_limit(&mut self, limit: usize) {
        self.descriptor_limit = limit;
    }

    /// Takes a smaller budget of pixels for each client, as a test does to
    /// reach it with small buffers.
    #[doc(hidden)]
    pub fn set_pixel_budget(&mut self, bytes: usize) {
        self.pixel_budget = bytes;
    }

    /// How many times a listener could not accept, as when the process had
    /// no descriptor left.
    pub fn failed_accepts(&self) -> u64 {
        self.failed_accepts
    }

    fn client_ready(&mut self, id: ClientId, readiness: Readiness) {
        if readiness.readable || readiness.hung_up {
            for _ in 0..READS_PER_TURN {
                let Some(client) = self.clients.get_mut(&id) else {
                    return;
                };
                if client.doomed {
                    break;
                }
                match client.conn.fill() {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(error) => {
                        client.fail(&error);
                        // What arrived before the close is still read.
                    }
                }
                if self.over_descriptor_budget(id)
                    && let Some(client) = self.clients.get_mut(&id)
                {
                    log::warning!("client {id}: its compartment holds too many descriptors; disconnecting it");
                    client.doomed = true;
                    break;
                }
                self.take_messages(id);
                if self.clients.get(&id).is_none_or(|client| client.doomed) {
                    break;
                }
            }
        }
        if readiness.writable
            && let Some(client) = self.clients.get_mut(&id)
            && let Err(error) = client.conn.flush()
        {
            client.fail(&error);
        }
    }

    fn take_messages(&mut self, id: ClientId) {
        loop {
            let Some(client) = self.clients.get_mut(&id) else {
                return;
            };
            if client.doomed {
                return;
            }
            let incoming = match client.conn.next_message() {
                Ok(Some(incoming)) => incoming,
                Ok(None) => return,
                Err(error) => {
                    client.post_error(ProtocolError {
                        object: 1,
                        code: wl_display::error::INVALID_METHOD,
                        message: format!("unreadable message: {error}"),
                    });
                    return;
                }
            };
            if let Err(error) = self.dispatch(id, incoming)
                && let Some(client) = self.clients.get_mut(&id)
            {
                client.post_error(error);
            }
        }
    }

    /// Sends what is queued for every client, watches the sockets that are
    /// full, and lets go of the clients that are done.
    fn flush_clients(&mut self) {
        let mut gone = Vec::new();
        for (id, client) in &mut self.clients {
            let flushed = match client.conn.flush() {
                Ok(flushed) => flushed,
                Err(error) => {
                    client.fail(&error);
                    true
                }
            };
            if client.doomed {
                gone.push(*id);
                continue;
            }
            if flushed == client.watching_writes {
                let watch = !flushed;
                if self
                    .event_loop
                    .set_writable(client.conn.stream().as_fd(), Token::Client(*id), watch)
                    .is_ok()
                {
                    client.watching_writes = watch;
                }
            }
        }
        for id in gone {
            self.disconnect(id);
        }
    }

    fn disconnect(&mut self, id: ClientId) {
        if let Some(client) = self.clients.remove(&id) {
            self.event_loop.remove(client.conn.stream().as_fd());
            for object in client.objects.values() {
                self.forget(&object.kind);
            }
            log::debug!("client {id} disconnected");
        }
    }

    /// Lets go of what an object held in the compositor, when its client is
    /// gone.
    fn forget(&mut self, kind: &ObjectKind) {
        match kind {
            ObjectKind::Surface(surface) => self.destroy_surface(*surface, false),
            ObjectKind::Region(region) => {
                self.regions.remove(region);
            }
            ObjectKind::Pool(pool) => self.forget_pool(*pool),
            ObjectKind::Buffer(buffer) => self.forget_buffer(*buffer),
            ObjectKind::Positioner(positioner) => {
                self.positioners.remove(positioner);
            }
            ObjectKind::XdgSurface(surface) => self.forget_xdg(*surface),
            ObjectKind::Layer(surface) => self.forget_layer(*surface),
            ObjectKind::DataSource(source) => self.forget_source(*source),
            ObjectKind::DataOffer(offer) => self.forget_offer(*offer),
            ObjectKind::PrimarySource(source) => self.forget_primary_source(*source),
            ObjectKind::PrimaryOffer(offer) => self.forget_primary_offer(*offer),
            ObjectKind::BufferParams(params) => {
                self.dmabufs.params.remove(params);
            }
            ObjectKind::SyncobjTimeline(timeline) => {
                self.explicit_sync.timelines.remove(timeline);
            }
            ObjectKind::IdleInhibitor(inhibitor) => self.forget_inhibitor(*inhibitor),
            _ => {}
        }
    }

    /// One request, from its header to the code that carries it out.
    fn dispatch(&mut self, client_id: ClientId, incoming: Incoming) -> Result<(), ProtocolError> {
        let Incoming { header, body } = incoming;
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        let Some(object) = client.objects.get(&header.object) else {
            return Err(ProtocolError {
                object: 1,
                code: wl_display::error::INVALID_OBJECT,
                message: format!("invalid object {}", header.object),
            });
        };
        let invalid_method = |why: String| ProtocolError {
            object: header.object,
            code: wl_display::error::INVALID_METHOD,
            message: why,
        };
        let Some(info) = object.info else {
            return Err(ProtocolError {
                object: header.object,
                code: wl_display::error::IMPLEMENTATION,
                message: format!("{}#{} has no requests here", object.interface, header.object),
            });
        };
        let Some(message) = info.requests.get(usize::from(header.opcode)) else {
            return Err(invalid_method(format!(
                "invalid method {}, object {}#{}",
                header.opcode, info.name, header.object
            )));
        };
        if message.since > object.version {
            return Err(invalid_method(format!(
                "{}#{}.{} needs version {}, the object has {}",
                info.name, header.object, message.name, message.since, object.version
            )));
        }
        let what = || format!("{}#{}.{}", info.name, header.object, message.name);
        // Every object argument names an existing object of the interface
        // the protocol says, as libwayland checks.
        for (arg, named) in message
            .object_args(&body)
            .map_err(|error| invalid_method(format!("invalid arguments for {}: {error}", what())))?
        {
            if named == 0 {
                continue;
            }
            let fits = client
                .objects
                .get(&named)
                .is_some_and(|object| arg.interface.is_none_or(|interface| object.interface == interface));
            if !fits {
                return Err(ProtocolError {
                    object: 1,
                    code: wl_display::error::INVALID_OBJECT,
                    message: format!("invalid object {named} for {}.{}", what(), arg.name),
                });
            }
        }
        let bad_arguments = |error: DecodeError| invalid_method(format!("invalid arguments for {}: {error}", what()));
        let destructor = message.destructor;
        let opcode = header.opcode;
        let id = header.object;
        let mut fds = std::mem::take(client.conn.fds());
        let decoded = {
            let mut args = ArgReader::new(&body, &mut fds);
            match &object.kind {
                ObjectKind::Display => wl_display::Request::decode(opcode, &mut args).map(Request::Display),
                ObjectKind::Registry => wl_registry::Request::decode(opcode, &mut args).map(Request::Registry),
                ObjectKind::Callback => wl_callback::Request::decode(opcode, &mut args).map(Request::Callback),
                ObjectKind::Output(_) => wl_output::Request::decode(opcode, &mut args).map(Request::Output),
                ObjectKind::SecurityContextManager => {
                    wp_security_context_manager_v1::Request::decode(opcode, &mut args)
                        .map(Request::SecurityContextManager)
                }
                ObjectKind::SecurityContext(_) => {
                    wp_security_context_v1::Request::decode(opcode, &mut args).map(Request::SecurityContext)
                }
                ObjectKind::Compositor => wl_compositor::Request::decode(opcode, &mut args).map(Request::Compositor),
                ObjectKind::Surface(surface) => {
                    wl_surface::Request::decode(opcode, &mut args).map(|request| Request::Surface(*surface, request))
                }
                ObjectKind::Region(region) => {
                    wl_region::Request::decode(opcode, &mut args).map(|request| Request::Region(*region, request))
                }
                ObjectKind::Subcompositor => {
                    wl_subcompositor::Request::decode(opcode, &mut args).map(Request::Subcompositor)
                }
                ObjectKind::Subsurface(surface) => wl_subsurface::Request::decode(opcode, &mut args)
                    .map(|request| Request::Subsurface(*surface, request)),
                ObjectKind::Shm => wl_shm::Request::decode(opcode, &mut args).map(Request::Shm),
                ObjectKind::Pool(pool) => {
                    wl_shm_pool::Request::decode(opcode, &mut args).map(|request| Request::Pool(*pool, request))
                }
                ObjectKind::Buffer(buffer) => {
                    wl_buffer::Request::decode(opcode, &mut args).map(|request| Request::Buffer(*buffer, request))
                }
                ObjectKind::XdgWmBase => xdg_wm_base::Request::decode(opcode, &mut args).map(Request::WmBase),
                ObjectKind::Positioner(positioner) => xdg_positioner::Request::decode(opcode, &mut args)
                    .map(|request| Request::Positioner(*positioner, request)),
                ObjectKind::XdgSurface(surface) => xdg_surface::Request::decode(opcode, &mut args)
                    .map(|request| Request::XdgSurface(*surface, request)),
                ObjectKind::Toplevel(surface) => {
                    xdg_toplevel::Request::decode(opcode, &mut args).map(|request| Request::Toplevel(*surface, request))
                }
                ObjectKind::Popup(surface) => {
                    xdg_popup::Request::decode(opcode, &mut args).map(|request| Request::Popup(*surface, request))
                }
                ObjectKind::Seat => wl_seat::Request::decode(opcode, &mut args).map(Request::Seat),
                ObjectKind::Keyboard => wl_keyboard::Request::decode(opcode, &mut args).map(Request::Keyboard),
                ObjectKind::Pointer => wl_pointer::Request::decode(opcode, &mut args).map(Request::Pointer),
                ObjectKind::LayerShell => {
                    zwlr_layer_shell_v1::Request::decode(opcode, &mut args).map(Request::LayerShell)
                }
                ObjectKind::Layer(surface) => zwlr_layer_surface_v1::Request::decode(opcode, &mut args)
                    .map(|request| Request::Layer(*surface, request)),
                ObjectKind::DataDeviceManager => {
                    wl_data_device_manager::Request::decode(opcode, &mut args).map(Request::DataDeviceManager)
                }
                ObjectKind::DataSource(source) => wl_data_source::Request::decode(opcode, &mut args)
                    .map(|request| Request::DataSource(*source, request)),
                ObjectKind::DataOffer(offer) => {
                    wl_data_offer::Request::decode(opcode, &mut args).map(|request| Request::DataOffer(*offer, request))
                }
                ObjectKind::PrimaryManager => {
                    zwp_primary_selection_device_manager_v1::Request::decode(opcode, &mut args)
                        .map(Request::PrimaryManager)
                }
                ObjectKind::PrimaryDevice => {
                    zwp_primary_selection_device_v1::Request::decode(opcode, &mut args).map(Request::PrimaryDevice)
                }
                ObjectKind::PrimarySource(source) => {
                    zwp_primary_selection_source_v1::Request::decode(opcode, &mut args)
                        .map(|request| Request::PrimarySource(*source, request))
                }
                ObjectKind::PrimaryOffer(offer) => zwp_primary_selection_offer_v1::Request::decode(opcode, &mut args)
                    .map(|request| Request::PrimaryOffer(*offer, request)),
                ObjectKind::DataDevice => wl_data_device::Request::decode(opcode, &mut args).map(Request::DataDevice),
                ObjectKind::Viewporter => wp_viewporter::Request::decode(opcode, &mut args).map(Request::Viewporter),
                ObjectKind::Dmabuf => zwp_linux_dmabuf_v1::Request::decode(opcode, &mut args).map(Request::Dmabuf),
                ObjectKind::BufferParams(params) => zwp_linux_buffer_params_v1::Request::decode(opcode, &mut args)
                    .map(|request| Request::BufferParams(*params, request)),
                ObjectKind::ExplicitSync => {
                    wp_linux_drm_syncobj_manager_v1::Request::decode(opcode, &mut args).map(Request::ExplicitSync)
                }
                ObjectKind::SyncobjSurface(surface) => {
                    wp_linux_drm_syncobj_surface_v1::Request::decode(opcode, &mut args)
                        .map(|request| Request::SyncobjSurface(*surface, request))
                }
                ObjectKind::SyncobjTimeline(_) => {
                    wp_linux_drm_syncobj_timeline_v1::Request::decode(opcode, &mut args).map(Request::SyncobjTimeline)
                }
                ObjectKind::Presentation => {
                    wp_presentation::Request::decode(opcode, &mut args).map(Request::Presentation)
                }
                ObjectKind::PresentationFeedback => {
                    wp_presentation_feedback::Request::decode(opcode, &mut args).map(Request::PresentationFeedback)
                }
                ObjectKind::FractionalScaleManager => {
                    wp_fractional_scale_manager_v1::Request::decode(opcode, &mut args)
                        .map(Request::FractionalScaleManager)
                }
                ObjectKind::FractionalScale(surface) => wp_fractional_scale_v1::Request::decode(opcode, &mut args)
                    .map(|request| Request::FractionalScale(*surface, request)),
                ObjectKind::IdleInhibitManager => {
                    zwp_idle_inhibit_manager_v1::Request::decode(opcode, &mut args).map(Request::IdleInhibitManager)
                }
                ObjectKind::IdleInhibitor(inhibitor) => zwp_idle_inhibitor_v1::Request::decode(opcode, &mut args)
                    .map(|request| Request::IdleInhibitor(*inhibitor, request)),
                ObjectKind::CursorShapeManager => {
                    wp_cursor_shape_manager_v1::Request::decode(opcode, &mut args).map(Request::CursorShapeManager)
                }
                ObjectKind::CursorShapeDevice => {
                    wp_cursor_shape_device_v1::Request::decode(opcode, &mut args).map(Request::CursorShapeDevice)
                }
                ObjectKind::DmabufFeedback => {
                    zwp_linux_dmabuf_feedback_v1::Request::decode(opcode, &mut args).map(Request::DmabufFeedback)
                }
                ObjectKind::Viewport(surface) => {
                    wp_viewport::Request::decode(opcode, &mut args).map(|request| Request::Viewport(*surface, request))
                }
                ObjectKind::Unimplemented => {
                    *client.conn.fds() = fds;
                    return Err(ProtocolError {
                        object: id,
                        code: wl_display::error::IMPLEMENTATION,
                        message: format!("{} is not carried out by this build yet", what()),
                    });
                }
            }
        };
        // Descriptors the message did not take stay for the next one.
        *client.conn.fds() = fds;
        let request = decoded.map_err(bad_arguments)?;
        let version = object.version;
        match request {
            Request::Display(request) => self.display_request(client_id, version, request)?,
            Request::Registry(wl_registry::Request::Bind { name, id: new }) => {
                self.bind(client_id, id, name, new)?;
            }
            Request::Callback(request) => match request {},
            Request::Output(wl_output::Request::Release) => {}
            Request::SecurityContextManager(request) => self.security_context_manager_request(client_id, request)?,
            Request::SecurityContext(request) => self.security_context_request(client_id, id, request)?,
            Request::Compositor(request) => self.compositor_request(client_id, version, request)?,
            Request::Surface(surface, request) => self.surface_request(client_id, id, version, surface, request)?,
            Request::Region(region, request) => self.region_request(id, region, request)?,
            Request::Subcompositor(request) => self.subcompositor_request(client_id, id, request)?,
            Request::Subsurface(surface, request) => self.subsurface_request(id, surface, request, client_id)?,
            Request::Shm(request) => self.shm_request(client_id, id, request)?,
            Request::Pool(pool, request) => self.pool_request(client_id, id, pool, request)?,
            Request::Buffer(buffer, wl_buffer::Request::Destroy) => self.forget_buffer(buffer),
            Request::WmBase(request) => self.wm_base_request(client_id, id, version, request)?,
            Request::Positioner(positioner, request) => self.positioner_request(id, positioner, request)?,
            Request::XdgSurface(surface, request) => {
                self.xdg_surface_request(client_id, id, version, surface, request)?;
            }
            Request::Toplevel(surface, request) => self.toplevel_request(client_id, id, surface, request)?,
            Request::Popup(surface, request) => self.popup_request(id, surface, request)?,
            Request::LayerShell(request) => self.layer_shell_request(client_id, id, version, request)?,
            Request::Seat(request) => self.seat_request(client_id, id, version, request)?,
            Request::Keyboard(wl_keyboard::Request::Release) => {}
            Request::Pointer(request) => self.pointer_request(client_id, id, request)?,
            Request::Layer(surface, request) => self.layer_surface_request(id, surface, request)?,
            Request::DataDeviceManager(request) => self.data_device_manager_request(client_id, version, request)?,
            Request::DataSource(source, request) => self.data_source_request(client_id, id, source, request)?,
            Request::DataOffer(offer, request) => self.data_offer_request(id, offer, request)?,
            Request::PrimaryManager(request) => self.primary_manager_request(client_id, version, request)?,
            Request::PrimaryDevice(request) => self.primary_device_request(client_id, request)?,
            Request::PrimarySource(source, request) => self.primary_source_request(client_id, source, request)?,
            Request::PrimaryOffer(offer, request) => self.primary_offer_request(offer, request)?,
            Request::DataDevice(request) => self.data_device_request(client_id, id, request)?,
            Request::Viewporter(request) => self.viewporter_request(client_id, id, request)?,
            Request::Viewport(surface, request) => self.viewport_request(id, surface, request)?,
            Request::Dmabuf(request) => self.dmabuf_request(client_id, id, version, request)?,
            Request::BufferParams(params, request) => self.buffer_params_request(client_id, id, params, request)?,
            Request::DmabufFeedback(zwp_linux_dmabuf_feedback_v1::Request::Destroy) => {}
            Request::Presentation(request) => self.presentation_request(client_id, request)?,
            Request::ExplicitSync(request) => self.explicit_sync_request(client_id, id, request)?,
            Request::SyncobjSurface(surface, request) => {
                self.syncobj_surface_request(client_id, id, surface, request)?
            }
            Request::SyncobjTimeline(wp_linux_drm_syncobj_timeline_v1::Request::Destroy) => {}
            Request::PresentationFeedback(request) => match request {},
            Request::FractionalScaleManager(request) => {
                self.fractional_scale_manager_request(client_id, id, request)?
            }
            Request::FractionalScale(surface, wp_fractional_scale_v1::Request::Destroy) => {
                self.forget_fractional_scale(surface);
            }
            Request::IdleInhibitManager(request) => self.idle_inhibit_manager_request(client_id, request)?,
            Request::IdleInhibitor(inhibitor, zwp_idle_inhibitor_v1::Request::Destroy) => {
                self.forget_inhibitor(inhibitor);
            }
            Request::CursorShapeManager(request) => {
                self.cursor_shape_manager_request(client_id, id, version, request)?
            }
            Request::CursorShapeDevice(request) => self.cursor_shape_device_request(client_id, id, version, request)?,
        }
        if destructor && let Some(client) = self.clients.get_mut(&client_id) {
            client.destroy(id);
        }
        Ok(())
    }

    fn display_request(
        &mut self,
        client_id: ClientId,
        version: u32,
        request: wl_display::Request,
    ) -> Result<(), ProtocolError> {
        let serial = self.serial;
        let client = self.clients.get_mut(&client_id).expect("the client is dispatching");
        match request {
            wl_display::Request::Sync { callback } => {
                client.create(
                    callback,
                    Cow::Borrowed(wl_callback::NAME),
                    Some(&wl_callback::INFO),
                    1,
                    ObjectKind::Callback,
                )?;
                client.send(callback.0, wl_callback::Event::Done { callback_data: serial });
                client.destroy(callback.0);
            }
            wl_display::Request::GetRegistry { registry } => {
                client.create(
                    registry,
                    Cow::Borrowed(wl_registry::NAME),
                    Some(&wl_registry::INFO),
                    version,
                    ObjectKind::Registry,
                )?;
                self.fill_registry(client_id, registry.0);
            }
        }
        Ok(())
    }
}

/// A request, decoded by the interface of the object it was sent to.
enum Request {
    Display(wl_display::Request),
    Registry(wl_registry::Request),
    Callback(wl_callback::Request),
    Output(wl_output::Request),
    SecurityContextManager(wp_security_context_manager_v1::Request),
    SecurityContext(wp_security_context_v1::Request),
    Compositor(wl_compositor::Request),
    Surface(SurfaceId, wl_surface::Request),
    Region(RegionId, wl_region::Request),
    Subcompositor(wl_subcompositor::Request),
    Subsurface(SurfaceId, wl_subsurface::Request),
    Shm(wl_shm::Request),
    Pool(PoolId, wl_shm_pool::Request),
    Buffer(BufferId, wl_buffer::Request),
    WmBase(xdg_wm_base::Request),
    Positioner(PositionerId, xdg_positioner::Request),
    XdgSurface(SurfaceId, xdg_surface::Request),
    Toplevel(SurfaceId, xdg_toplevel::Request),
    Popup(SurfaceId, xdg_popup::Request),
    LayerShell(zwlr_layer_shell_v1::Request),
    Seat(wl_seat::Request),
    Keyboard(wl_keyboard::Request),
    Pointer(wl_pointer::Request),
    Layer(SurfaceId, zwlr_layer_surface_v1::Request),
    DataDeviceManager(wl_data_device_manager::Request),
    DataSource(crate::data_device::SourceId, wl_data_source::Request),
    DataOffer(crate::data_device::OfferId, wl_data_offer::Request),
    PrimaryManager(zwp_primary_selection_device_manager_v1::Request),
    PrimaryDevice(zwp_primary_selection_device_v1::Request),
    PrimarySource(
        crate::primary_selection::PrimarySourceId,
        zwp_primary_selection_source_v1::Request,
    ),
    PrimaryOffer(
        crate::primary_selection::PrimaryOfferId,
        zwp_primary_selection_offer_v1::Request,
    ),
    DataDevice(wl_data_device::Request),
    Viewporter(wp_viewporter::Request),
    Viewport(SurfaceId, wp_viewport::Request),
    Dmabuf(zwp_linux_dmabuf_v1::Request),
    BufferParams(crate::dmabuf::ParamsId, zwp_linux_buffer_params_v1::Request),
    DmabufFeedback(zwp_linux_dmabuf_feedback_v1::Request),
    FractionalScaleManager(wp_fractional_scale_manager_v1::Request),
    FractionalScale(SurfaceId, wp_fractional_scale_v1::Request),
    IdleInhibitManager(zwp_idle_inhibit_manager_v1::Request),
    IdleInhibitor(crate::idle_inhibit::InhibitorId, zwp_idle_inhibitor_v1::Request),
    CursorShapeManager(wp_cursor_shape_manager_v1::Request),
    CursorShapeDevice(wp_cursor_shape_device_v1::Request),
    Presentation(wp_presentation::Request),
    ExplicitSync(wp_linux_drm_syncobj_manager_v1::Request),
    SyncobjSurface(SurfaceId, wp_linux_drm_syncobj_surface_v1::Request),
    SyncobjTimeline(wp_linux_drm_syncobj_timeline_v1::Request),
    PresentationFeedback(wp_presentation_feedback::Request),
}

impl From<ConnectionError> for ProtocolError {
    fn from(error: ConnectionError) -> Self {
        ProtocolError {
            object: 1,
            code: wl_display::error::IMPLEMENTATION,
            message: error.to_string(),
        }
    }
}
