//! Wayland clients for looking at the compositor from outside, on the client
//! side of `fulcrum-wire`, and the stand-in for the Warden. Never part of
//! Base.
//!
//! What the probe sees is what any client sees: a socket, the globals the
//! registry advertises, and the events and errors that come back. Events are
//! read by their protocol description, not by type, so the probe can watch
//! any object it makes.

#![forbid(unsafe_code)]

use std::{
    collections::{HashMap, VecDeque},
    io,
    os::{
        fd::{AsFd, OwnedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use fulcrum_wire::{
    Arg, ArgReader, Connection, ConnectionError, DynamicNewId, Encode, NewId,
    protocol::{
        self,
        security_context_v1::{wp_security_context_manager_v1, wp_security_context_v1},
        wayland::{self, wl_display, wl_registry},
    },
};

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// How long the probe waits for the compositor before it gives up.
pub const PATIENCE: Duration = Duration::from_secs(10);

/// A global as the registry advertised it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Global {
    pub name: u32,
    pub interface: String,
    pub version: u32,
}

/// The compositor's `wl_display.error`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolError {
    pub object: u32,
    pub code: u32,
    pub message: String,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "protocol error {} on object {}: {}",
            self.code, self.object, self.message
        )
    }
}

impl std::error::Error for ProtocolError {}

/// One event, read by its description.
#[derive(Debug)]
pub struct Event {
    pub object: u32,
    pub interface: &'static str,
    pub name: &'static str,
    pub args: Vec<Arg>,
}

/// Connects to a socket by its path.
pub fn connect(path: &Path) -> io::Result<UnixStream> {
    UnixStream::connect(path)
}

/// The socket `WAYLAND_DISPLAY` names: a path, or a name in
/// `XDG_RUNTIME_DIR`.
pub fn display_path() -> Result<PathBuf, Error> {
    let display = std::env::var("WAYLAND_DISPLAY").map_err(|_| "WAYLAND_DISPLAY is not set")?;
    let path = PathBuf::from(&display);
    if path.is_absolute() {
        return Ok(path);
    }
    let runtime = std::env::var("XDG_RUNTIME_DIR").map_err(|_| "XDG_RUNTIME_DIR is not set")?;
    Ok(Path::new(&runtime).join(display))
}

pub struct Client {
    conn: Connection,
    next_id: u32,
    registry: u32,
    /// The interface of each object this end made.
    objects: HashMap<u32, &'static str>,
    pub globals: Vec<Global>,
    pub error: Option<ProtocolError>,
    /// Events on objects other than the display and the registry, oldest
    /// first.
    pub events: VecDeque<Event>,
    /// The callbacks that have come back.
    done: Vec<u32>,
    closed: bool,
    /// Globals this end bound once for its helpers.
    bound: HashMap<&'static str, u32>,
    /// Frame callbacks asked for and not yet answered.
    frame_callbacks: std::collections::HashSet<u32>,
    /// How many frame callbacks the compositor has answered.
    pub frames_done: u32,
    /// The `xdg_surface`s that have been configured.
    configured: std::collections::HashSet<u32>,
    /// The size each toplevel's last configure gave, where zero means the
    /// client chooses.
    toplevel_size: HashMap<u32, (i32, i32)>,
    /// The size each layer surface's last configure gave.
    pub layer_sizes: HashMap<u32, (u32, u32)>,
}

impl Client {
    /// Gets the registry and waits for the globals. Fails when the
    /// compositor closes the connection instead of answering, which is how
    /// it refuses a peer.
    pub fn new(stream: UnixStream) -> Result<Self, Error> {
        let mut client = Client {
            conn: Connection::new(stream)?,
            next_id: 2,
            registry: 0,
            objects: HashMap::from([(1, wl_display::NAME)]),
            globals: Vec::new(),
            error: None,
            events: VecDeque::new(),
            done: Vec::new(),
            closed: false,
            bound: HashMap::new(),
            frame_callbacks: Default::default(),
            frames_done: 0,
            configured: Default::default(),
            toplevel_size: HashMap::new(),
            layer_sizes: HashMap::new(),
        };
        let registry = client.new_id(wl_registry::NAME);
        client.registry = registry.0;
        client.request(1, wl_display::Request::GetRegistry { registry })?;
        client.roundtrip()?;
        Ok(client)
    }

