//! Wayland clients for looking at the compositor from outside.
//!
//! The crate uses `wayland-client` only, and nothing from Smithay. The tests
//! and the `crowbar-probe` binary both drive it, so what a test asserts is
//! what a real client sees.
//!
//! One client here is not ordinary: [`Client::register_compartment`] does what
//! the Warden does (ADR 0016). It works only on the Warden's connection, so a
//! test or a check that uses it is a stand-in for the Warden.

use std::{
    io,
    os::{
        fd::{AsFd, OwnedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::Arc,
};

use wayland_client::{
    Connection, Dispatch, DispatchError, EventQueue, Proxy, QueueHandle, WEnum,
    backend::{
        Backend, ObjectData, ObjectId, WaylandError,
        protocol::{Argument, Message},
    },
    delegate_noop,
    protocol::{
        wl_buffer::WlBuffer,
        wl_callback::{self, WlCallback},
        wl_compositor::WlCompositor,
        wl_keyboard::{self, WlKeyboard},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_subcompositor::WlSubcompositor,
        wl_subsurface::WlSubsurface,
        wl_surface::{self, WlSurface},
    },
};
use wayland_protocols::wp::security_context::v1::client::{
    wp_security_context_manager_v1::WpSecurityContextManagerV1, wp_security_context_v1::WpSecurityContextV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};
pub use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

/// One global as the registry advertised it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Global {
    pub name: u32,
    pub interface: String,
    pub version: u32,
}

/// Connects to a socket by its path.
pub fn connect(path: &Path) -> io::Result<Connection> {
    Connection::from_socket(UnixStream::connect(path)?).map_err(io::Error::other)
}

/// Connects the way any Wayland client does: `WAYLAND_SOCKET` if it is set,
/// otherwise `WAYLAND_DISPLAY`, which may be a path.
pub fn connect_env() -> io::Result<Connection> {
    Connection::connect_to_env().map_err(io::Error::other)
}

/// What the client's keyboard was told, in the order it was told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    /// Focus arrived. `held` lists the keys the compositor reported as down.
    Enter {
        held: Vec<u32>,
    },
    Leave,
    /// `key` is the evdev code (`KEY_*` in `linux/input-event-codes.h`).
    Key {
        key: u32,
        pressed: bool,
    },
    Modifiers {
        depressed: u32,
    },
}

#[derive(Default)]
pub struct ClientState {
    pub globals: Vec<Global>,
    pub keys: Vec<KeyEvent>,
    keyboard: Option<WlKeyboard>,
    /// Size from the latest xdg_toplevel.configure. Zero means "you choose".
    pub toplevel_size: (i32, i32),
    xdg_configured: bool,
    /// Size from the latest zwlr_layer_surface_v1.configure.
    pub layer_size: Option<(u32, u32)>,
    pub closed: bool,
    /// Every wl_surface.preferred_buffer_scale the compositor sent.
    pub buffer_scales: Vec<i32>,
    /// How many frame callbacks have come back.
    pub frames_done: u32,
}

/// A mapped xdg-shell window.
pub struct Toplevel {
    pub surface: WlSurface,
    pub xdg_surface: XdgSurface,
    pub toplevel: XdgToplevel,
}

/// What to ask for when creating a layer surface.
pub struct LayerRequest<'a> {
    pub namespace: &'a str,
    pub layer: Layer,
    pub anchor: Anchor,
    /// Zero on an axis means "stretch between the anchored edges".
    pub size: (u32, u32),
    pub exclusive_zone: i32,
    pub keyboard: KeyboardInteractivity,
}

/// A mapped layer surface and the size the compositor gave it.
pub struct LayerWindow {
    pub surface: WlSurface,
    pub layer_surface: ZwlrLayerSurfaceV1,
    pub size: (u32, u32),
}

pub struct Client {
    pub conn: Connection,
    pub queue: EventQueue<ClientState>,
    pub state: ClientState,
    pub registry: WlRegistry,
}

pub type Error = Box<dyn std::error::Error + Send + Sync>;

impl Client {
    /// Fails when the compositor closes the connection instead of answering,
    /// which is how it refuses a peer.
    pub fn new(conn: Connection) -> Result<Self, Error> {
        let queue = conn.new_event_queue::<ClientState>();
        let registry = conn.display().get_registry(&queue.handle(), ());
        let mut client = Client {
            conn,
            queue,
            state: ClientState::default(),
            registry,
        };
        client.roundtrip()?;
        Ok(client)
    }

    pub fn roundtrip(&mut self) -> Result<(), Error> {
        self.queue.roundtrip(&mut self.state)?;
        Ok(())
    }

