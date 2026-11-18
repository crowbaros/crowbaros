//! The compositor: its event loop, its clients, and how each request reaches
//! the code that carries it out.

use std::{borrow::Cow, collections::HashMap, io, os::fd::AsFd, sync::Weak, time::Duration};

use fulcrum_wire::{
    ArgReader, ConnectionError, DecodeError, Incoming,
    protocol::{
        security_context_v1::{wp_security_context_manager_v1, wp_security_context_v1},
        wayland::{
            wl_buffer, wl_callback, wl_compositor, wl_display, wl_output, wl_region, wl_registry, wl_shm, wl_shm_pool,
            wl_subcompositor, wl_subsurface, wl_surface,
        },
    },
};

use crate::{
    client::{Client, ClientId, ObjectKind, ProtocolError},
    connections::{CompartmentListener, TrustedListener},
    event_loop::{EventLoop, Readiness, Remote, TimerKey, Token},
    geometry::Region,
    globals::{GlobalKind, Globals},
    identity::Compartment,
    log,
    output::Output,
    shm::{Buffer, BufferId, Pool, PoolId},
    surface::{RegionId, Surface, SurfaceId},
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
        for (info, version) in [
            (&wayland::wl_seat::INFO, 7),
            (&xdg_shell::xdg_wm_base::INFO, 6),
            (&viewporter::wp_viewporter::INFO, 1),
            (&layer::zwlr_layer_shell_v1::INFO, 4),
        ] {
            self.add_global(info, version, GlobalKind::Unimplemented);
        }
    }

    /// Offers linux-dmabuf. The table still decides which clients see it.
    pub fn offer_dmabuf(&mut self) {
        use fulcrum_wire::protocol::linux_dmabuf_v1::zwp_linux_dmabuf_v1;
        self.add_global(&zwp_linux_dmabuf_v1::INFO, 4, GlobalKind::Unimplemented);
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
            }
        }
        for timer in timers {
            match timer {
                TimerKey::RetryTrusted(index) => self.retry_trusted(index),
                TimerKey::RetryCompartment(number) => self.retry_compartment(number),
            }
        }
        self.flush_clients();
        Ok(())
    }

    /// How many clients are connected.
    pub fn client_count(&self) -> usize {
        self.clients.len()
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