    pub fn connect(path: &Path) -> Result<Self, Error> {
        Self::new(connect(path)?)
    }

    /// The next free ID, for an object of this interface.
    pub fn new_id(&mut self, interface: &'static str) -> NewId {
        let id = self.next_id;
        self.next_id += 1;
        self.objects.insert(id, interface);
        NewId(id)
    }

    /// Sends a request on an object.
    pub fn request(&mut self, object: u32, request: impl Encode) -> Result<(), Error> {
        let (bytes, fds) = request.to_message(object)?;
        self.send_raw(bytes, fds)
    }

    /// Sends bytes as they are, for the tests of what the compositor refuses.
    pub fn send_raw(&mut self, bytes: Vec<u8>, fds: Vec<OwnedFd>) -> Result<(), Error> {
        self.conn.queue(bytes, fds)?;
        let deadline = Instant::now() + PATIENCE;
        loop {
            match self.conn.flush() {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(ConnectionError::Io(error))
                    if matches!(error.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset) =>
                {
                    // The compositor closed the connection, most likely after
                    // an error it wrote first. Read what it sent.
                    self.read_until_closed();
                    return Err(match &self.error {
                        Some(error) => error.clone().into(),
                        None => "the compositor closed the connection".into(),
                    });
                }
                Err(error) => return Err(error.into()),
            }
            self.wait_readable(deadline)?;
            if Instant::now() > deadline {
                return Err("the compositor stopped reading".into());
            }
        }
    }