    /// The advertised interface names, sorted.
    pub fn interfaces(&self) -> Vec<String> {
        let mut names: Vec<String> = self.state.globals.iter().map(|g| g.interface.clone()).collect();
        names.sort();
        names
    }

    /// Binds the advertised global with this interface, if there is one.
    pub fn bind<I>(&self, version: u32) -> Option<I>
    where
        I: Proxy + 'static,
        ClientState: Dispatch<I, ()>,
    {
        let global = self.state.globals.iter().find(|g| g.interface == I::interface().name)?;
        Some(
            self.registry
                .bind::<I, _, _>(global.name, version.min(global.version), &self.queue.handle(), ()),
        )
    }

    fn required<I>(&self, version: u32) -> Result<I, Error>
    where
        I: Proxy + 'static,
        ClientState: Dispatch<I, ()>,
    {
        self.bind(version)
            .ok_or_else(|| format!("{} is not offered to this client", I::interface().name).into())
    }
}

/// What the compositor did with a bind the client was never offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guess {
    /// The compositor answered with a protocol error and closed the connection.
    Refused,
    /// The compositor made the object.
    Bound,
}

impl Client {
    /// Asks for a global by its number and the name of its interface, without
    /// having been offered it. A refusal ends the connection, so this takes
    /// the client.
    pub fn guess(mut self, name: u32, interface: &str, version: u32) -> Result<Guess, Error> {
        /// The object the bind would make. This end has to say what it is, and
        /// calls it a `wl_callback`: the plainest object there is. What goes
        /// to the compositor is the interface name the caller gave.
        struct Unused;
        impl ObjectData for Unused {
            fn event(self: Arc<Self>, _: &Backend, _: Message<ObjectId, OwnedFd>) -> Option<Arc<dyn ObjectData>> {
                None
            }
            fn destroyed(&self, _: ObjectId) {}
        }
        let bind = Message {
            sender_id: self.registry.id(),
            opcode: 0,
            args: [
                Argument::Uint(name),
                Argument::Str(Some(Box::new(std::ffi::CString::new(interface)?))),
                Argument::Uint(version),
                Argument::NewId(ObjectId::null()),
            ]
            .into_iter()
            .collect(),
        };
        self.conn
            .backend()
            .send_request(bind, Some(Arc::new(Unused)), Some((WlCallback::interface(), version)))?;
        match self.queue.roundtrip(&mut self.state) {
            Ok(_) => Ok(Guess::Bound),
            Err(DispatchError::Backend(error)) if is_refusal(&error) => Ok(Guess::Refused),
            Err(other) => Err(other.into()),
        }
    }

    /// The global numbers up to `last` that this client was not told about.
    /// Each is either unused or a global hidden from this client.
    pub fn unadvertised(&self, last: u32) -> Vec<u32> {
        (1..=last)
            .filter(|name| !self.state.globals.iter().any(|g| g.name == *name))
            .collect()
    }
}

/// Asks for an interface at every global number that a client was not told
/// about, up to `past` numbers beyond the last one it was. Each guess is a
/// new connection from `connect`, because a refusal ends one. Returns the
/// numbers at which the compositor made the object.
pub fn guess_everywhere(
    connect: &dyn Fn() -> Result<Client, Error>,
    interface: &str,
    past: u32,
) -> Result<Vec<u32>, Error> {
    let seen = connect()?;
    let last = seen.state.globals.iter().map(|global| global.name).max().unwrap_or(0) + past;
    let mut bound = Vec::new();
    for name in seen.unadvertised(last) {
        if connect()?.guess(name, interface, 1)? == Guess::Bound {
            bound.push(name);
        }
    }
    Ok(bound)
}

/// Whether an error is the compositor's own refusal of a request: the
/// `invalid_object` error on `wl_display`. Anything else is not a refusal,
/// such as an event this client could not read from an object the compositor
/// did make.
fn is_refusal(error: &WaylandError) -> bool {
    match error {
        WaylandError::Protocol(error) => error.object_interface == "wl_display" && error.code == 0,
        // libwayland reports that same error as this errno, and no other
        // error that way.
        WaylandError::Io(error) => error.raw_os_error() == Some(rustix::io::Errno::INVAL.raw_os_error()),
    }
}

/// A compartment's listening socket, registered with the compositor. Clients
/// that connect to `path` belong to that compartment. Dropping this tells the
/// compositor to stop accepting them.
pub struct CompartmentListener {
    pub path: PathBuf,
    _keep_open: OwnedFd,
}

