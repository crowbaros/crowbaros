//! An ordinary Wayland client used to look at the compositor from outside.
//!
//! It uses `wayland-client` only. The tests and the `crowbar-probe` binary
//! both drive it, so what a test asserts is what a real client sees.

use std::{
    io,
    os::{
        fd::{AsFd, OwnedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
};

use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop,
    protocol::{
        wl_buffer::WlBuffer,
        wl_compositor::WlCompositor,
        wl_keyboard::{self, WlKeyboard},
        wl_registry::{self, WlRegistry},
        wl_seat::{self, WlSeat},
        wl_shm::{self, WlShm},
        wl_shm_pool::WlShmPool,
        wl_surface::{self, WlSurface},
    },
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
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

/// Connects to a compositor socket by path, as any program could.
pub fn connect(path: &Path) -> io::Result<Connection> {
    from_stream(UnixStream::connect(path)?)
}

/// Connects the way any Wayland client does: `WAYLAND_SOCKET` if it is set,
/// otherwise `WAYLAND_DISPLAY`.
pub fn connect_env() -> io::Result<Connection> {
    Connection::connect_to_env().map_err(io::Error::other)
}

/// Wraps an already connected socket, as the shell receives one.
pub fn from_stream(stream: UnixStream) -> io::Result<Connection> {
    Connection::from_socket(stream).map_err(io::Error::other)
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
    /// Windows of other clients, where the compositor offers
    /// ext-foreign-toplevel-list. CrowbarOS does not; a host desktop may.
    pub foreign_toplevels: Vec<ForeignToplevel>,
}

#[derive(Clone, Debug, Default)]
pub struct ForeignToplevel {
    handle: Option<ExtForeignToplevelHandleV1>,
    pub identifier: String,
    pub app_id: String,
    pub title: String,
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
}

/// A listening socket registered through security-context. Clients that
/// connect to `path` are security-context clients for as long as this value lives.
pub struct SecurityContextListener {
    pub path: PathBuf,
    _keep_open: OwnedFd,
}

impl Client {
    /// Binds the advertised global with this interface, if there is one.
    pub fn bind<I>(&self, version: u32) -> Option<I>
    where
        I: Proxy + 'static,
        ClientState: Dispatch<I, ()>,
    {
        let global = self.state.globals.iter().find(|g| g.interface == I::interface().name)?;
        Some(self.bind_by_name(global.name, version.min(global.version)))
    }

    /// Binds a global by its number without checking that it was advertised.
    pub fn bind_by_name<I>(&self, name: u32, version: u32) -> I
    where
        I: Proxy + 'static,
        ClientState: Dispatch<I, ()>,
    {
        self.registry.bind::<I, _, _>(name, version, &self.queue.handle(), ())
    }

    /// Registers a new listening socket at `path` as a security context, the
    /// way a container engine does before it starts a program.
    pub fn security_context(&mut self, path: &Path) -> Result<SecurityContextListener, Error> {
        let manager: WpSecurityContextManagerV1 = self
            .bind(1)
            .ok_or("wp_security_context_manager_v1 is not offered to this client")?;
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        let (close_ours, close_theirs) = rustix::pipe::pipe()?;
        let context = manager.create_listener(listener.as_fd(), close_theirs.as_fd(), &self.queue.handle(), ());
        context.set_sandbox_engine("org.crowbaros.probe".into());
        context.set_app_id("probe".into());
        context.set_instance_id(std::process::id().to_string());
        context.commit();
        self.roundtrip()?;
        Ok(SecurityContextListener {
            path: path.to_path_buf(),
            _keep_open: close_ours,
        })
    }
}

impl Client {
    fn required<I>(&self, version: u32) -> Result<I, Error>
    where
        I: Proxy + 'static,
        ClientState: Dispatch<I, ()>,
    {
        self.bind(version)
            .ok_or_else(|| format!("{} is not offered to this client", I::interface().name).into())
    }

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

    /// Asks the seat for a second keyboard object, as a client may at any time.
    pub fn new_keyboard(&mut self) -> Result<(), Error> {
        self.state.keyboard = None;
        self.listen_to_keyboard()
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
        let size = match self.state.toplevel_size {
            (0, _) | (_, 0) => size,
            given => given,
        };
        surface.attach(Some(&self.buffer(size, argb)?), 0, 0);
        surface.damage_buffer(0, 0, size.0, size.1);
        surface.commit();
        self.roundtrip()?;
        Ok(Toplevel {
            surface,
            xdg_surface,
            toplevel,
        })
    }

    /// Redraws a window at the size of the latest configure.
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

impl Client {
    /// Lists the windows the compositor is willing to tell this client about.
    pub fn foreign_toplevels(&mut self) -> Result<Vec<ForeignToplevel>, Error> {
        let _list: ExtForeignToplevelListV1 = self.required(1)?;
        self.roundtrip()?;
        self.roundtrip()?;
        Ok(self.state.foreign_toplevels.clone())
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for ClientState {
    fn event(
        state: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            state.foreign_toplevels.push(ForeignToplevel {
                handle: Some(toplevel),
                ..Default::default()
            });
        }
    }

    wayland_client::event_created_child!(ClientState, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for ClientState {
    fn event(
        state: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(entry) = state
            .foreign_toplevels
            .iter_mut()
            .find(|t| t.handle.as_ref() == Some(handle))
        else {
            return;
        };
        match event {
            ext_foreign_toplevel_handle_v1::Event::Identifier { identifier } => entry.identifier = identifier,
            ext_foreign_toplevel_handle_v1::Event::Title { title } => entry.title = title,
            ext_foreign_toplevel_handle_v1::Event::AppId { app_id } => entry.app_id = app_id,
            _ => {}
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