    fn read_until_closed(&mut self) {
        let deadline = Instant::now() + PATIENCE;
        while !self.closed && Instant::now() < deadline {
            match self.read() {
                Ok(true) => {}
                Ok(false) => {
                    if self.wait_readable(deadline).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    }

    fn wait_readable(&mut self, deadline: Instant) -> Result<(), Error> {
        let left = deadline.saturating_duration_since(Instant::now());
        let mut fds = [rustix::event::PollFd::new(
            self.conn.stream(),
            rustix::event::PollFlags::IN,
        )];
        let timeout = rustix::event::Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: i64::from(left.subsec_nanos()) as _,
        };
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(_) | Err(rustix::io::Errno::INTR) => Ok(()),
            Err(errno) => Err(errno.into()),
        }
    }

    /// Reads and handles what has arrived. Returns whether anything did.
    fn read(&mut self) -> Result<bool, Error> {
        let read = match self.conn.fill() {
            Ok(read) => read,
            Err(ConnectionError::Closed) => {
                self.closed = true;
                0
            }
            Err(error) => return Err(error.into()),
        };
        while let Some(incoming) = self.conn.next_message()? {
            let object = incoming.header.object;
            let Some(interface) = self.objects.get(&object).copied() else {
                // An event for an object this end already let go of.
                continue;
            };
            let info = protocol::interface(interface).ok_or("an object of an unknown interface")?;
            let message = info
                .events
                .get(usize::from(incoming.header.opcode))
                .ok_or("an event with an unknown opcode")?;
            let mut fds = std::mem::take(self.conn.fds());
            let args = message.decode(&mut ArgReader::new(&incoming.body, &mut fds))?;
            *self.conn.fds() = fds;
            self.handle(Event {
                object,
                interface: info.name,
                name: message.name,
                args,
            });
        }
        Ok(read > 0)
    }

    fn handle(&mut self, event: Event) {
        match (event.interface, event.name, event.args.as_slice()) {
            ("wl_display", "error", [Arg::Object(object), Arg::Uint(code), Arg::String(message)]) => {
                self.error = Some(ProtocolError {
                    object: object.map_or(0, |object| object.0),
                    code: *code,
                    message: message.clone().unwrap_or_default(),
                });
            }
            ("wl_display", "delete_id", [Arg::Uint(id)]) => {
                self.objects.remove(id);
            }
            ("wl_registry", "global", [Arg::Uint(name), Arg::String(Some(interface)), Arg::Uint(version)])
                if event.object == self.registry =>
            {
                self.globals.push(Global {
                    name: *name,
                    interface: interface.clone(),
                    version: *version,
                });
            }
            ("wl_registry", "global_remove", [Arg::Uint(name)]) if event.object == self.registry => {
                self.globals.retain(|global| global.name != *name);
            }
            ("wl_callback", "done", _) => {
                if self.frame_callbacks.remove(&event.object) {
                    self.frames_done += 1;
                }
                self.done.push(event.object);
            }
            // A client acknowledges every configure, as a real one does
            // before it draws.
            ("xdg_surface", "configure", [Arg::Uint(serial)]) => {
                let (object, serial) = (event.object, *serial);
                let _ = self.request(
                    object,
                    fulcrum_wire::protocol::xdg_shell::xdg_surface::Request::AckConfigure { serial },
                );
                self.configured.insert(object);
                self.events.push_back(event);
            }
            ("zwlr_layer_surface_v1", "configure", [Arg::Uint(serial), Arg::Uint(width), Arg::Uint(height)]) => {
                let (object, serial) = (event.object, *serial);
                self.layer_sizes.insert(object, (*width, *height));
                let _ = self.request(
                    object,
                    fulcrum_wire::protocol::wlr_layer_shell_unstable_v1::zwlr_layer_surface_v1::Request::AckConfigure {
                        serial,
                    },
                );
                self.events.push_back(event);
            }
            ("xdg_toplevel", "configure", [Arg::Int(width), Arg::Int(height), _]) => {
                self.toplevel_size.insert(event.object, (*width, *height));
                self.events.push_back(event);
            }
            _ => self.events.push_back(event),
        }
    }

    /// Reads until `done` holds, the compositor sends an error, or the
    /// connection closes.
    pub fn wait_until(&mut self, mut done: impl FnMut(&Self) -> bool) -> Result<(), Error> {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if done(self) {
                return Ok(());
            }
            if let Some(error) = &self.error {
                return Err(error.clone().into());
            }
            if self.closed {
                return Err("the compositor closed the connection".into());
            }
            if Instant::now() > deadline {
                return Err("the compositor did not answer".into());
            }
            if !self.read()? {
                self.wait_readable(deadline)?;
            }
        }
    }

    /// Waits until the compositor has handled every request sent so far.
    pub fn roundtrip(&mut self) -> Result<(), Error> {
        let callback = self.new_id("wl_callback");
        self.request(1, wl_display::Request::Sync { callback })?;
        self.wait_until(|client| client.done.contains(&callback.0))
    }

    /// Whether the compositor has closed the connection.
    pub fn is_closed(&mut self) -> bool {
        let _ = self.read();
        self.closed
    }

    /// The advertised interface names, sorted.
    pub fn interfaces(&self) -> Vec<String> {
        let mut names: Vec<String> = self.globals.iter().map(|global| global.interface.clone()).collect();
        names.sort();
        names
    }

    /// Binds the first advertised global of an interface.
    pub fn bind(&mut self, interface: &'static str, version: u32) -> Result<u32, Error> {
        let global = self
            .globals
            .iter()
            .find(|global| global.interface == interface)
            .ok_or_else(|| format!("{interface} is not offered to this client"))?
            .clone();
        self.bind_name(global.name, interface, version)
    }