impl Client {
    /// Registers a listening socket at `path` for one compartment, as the
    /// Warden does: the principal ID is the app ID and the compartment ID is
    /// the instance ID (ADR 0016). `None` leaves a field unset, which the
    /// compositor must refuse.
    pub fn register_compartment(
        &mut self,
        path: &Path,
        principal: Option<&str>,
        compartment: Option<&str>,
    ) -> Result<CompartmentListener, Error> {
        let manager: WpSecurityContextManagerV1 = self.required(1)?;
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        let (close_ours, close_theirs) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
        let context = manager.create_listener(listener.as_fd(), close_theirs.as_fd(), &self.queue.handle(), ());
        context.set_sandbox_engine("org.crowbaros.warden".into());
        if let Some(principal) = principal {
            context.set_app_id(principal.into());
        }
        if let Some(compartment) = compartment {
            context.set_instance_id(compartment.into());
        }
        context.commit();
        self.roundtrip()?;
        // The compositor holds the listener now. This end keeps no copy, so a
        // listener the compositor refused accepts nothing.
        drop(listener);
        Ok(CompartmentListener {
            path: path.to_path_buf(),
            _keep_open: close_ours,
        })
    }
}

impl Client {
    /// A solid-colour shm buffer. `argb` is 0xAARRGGBB.
    fn buffer(&self, size: (i32, i32), argb: u32) -> Result<WlBuffer, Error> {
        use std::io::Write;
        let shm: WlShm = self.required(1)?;
        let (w, h) = (size.0.max(1), size.1.max(1));
        let fd = rustix::fs::memfd_create("crowbar-probe", rustix::fs::MemfdFlags::CLOEXEC)?;
        let mut file = std::fs::File::from(fd);
        let row: Vec<u8> = argb.to_le_bytes().repeat(w as usize);
        for _ in 0..h {
            file.write_all(&row)?;
        }
        let qh = self.queue.handle();
        let pool = shm.create_pool(file.as_fd(), w * h * 4, &qh, ());
        let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Argb8888, &qh, ());
        pool.destroy();
        Ok(buffer)
    }

    /// Starts listening to the seat's keyboard.
    pub fn listen_to_keyboard(&mut self) -> Result<(), Error> {
        let _seat: WlSeat = self.required(5)?;
        self.roundtrip()?;
        self.roundtrip()
    }

    /// Maps an xdg-shell window filled with one colour.
    pub fn open_toplevel(&mut self, title: &str, app_id: &str, size: (i32, i32), argb: u32) -> Result<Toplevel, Error> {
        let qh = self.queue.handle();
        let compositor: WlCompositor = self.required(6)?;
        let wm_base: XdgWmBase = self.required(1)?;
        let surface = compositor.create_surface(&qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg_surface.get_toplevel(&qh, ());
        toplevel.set_title(title.into());
        toplevel.set_app_id(app_id.into());
        surface.commit();
        self.state.xdg_configured = false;
        while !self.state.xdg_configured {
            self.queue.blocking_dispatch(&mut self.state)?;
        }
        let window = Toplevel {
            surface,
            xdg_surface,
            toplevel,
        };
        self.redraw(&window, size, argb)?;
        Ok(window)
    }

    /// Draws a window in one colour at the size of the latest configure, or
    /// at `fallback` when the compositor left the size to the client.
    pub fn redraw(&mut self, window: &Toplevel, fallback: (i32, i32), argb: u32) -> Result<(), Error> {
        let size = match self.state.toplevel_size {
            (0, _) | (_, 0) => fallback,
            given => given,
        };
        window.surface.attach(Some(&self.buffer(size, argb)?), 0, 0);
        window.surface.damage_buffer(0, 0, size.0, size.1);
        window.surface.commit();
        self.roundtrip()
    }

    /// Adds a part to a window: a subsurface in one colour, at the window's
    /// top left corner. Destroying the returned surface takes the part away.
    pub fn open_subsurface(&mut self, window: &Toplevel, size: (i32, i32), argb: u32) -> Result<WlSurface, Error> {
        let qh = self.queue.handle();
        let compositor: WlCompositor = self.required(6)?;
        let subcompositor: WlSubcompositor = self.required(1)?;
        let surface = compositor.create_surface(&qh, ());
        let subsurface = subcompositor.get_subsurface(&surface, &window.surface, &qh, ());
        subsurface.set_desync();
        surface.attach(Some(&self.buffer(size, argb)?), 0, 0);
        surface.damage_buffer(0, 0, size.0, size.1);
        surface.commit();
        window.surface.commit();
        self.roundtrip()?;
        Ok(surface)
    }

    /// Asks to be told when the next frame is shown, and commits without
    /// changing what the window shows. [`ClientState::frames_done`] counts
    /// the answers.
    pub fn commit_unchanged(&mut self, window: &Toplevel) -> Result<(), Error> {
        window.surface.frame(&self.queue.handle(), ());
        window.surface.commit();
        self.conn.flush()?;
        Ok(())
    }

    /// Waits for the next message from the compositor and handles it.
    pub fn wait(&mut self) -> Result<(), Error> {
        self.queue.blocking_dispatch(&mut self.state)?;
        Ok(())
    }

    /// Maps a layer surface filled with one colour.
    pub fn open_layer(&mut self, request: LayerRequest<'_>, argb: u32) -> Result<LayerWindow, Error> {
        let qh = self.queue.handle();
        let compositor: WlCompositor = self.required(6)?;
        let layer_shell: ZwlrLayerShellV1 = self.required(4)?;
        let surface = compositor.create_surface(&qh, ());
        let layer_surface =
            layer_shell.get_layer_surface(&surface, None, request.layer, request.namespace.into(), &qh, ());
        layer_surface.set_size(request.size.0, request.size.1);
        layer_surface.set_anchor(request.anchor);
        layer_surface.set_exclusive_zone(request.exclusive_zone);
        layer_surface.set_keyboard_interactivity(request.keyboard);
        surface.commit();
        self.state.layer_size = None;
        let size = loop {
            if let Some(size) = self.state.layer_size {
                break size;
            }
            self.queue.blocking_dispatch(&mut self.state)?;
        };
        surface.attach(Some(&self.buffer((size.0 as i32, size.1 as i32), argb)?), 0, 0);
        surface.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
        surface.commit();
        self.roundtrip()?;
        Ok(LayerWindow {
            surface,
            layer_surface,
            size,
        })
    }

    /// Returns the keyboard events seen since the last call.
    pub fn take_keys(&mut self) -> Result<Vec<KeyEvent>, Error> {
        self.roundtrip()?;
        Ok(std::mem::take(&mut self.state.keys))
    }
}