    fn bind_name(&mut self, name: u32, interface: &'static str, version: u32) -> Result<u32, Error> {
        let id = self.new_id(interface);
        self.request(
            self.registry,
            wl_registry::Request::Bind {
                name,
                id: DynamicNewId {
                    interface: interface.to_string(),
                    version,
                    id: id.0,
                },
            },
        )?;
        Ok(id.0)
    }

    /// The highest global number this client was told about.
    pub fn last_global(&self) -> u32 {
        self.globals.iter().map(|global| global.name).max().unwrap_or(0)
    }

    /// The global numbers up to `last` that this client was not told about.
    /// Each is either unused or a global hidden from this client.
    pub fn unadvertised(&self, last: u32) -> Vec<u32> {
        (1..=last)
            .filter(|name| !self.globals.iter().any(|global| global.name == *name))
            .collect()
    }

    /// Asks for a global by its number and an interface name, without having
    /// been offered it. A refusal ends the connection, so this takes the
    /// client.
    pub fn guess(mut self, name: u32, interface: &str, version: u32) -> Result<Guess, Error> {
        // The interface may be one this end does not speak. The ID is
        // recorded as a callback, which has no events to misread.
        let id = self.new_id("wl_callback");
        self.request(
            self.registry,
            wl_registry::Request::Bind {
                name,
                id: DynamicNewId {
                    interface: interface.to_string(),
                    version,
                    id: id.0,
                },
            },
        )?;
        match self.roundtrip() {
            Ok(()) => Ok(Guess::Bound),
            Err(_)
                if self
                    .error
                    .as_ref()
                    .is_some_and(|error| error.code == wl_display::error::INVALID_OBJECT) =>
            {
                Ok(Guess::Refused)
            }
            Err(error) => Err(error),
        }
    }

    /// Registers a listener for one compartment, as the Warden does. With a
    /// principal ID or compartment ID of `None`, that field is never set.
    pub fn register_compartment(
        &mut self,
        path: &Path,
        principal: Option<&str>,
        compartment: Option<&str>,
    ) -> Result<CompartmentListener, Error> {
        let manager = self.bind(wp_security_context_manager_v1::NAME, 1)?;
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        let (close_read, close_write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?;
        let context = self.new_id(wp_security_context_v1::NAME);
        self.request(
            manager,
            wp_security_context_manager_v1::Request::CreateListener {
                id: context,
                listen_fd: listener.as_fd().try_clone_to_owned()?,
                close_fd: close_read,
            },
        )?;
        self.request(
            context.0,
            wp_security_context_v1::Request::SetSandboxEngine {
                name: "org.crowbaros.warden".into(),
            },
        )?;
        if let Some(principal) = principal {
            self.request(
                context.0,
                wp_security_context_v1::Request::SetAppId {
                    app_id: principal.into(),
                },
            )?;
        }
        if let Some(compartment) = compartment {
            self.request(
                context.0,
                wp_security_context_v1::Request::SetInstanceId {
                    instance_id: compartment.into(),
                },
            )?;
        }
        self.request(context.0, wp_security_context_v1::Request::Commit)?;
        self.request(context.0, wp_security_context_v1::Request::Destroy)?;
        self.roundtrip()?;
        // The compositor holds the listener now. With this end's copy gone, a
        // listener the compositor refused, or later lets go of, refuses
        // every connection at once.
        drop(listener);
        Ok(CompartmentListener {
            path: path.to_path_buf(),
            close: Some(close_write),
        })
    }
}

/// A buffer in shared memory that this end made and drew into.
pub struct ShmBuffer {
    pub pool: u32,
    pub buffer: u32,
    /// The file the pool shares. Writing to it changes the buffer.
    pub file: OwnedFd,
    pub size: (i32, i32),
}

impl Client {
    /// Binds `wl_compositor` once and returns it.
    pub fn compositor(&mut self) -> Result<u32, Error> {
        if let Some(id) = self.bound.get(wayland::wl_compositor::NAME) {
            return Ok(*id);
        }
        let id = self.bind(wayland::wl_compositor::NAME, 6)?;
        self.bound.insert(wayland::wl_compositor::NAME, id);
        Ok(id)
    }

    fn shm(&mut self) -> Result<u32, Error> {
        if let Some(id) = self.bound.get(wayland::wl_shm::NAME) {
            return Ok(*id);
        }
        let id = self.bind(wayland::wl_shm::NAME, 1)?;
        self.bound.insert(wayland::wl_shm::NAME, id);
        Ok(id)
    }

    /// A new surface with no role.
    pub fn create_surface(&mut self) -> Result<u32, Error> {
        let compositor = self.compositor()?;
        let id = self.new_id(wayland::wl_surface::NAME);
        self.request(compositor, wayland::wl_compositor::Request::CreateSurface { id })?;
        Ok(id.0)
    }

    /// A buffer of one colour, premultiplied ARGB8888, in a pool of its own.
    pub fn shm_buffer(&mut self, size: (i32, i32), argb: u32) -> Result<ShmBuffer, Error> {
        let shm = self.shm()?;
        let (width, height) = size;
        let bytes: Vec<u8> = std::iter::repeat_n(argb.to_le_bytes(), (width * height) as usize)
            .flatten()
            .collect();
        let file = rustix::fs::memfd_create("crowbar-probe", rustix::fs::MemfdFlags::CLOEXEC)?;
        rustix::fs::ftruncate(&file, bytes.len() as u64)?;
        rustix::io::pwrite(&file, &bytes, 0)?;
        let pool = self.new_id(wayland::wl_shm_pool::NAME);
        self.request(
            shm,
            wayland::wl_shm::Request::CreatePool {
                id: pool,
                fd: file.try_clone()?,
                size: bytes.len() as i32,
            },
        )?;
        let buffer = self.new_id(wayland::wl_buffer::NAME);
        self.request(
            pool.0,
            wayland::wl_shm_pool::Request::CreateBuffer {
                id: buffer,
                offset: 0,
                width,
                height,
                stride: width * 4,
                format: 0,
            },
        )?;
        Ok(ShmBuffer {
            pool: pool.0,
            buffer: buffer.0,
            file,
            size,
        })
    }

    /// Attaches a buffer, damages all of it, and commits.
    pub fn show(&mut self, surface: u32, buffer: Option<&ShmBuffer>) -> Result<(), Error> {
        self.request(
            surface,
            wayland::wl_surface::Request::Attach {
                buffer: buffer.map(|buffer| fulcrum_wire::ObjectId(buffer.buffer)),
                x: 0,
                y: 0,
            },
        )?;
        if let Some(buffer) = buffer {
            self.request(
                surface,
                wayland::wl_surface::Request::Damage {
                    x: 0,
                    y: 0,
                    width: buffer.size.0,
                    height: buffer.size.1,
                },
            )?;
        }
        self.request(surface, wayland::wl_surface::Request::Commit)
    }

    /// Makes `surface` a subsurface of `parent`, and returns the
    /// `wl_subsurface`.
    pub fn subsurface(&mut self, surface: u32, parent: u32) -> Result<u32, Error> {
        let subcompositor = match self.bound.get(wayland::wl_subcompositor::NAME) {
            Some(id) => *id,
            None => {
                let id = self.bind(wayland::wl_subcompositor::NAME, 1)?;
                self.bound.insert(wayland::wl_subcompositor::NAME, id);
                id
            }
        };
        let id = self.new_id(wayland::wl_subsurface::NAME);
        self.request(
            subcompositor,
            wayland::wl_subcompositor::Request::GetSubsurface {
                id,
                surface: fulcrum_wire::ObjectId(surface),
                parent: fulcrum_wire::ObjectId(parent),
            },
        )?;
        Ok(id.0)
    }