impl Dispatch<XdgWmBase, ()> for ClientState {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for ClientState {
    fn event(
        state: &mut Self,
        xdg_surface: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            state.xdg_configured = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure { width, height, .. } => state.toplevel_size = (width, height),
            xdg_toplevel::Event::Close => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for ClientState {
    fn event(
        state: &mut Self,
        layer_surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer_surface.ack_configure(serial);
                state.layer_size = Some((width, height));
            }
            zwlr_layer_surface_v1::Event::Closed => state.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for ClientState {
    fn event(state: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
            && capabilities.contains(wl_seat::Capability::Keyboard)
            && state.keyboard.is_none()
        {
            state.keyboard = Some(seat.get_keyboard(qh, ()));
        }
    }
}

impl Dispatch<WlKeyboard, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { keys, .. } => state.keys.push(KeyEvent::Enter {
                held: keys.as_chunks::<4>().0.iter().map(|c| u32::from_ne_bytes(*c)).collect(),
            }),
            wl_keyboard::Event::Leave { .. } => state.keys.push(KeyEvent::Leave),
            wl_keyboard::Event::Key {
                key,
                state: WEnum::Value(key_state),
                ..
            } => state.keys.push(KeyEvent::Key {
                key,
                pressed: key_state == wl_keyboard::KeyState::Pressed,
            }),
            wl_keyboard::Event::Modifiers { mods_depressed, .. } => state.keys.push(KeyEvent::Modifiers {
                depressed: mods_depressed,
            }),
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.frames_done += 1;
        }
    }
}

delegate_noop!(ClientState: WlCompositor);
impl Dispatch<WlSurface, ()> for ClientState {
    fn event(state: &mut Self, _: &WlSurface, event: wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_surface::Event::PreferredBufferScale { factor } = event {
            state.buffer_scales.push(factor);
        }
    }
}
delegate_noop!(ClientState: ignore WlShm);
delegate_noop!(ClientState: WlShmPool);
delegate_noop!(ClientState: WlSubcompositor);
delegate_noop!(ClientState: WlSubsurface);
delegate_noop!(ClientState: ignore WlBuffer);
delegate_noop!(ClientState: ZwlrLayerShellV1);
delegate_noop!(ClientState: WpSecurityContextManagerV1);
delegate_noop!(ClientState: WpSecurityContextV1);

impl Dispatch<WlRegistry, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => state.globals.push(Global {
                name,
                interface,
                version,
            }),
            wl_registry::Event::GlobalRemove { name } => state.globals.retain(|g| g.name != name),
            _ => {}
        }
    }
}