    /// Whether an event with this name has arrived on this object, taking it.
    pub fn take_event(&mut self, object: u32, name: &str) -> Option<Event> {
        let at = self
            .events
            .iter()
            .position(|event| event.object == object && event.name == name)?;
        self.events.remove(at)
    }
}

/// A window: its surface, its `xdg_surface`, and its `xdg_toplevel`.
#[derive(Clone, Copy, Debug)]
pub struct Toplevel {
    pub surface: u32,
    pub xdg_surface: u32,
    pub toplevel: u32,
}

impl Client {
    fn wm_base(&mut self) -> Result<u32, Error> {
        use fulcrum_wire::protocol::xdg_shell::xdg_wm_base;
        if let Some(id) = self.bound.get(xdg_wm_base::NAME) {
            return Ok(*id);
        }
        let id = self.bind(xdg_wm_base::NAME, 6)?;
        self.bound.insert(xdg_wm_base::NAME, id);
        Ok(id)
    }

    /// Opens a window, as a client does: role, title, an empty commit, the
    /// configure, then a buffer of one colour.
    pub fn open_toplevel(&mut self, title: &str, app_id: &str, size: (i32, i32), argb: u32) -> Result<Toplevel, Error> {
        use fulcrum_wire::protocol::xdg_shell::{xdg_surface, xdg_toplevel, xdg_wm_base};
        let wm_base = self.wm_base()?;
        let surface = self.create_surface()?;
        let xdg = self.new_id(xdg_surface::NAME);
        self.request(
            wm_base,
            xdg_wm_base::Request::GetXdgSurface {
                id: xdg,
                surface: fulcrum_wire::ObjectId(surface),
            },
        )?;
        let toplevel = self.new_id(xdg_toplevel::NAME);
        self.request(xdg.0, xdg_surface::Request::GetToplevel { id: toplevel })?;
        self.request(toplevel.0, xdg_toplevel::Request::SetTitle { title: title.into() })?;
        self.request(toplevel.0, xdg_toplevel::Request::SetAppId { app_id: app_id.into() })?;
        self.request(surface, wayland::wl_surface::Request::Commit)?;
        self.wait_until(|client| client.configured.contains(&xdg.0))?;
        let window = Toplevel {
            surface,
            xdg_surface: xdg.0,
            toplevel: toplevel.0,
        };
        self.redraw(&window, size, argb)?;
        Ok(window)
    }

    /// Draws a window again, at the size its configure gave or else at
    /// `fallback`, and waits for the compositor to have it.
    pub fn redraw(&mut self, window: &Toplevel, fallback: (i32, i32), argb: u32) -> Result<(), Error> {
        let size = match self.toplevel_size.get(&window.toplevel).copied() {
            Some((0, _)) | Some((_, 0)) | None => fallback,
            Some(given) => given,
        };
        let buffer = self.shm_buffer(size, argb)?;
        self.show(window.surface, Some(&buffer))?;
        self.roundtrip()
    }

    /// Asks for a frame callback and commits with nothing changed.
    pub fn commit_unchanged(&mut self, surface: u32) -> Result<(), Error> {
        let callback = self.new_id("wl_callback");
        self.frame_callbacks.insert(callback.0);
        self.request(surface, wayland::wl_surface::Request::Frame { callback })?;
        self.request(surface, wayland::wl_surface::Request::Commit)
    }

    /// A desynchronized subsurface of one colour on a window, shown.
    pub fn open_subsurface(&mut self, window: &Toplevel, size: (i32, i32), argb: u32) -> Result<u32, Error> {
        let surface = self.create_surface()?;
        let subsurface = self.subsurface(surface, window.surface)?;
        self.request(subsurface, wayland::wl_subsurface::Request::SetDesync)?;
        let buffer = self.shm_buffer(size, argb)?;
        self.show(surface, Some(&buffer))?;
        self.request(window.surface, wayland::wl_surface::Request::Commit)?;
        self.roundtrip()?;
        Ok(surface)
    }

    /// Reads whatever has arrived, waiting at most `timeout` for something.
    pub fn wait_for_events(&mut self, timeout: Duration) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        if !self.read()? {
            self.wait_readable(deadline)?;
            self.read()?;
        }
        if let Some(error) = &self.error {
            return Err(error.clone().into());
        }
        Ok(())
    }
}

/// A keyboard event, as a client hears it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    /// The keyboard came to a surface, with the keys already held.
    Enter {
        keys: Vec<u32>,
    },
    Leave,
    Key {
        key: u32,
        pressed: bool,
    },
}

impl Client {
    /// Gets the seat's keyboard. Its events are read by [`Client::take_keys`].
    pub fn listen_to_keyboard(&mut self) -> Result<u32, Error> {
        let seat = self.bind(wayland::wl_seat::NAME, 7)?;
        let keyboard = self.new_id(wayland::wl_keyboard::NAME);
        self.request(seat, wayland::wl_seat::Request::GetKeyboard { id: keyboard })?;
        self.roundtrip()?;
        Ok(keyboard.0)
    }

    /// Gets the seat's pointer.
    pub fn listen_to_pointer(&mut self) -> Result<u32, Error> {
        let seat = self.bind(wayland::wl_seat::NAME, 7)?;
        let pointer = self.new_id(wayland::wl_pointer::NAME);
        self.request(seat, wayland::wl_seat::Request::GetPointer { id: pointer })?;
        self.roundtrip()?;
        Ok(pointer.0)
    }

    /// Every keyboard event that has arrived, taken.
    pub fn take_keys(&mut self) -> Result<Vec<KeyEvent>, Error> {
        self.roundtrip()?;
        let mut keys = Vec::new();
        let mut rest = VecDeque::new();
        for event in std::mem::take(&mut self.events) {
            if event.interface != "wl_keyboard" {
                rest.push_back(event);
                continue;
            }
            match (event.name, event.args.as_slice()) {
                ("enter", [_, _, Arg::Array(held)]) => keys.push(KeyEvent::Enter {
                    keys: held
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|chunk| u32::from_ne_bytes(*chunk))
                        .collect(),
                }),
                ("leave", _) => keys.push(KeyEvent::Leave),
                ("key", [_, _, Arg::Uint(key), Arg::Uint(state)]) => keys.push(KeyEvent::Key {
                    key: *key,
                    pressed: *state == wayland::wl_keyboard::key_state::PRESSED,
                }),
                _ => {}
            }
        }
        self.events = rest;
        Ok(keys)
    }

    /// Every pointer event's name that has arrived, taken.
    pub fn take_pointer_events(&mut self) -> Result<Vec<Event>, Error> {
        self.roundtrip()?;
        let (pointer, rest): (VecDeque<Event>, VecDeque<Event>) = std::mem::take(&mut self.events)
            .into_iter()
            .partition(|event| event.interface == "wl_pointer");
        self.events = rest;
        Ok(pointer.into_iter().filter(|event| event.name != "frame").collect())
    }
}

/// What to ask for when making a layer surface.
#[derive(Clone, Copy, Debug)]
pub struct LayerRequest<'a> {
    pub namespace: &'a str,
    /// 0 background, 1 bottom, 2 top, 3 overlay.
    pub layer: u32,
    /// The `zwlr_layer_surface_v1.anchor` bits.
    pub anchor: u32,
    /// Zero on an axis stretches between the anchored edges.
    pub size: (u32, u32),
    pub exclusive_zone: i32,
    /// 0 none, 1 exclusive, 2 on demand.
    pub keyboard: u32,
}

/// A mapped layer surface and the size the compositor gave it.
#[derive(Clone, Copy, Debug)]
pub struct LayerWindow {
    pub surface: u32,
    pub layer_surface: u32,
    pub size: (u32, u32),
}

impl Client {
    /// Makes a layer surface, as the shell does: placement, an empty commit,
    /// the configure, then a buffer of the size it was given.
    pub fn open_layer(&mut self, request: LayerRequest<'_>, argb: u32) -> Result<LayerWindow, Error> {
        use fulcrum_wire::protocol::wlr_layer_shell_unstable_v1::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};
        let shell = match self.bound.get(zwlr_layer_shell_v1::NAME) {
            Some(id) => *id,
            None => {
                let id = self.bind(zwlr_layer_shell_v1::NAME, 4)?;
                self.bound.insert(zwlr_layer_shell_v1::NAME, id);
                id
            }
        };
        let surface = self.create_surface()?;
        let layer = self.new_id(zwlr_layer_surface_v1::NAME);
        self.request(
            shell,
            zwlr_layer_shell_v1::Request::GetLayerSurface {
                id: layer,
                surface: fulcrum_wire::ObjectId(surface),
                output: None,
                layer: request.layer,
                namespace: request.namespace.into(),
            },
        )?;
        let (width, height) = request.size;
        self.request(layer.0, zwlr_layer_surface_v1::Request::SetSize { width, height })?;
        self.request(
            layer.0,
            zwlr_layer_surface_v1::Request::SetAnchor { anchor: request.anchor },
        )?;
        self.request(
            layer.0,
            zwlr_layer_surface_v1::Request::SetExclusiveZone {
                zone: request.exclusive_zone,
            },
        )?;
        self.request(
            layer.0,
            zwlr_layer_surface_v1::Request::SetKeyboardInteractivity {
                keyboard_interactivity: request.keyboard,
            },
        )?;
        self.request(surface, wayland::wl_surface::Request::Commit)?;
        self.wait_until(|client| client.layer_sizes.contains_key(&layer.0))?;
        let size = self.layer_sizes[&layer.0];
        let buffer = self.shm_buffer((size.0 as i32, size.1 as i32), argb)?;
        self.show(surface, Some(&buffer))?;
        self.roundtrip()?;
        Ok(LayerWindow {
            surface,
            layer_surface: layer.0,
            size,
        })
    }
}

/// What the compositor did with a bind the client was never offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guess {
    /// The compositor answered with a protocol error and closed the
    /// connection.
    Refused,
    /// The compositor made the object.
    Bound,
}

/// Asks for an interface at every global number up to `last` that a client
/// was not told about. Each guess is a new connection from `connect`,
/// because a refusal ends one. Returns the numbers at which the compositor
/// made the object.
pub fn guess_up_to(connect: &dyn Fn() -> Result<Client, Error>, interface: &str, last: u32) -> Result<Vec<u32>, Error> {
    let mut bound = Vec::new();
    for name in connect()?.unadvertised(last) {
        if connect()?.guess(name, interface, 1)? == Guess::Bound {
            bound.push(name);
        }
    }
    Ok(bound)
}

/// Asks at every number up to `past` beyond the last one the client was told
/// about.
pub fn guess_everywhere(
    connect: &dyn Fn() -> Result<Client, Error>,
    interface: &str,
    past: u32,
) -> Result<Vec<u32>, Error> {
    let last = connect()?.last_global() + past;
    guess_up_to(connect, interface, last)
}

/// A compartment's listening socket, registered with the compositor. Clients
/// that connect to `path` belong to that compartment. Dropping this tells
/// the compositor to stop accepting them.
pub struct CompartmentListener {
    pub path: PathBuf,
    close: Option<OwnedFd>,
}

impl CompartmentListener {
    /// Tells the compositor to stop accepting, and keeps the socket file.
    pub fn close(&mut self) {
        self.close.take();
    }
}

impl Drop for CompartmentListener {
    fn drop(&mut self) {
        self.close.take();
        let _ = std::fs::remove_file(&self.path);
    }
}
