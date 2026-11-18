//! The nested backend: the compositor as a window on a host Wayland session,
//! for development. One output, the size of the window; keys and the pointer
//! from the host's seat.
//!
//! It is built only with `--features nested`. Base never is: a compositor in
//! Base that became a client of another display, or looked for one, would
//! open a way in that ADR 0016 closes.
//!
//! The compositor speaks to the host on the client side of its own wire
//! code, as the probe does. Each frame is drawn by the CPU renderer and
//! written into shared memory the host reads; the host's frame callback ends
//! the frame's wait, as a page flip does on a TTY.

use std::{
    collections::{HashMap, HashSet},
    io,
    os::{
        fd::{AsFd, OwnedFd},
        unix::net::UnixStream,
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use fulcrum_wire::{
    ArgReader, Connection, ConnectionError, DecodeError, Encode, NewId, ObjectId,
    protocol::{
        wayland::{
            wl_buffer, wl_callback, wl_compositor, wl_display, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm,
            wl_shm_pool, wl_surface,
        },
        xdg_shell::{xdg_surface, xdg_toplevel, xdg_wm_base},
    },
};

use crate::{
    State,
    backend::{Backend, Drawn},
    log,
};

const SOURCE_HOST: u64 = 1;

/// The window's size until the host chooses one.
const DEFAULT_SIZE: (i32, i32) = (1280, 800);
/// The refresh the output claims. The host's frame callbacks set the pace.
const REFRESH: i32 = 60_000;
/// How long starting up waits for the host to answer.
const PATIENCE: Duration = Duration::from_secs(10);

const AXIS_VERTICAL: u32 = 0;

/// A buffer in shared memory that the host reads frames from.
struct HostBuffer {
    memory: OwnedFd,
    buffer: u32,
    size: (i32, i32),
    /// The host has it and has not released it.
    busy: bool,
}

pub struct Nested {
    conn: Connection,
    next_id: u32,
    /// The interface of each object this end made, to read its events.
    objects: HashMap<u32, &'static str>,
    globals: Vec<(u32, String, u32)>,
    compositor: u32,
    shm: u32,
    wm_base: u32,
    seat: u32,
    surface: u32,
    xdg_surface: u32,
    toplevel: u32,
    pointer: Option<u32>,
    keyboard: Option<u32>,
    output: u64,
    size: (i32, i32),
    /// The size the host's last toplevel configure asked for.
    asked: Option<(i32, i32)>,
    configured: bool,
    buffers: Vec<HostBuffer>,
    /// The host's frame callback for the frame on its way.
    frame: Option<u32>,
    /// Callbacks that come back once the host has handled every request.
    syncs: HashSet<u32>,
    /// Keys the compositor was told are down, to let go of when the
    /// window loses the keyboard.
    held: HashSet<u32>,
    /// Buttons the compositor was told are down, to let go of when the
    /// pointer leaves the window.
    buttons: HashSet<u32>,
    time: u32,
}

/// The socket of the session the compositor runs inside.
fn host_socket() -> io::Result<PathBuf> {
    let display = std::env::var_os("WAYLAND_DISPLAY")
        .ok_or_else(|| io::Error::other("the nested backend needs WAYLAND_DISPLAY"))?;
    let path = PathBuf::from(display);
    if path.is_absolute() {
        return Ok(path);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| io::Error::other("the nested backend needs XDG_RUNTIME_DIR"))?;
    Ok(Path::new(&runtime).join(path))
}

/// Starts the nested backend in the session `WAYLAND_DISPLAY` names.
pub fn init(state: &mut State) -> io::Result<()> {
    init_at(state, &host_socket()?)
}

/// Starts the nested backend: a window on the host session at `socket`, and
/// one output the size of it.
pub fn init_at(state: &mut State, socket: &Path) -> io::Result<()> {
    let stream = UnixStream::connect(socket)
        .map_err(|error| io::Error::other(format!("could not reach the host at {}: {error}", socket.display())))?;
    let mut nested = Nested {
        conn: Connection::new(stream)?,
        next_id: 2,
        objects: HashMap::from([(1, wl_display::NAME)]),
        globals: Vec::new(),
        compositor: 0,
        shm: 0,
        wm_base: 0,
        seat: 0,
        surface: 0,
        xdg_surface: 0,
        toplevel: 0,
        pointer: None,
        keyboard: None,
        output: 0,
        size: DEFAULT_SIZE,
        asked: None,
        configured: false,
        buffers: Vec::new(),
        frame: None,
        syncs: HashSet::new(),
        held: HashSet::new(),
        buttons: HashSet::new(),
        time: 0,
    };
    let registry = nested.new_id(wl_registry::NAME);
    nested.send(1, wl_display::Request::GetRegistry { registry })?;
    nested.roundtrip(state)?;

    nested.compositor = nested.bind(registry.0, wl_compositor::NAME, 4)?;
    nested.shm = nested.bind(registry.0, wl_shm::NAME, 1)?;
    nested.wm_base = nested.bind(registry.0, xdg_wm_base::NAME, 1)?;
    nested.seat = nested.bind(registry.0, wl_seat::NAME, 5)?;

    let surface = nested.new_id(wl_surface::NAME);
    nested.send(nested.compositor, wl_compositor::Request::CreateSurface { id: surface })?;
    nested.surface = surface.0;
    let xdg = nested.new_id(xdg_surface::NAME);
    nested.send(
        nested.wm_base,
        xdg_wm_base::Request::GetXdgSurface {
            id: xdg,
            surface: ObjectId(surface.0),
        },
    )?;
    nested.xdg_surface = xdg.0;
    let toplevel = nested.new_id(xdg_toplevel::NAME);
    nested.send(xdg.0, xdg_surface::Request::GetToplevel { id: toplevel })?;
    nested.toplevel = toplevel.0;
    nested.send(
        toplevel.0,
        xdg_toplevel::Request::SetTitle {
            title: "Fulcrum".into(),
        },
    )?;
    nested.send(
        toplevel.0,
        xdg_toplevel::Request::SetAppId {
            app_id: "org.crowbaros.Fulcrum".into(),
        },
    )?;
    nested.send(surface.0, wl_surface::Request::Commit)?;

    let deadline = Instant::now() + PATIENCE;
    while !nested.configured {
        nested.wait(deadline)?;
        nested.read(state)?;
    }
    nested.size = nested.asked.unwrap_or(DEFAULT_SIZE);
    nested.output = state.add_output("NESTED-1", nested.size, REFRESH);
    log::info!(
        "nested in the host session socket={} size={}x{}",
        socket.display(),
        nested.size.0,
        nested.size.1
    );
    state.watch(nested.conn.stream().as_fd(), SOURCE_HOST)?;
    state.set_backend(nested);
    Ok(())
}

impl Nested {
    fn new_id(&mut self, interface: &'static str) -> NewId {
        let id = self.next_id;
        self.next_id += 1;
        self.objects.insert(id, interface);
        NewId(id)
    }

    fn send(&mut self, object: u32, request: impl Encode) -> io::Result<()> {
        let (bytes, fds) = request.to_message(object).map_err(io::Error::other)?;
        self.conn.queue(bytes, fds).map_err(io::Error::other)?;
        self.flush()
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.conn.flush() {
            Ok(_) => Ok(()),
            Err(ConnectionError::Io(error)) => Err(error),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    fn bind(&mut self, registry: u32, interface: &'static str, version: u32) -> io::Result<u32> {
        let (name, offered) = self
            .globals
            .iter()
            .find(|(_, name, _)| name == interface)
            .map(|(name, _, version)| (*name, *version))
            .ok_or_else(|| io::Error::other(format!("the host offers no {interface}")))?;
        let id = self.next_id;
        self.next_id += 1;
        self.objects.insert(id, interface);
        self.send(
            registry,
            wl_registry::Request::Bind {
                name,
                id: fulcrum_wire::DynamicNewId {
                    interface: interface.into(),
                    version: version.min(offered),
                    id,
                },
            },
        )?;
        Ok(id)
    }

    /// Waits until the host has handled every request sent so far.
    fn roundtrip(&mut self, state: &mut State) -> io::Result<()> {
        let callback = self.new_id(wl_callback::NAME);
        self.send(1, wl_display::Request::Sync { callback })?;
        self.syncs.insert(callback.0);
        let deadline = Instant::now() + PATIENCE;
        while self.syncs.contains(&callback.0) {
            self.wait(deadline)?;
            self.read(state)?;
        }
        Ok(())
    }

    fn wait(&mut self, deadline: Instant) -> io::Result<()> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::other("the host did not answer"));
        }
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

    /// Reads and handles every event that has arrived.
    fn read(&mut self, state: &mut State) -> io::Result<()> {
        loop {
            let read = match self.conn.fill() {
                Ok(read) => read,
                Err(ConnectionError::Closed) => return Err(io::Error::other("the host closed the connection")),
                Err(ConnectionError::Io(error)) => return Err(error),
                Err(error) => return Err(io::Error::other(error)),
            };
            while let Some(incoming) = self.conn.next_message().map_err(io::Error::other)? {
                let object = incoming.header.object;
                let Some(interface) = self.objects.get(&object).copied() else {
                    // An object this end already let go of.
                    continue;
                };
                let mut fds = std::mem::take(self.conn.fds());
                let result = self.event(
                    state,
                    object,
                    interface,
                    incoming.header.opcode,
                    &mut ArgReader::new(&incoming.body, &mut fds),
                );
                *self.conn.fds() = fds;
                result?;
            }
            if read == 0 {
                return self.flush();
            }
        }
    }

    fn event(
        &mut self,
        state: &mut State,
        object: u32,
        interface: &str,
        opcode: u16,
        args: &mut ArgReader<'_>,
    ) -> io::Result<()> {
        let bad = |error: DecodeError| io::Error::other(format!("the host sent a bad {interface} event: {error}"));
        match interface {
            wl_display::NAME => match wl_display::Event::decode(opcode, args).map_err(bad)? {
                wl_display::Event::Error {
                    object_id,
                    code,
                    message,
                } => {
                    return Err(io::Error::other(format!(
                        "the host sent protocol error {code} on object {}: {message}",
                        object_id.0
                    )));
                }
                wl_display::Event::DeleteId { id } => {
                    self.objects.remove(&id);
                }
            },
            wl_registry::NAME => match wl_registry::Event::decode(opcode, args).map_err(bad)? {
                wl_registry::Event::Global {
                    name,
                    interface,
                    version,
                } => self.globals.push((name, interface, version)),
                wl_registry::Event::GlobalRemove { name } => self.globals.retain(|global| global.0 != name),
            },
            wl_callback::NAME => {
                let wl_callback::Event::Done { .. } = wl_callback::Event::decode(opcode, args).map_err(bad)?;
                self.syncs.remove(&object);
                if self.frame == Some(object) {
                    self.frame = None;
                    state.frame_shown(self.output);
                }
            }
            wl_buffer::NAME => {
                let wl_buffer::Event::Release = wl_buffer::Event::decode(opcode, args).map_err(bad)?;
                if let Some(buffer) = self.buffers.iter_mut().find(|buffer| buffer.buffer == object) {
                    buffer.busy = false;
                }
            }
            xdg_wm_base::NAME => {
                let xdg_wm_base::Event::Ping { serial } = xdg_wm_base::Event::decode(opcode, args).map_err(bad)?;
                self.send(object, xdg_wm_base::Request::Pong { serial })?;
            }
            xdg_surface::NAME => {
                let xdg_surface::Event::Configure { serial } = xdg_surface::Event::decode(opcode, args).map_err(bad)?;
                self.send(object, xdg_surface::Request::AckConfigure { serial })?;
                self.configured = true;
                if self.output != 0 {
                    let size = self.asked.unwrap_or(self.size);
                    if size != self.size {
                        self.size = size;
                        log::info!("the host resized the window size={}x{}", size.0, size.1);
                        state.resize_output(self.output, size);
                    }
                    // A configure asks for a commit in answer.
                    state.queue_redraw(self.output);
                }
            }
            xdg_toplevel::NAME => match xdg_toplevel::Event::decode(opcode, args).map_err(bad)? {
                xdg_toplevel::Event::Configure { width, height, .. } => {
                    self.asked = (width > 0 && height > 0).then_some((width, height));
                }
                xdg_toplevel::Event::Close => {
                    log::info!("the host closed the window");
                    state.stop();
                }
                _ => {}
            },
            wl_seat::NAME => {
                if let wl_seat::Event::Capabilities { capabilities } =
                    wl_seat::Event::decode(opcode, args).map_err(bad)?
                {
                    self.seat_capabilities(capabilities)?;
                }
            }
            wl_pointer::NAME => self.pointer_event(state, wl_pointer::Event::decode(opcode, args).map_err(bad)?)?,
            wl_keyboard::NAME => self.keyboard_event(state, wl_keyboard::Event::decode(opcode, args).map_err(bad)?),
            _ => {}
        }
        Ok(())
    }

    fn seat_capabilities(&mut self, capabilities: u32) -> io::Result<()> {
        if capabilities & wl_seat::capability::POINTER != 0 && self.pointer.is_none() {
            let id = self.new_id(wl_pointer::NAME);
            self.send(self.seat, wl_seat::Request::GetPointer { id })?;
            self.pointer = Some(id.0);
        }
        if capabilities & wl_seat::capability::KEYBOARD != 0 && self.keyboard.is_none() {
            let id = self.new_id(wl_keyboard::NAME);
            self.send(self.seat, wl_seat::Request::GetKeyboard { id })?;
            self.keyboard = Some(id.0);
        }
        Ok(())
    }

    /// The pointer's place on the desktop, from where it is on the window.
    fn desktop_point(&self, state: &State, x: f64, y: f64) -> (f64, f64) {
        let origin = state
            .outputs()
            .iter()
            .find(|output| output.number == self.output)
            .map_or((0, 0), |output| output.position);
        (f64::from(origin.0) + x, f64::from(origin.1) + y)
    }

    fn pointer_event(&mut self, state: &mut State, event: wl_pointer::Event) -> io::Result<()> {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface_x,
                surface_y,
                ..
            } => {
                // The compositor draws its own pointer into the frame.
                if let Some(pointer) = self.pointer {
                    self.send(
                        pointer,
                        wl_pointer::Request::SetCursor {
                            serial,
                            surface: None,
                            hotspot_x: 0,
                            hotspot_y: 0,
                        },
                    )?;
                }
                let (x, y) = self.desktop_point(state, surface_x.to_f64(), surface_y.to_f64());
                state.pointer_moved_at(x, y, self.time);
            }
            wl_pointer::Event::Motion {
                time,
                surface_x,
                surface_y,
            } => {
                self.time = time;
                let (x, y) = self.desktop_point(state, surface_x.to_f64(), surface_y.to_f64());
                state.pointer_moved_at(x, y, time);
            }
            wl_pointer::Event::Button {
                time,
                button,
                state: pressed,
                ..
            } => {
                self.time = time;
                let pressed = pressed == wl_pointer::button_state::PRESSED;
                if pressed {
                    self.buttons.insert(button);
                } else if !self.buttons.remove(&button) {
                    return Ok(());
                }
                state.pointer_button(button, pressed, time);
            }
            wl_pointer::Event::Axis { time, axis, value } => {
                self.time = time;
                let value = value.to_f64();
                if axis == AXIS_VERTICAL {
                    state.pointer_axis(0.0, value, time);
                } else {
                    state.pointer_axis(value, 0.0, time);
                }
            }
            wl_pointer::Event::Leave { .. } => {
                // A button held as the pointer left is let go, or the
                // compositor's implicit grab would never end.
                let buttons: Vec<u32> = self.buttons.drain().collect();
                for button in buttons {
                    state.pointer_button(button, false, self.time);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn keyboard_event(&mut self, state: &mut State, event: wl_keyboard::Event) {
        match event {
            wl_keyboard::Event::Key {
                time,
                key,
                state: pressed,
                ..
            } => {
                self.time = time;
                let pressed = pressed == wl_keyboard::key_state::PRESSED;
                if pressed {
                    self.held.insert(key);
                } else if !self.held.remove(&key) {
                    return;
                }
                state.handle_key(key, pressed, time);
            }
            wl_keyboard::Event::Leave { .. } => {
                // The keys still down when the window loses the keyboard are
                // let go, or a modifier would stay held in the compositor.
                let held: Vec<u32> = self.held.drain().collect();
                for key in held {
                    state.handle_key(key, false, self.time);
                }
            }
            // The compositor has its own keymap; the keys come as evdev
            // codes either way.
            _ => {}
        }
    }

    /// A free buffer of the window's size, made if there is none.
    fn free_buffer(&mut self) -> io::Result<usize> {
        let size = self.size;
        // Buffers of an old size are let go once the host is done with them.
        let stale: Vec<u32> = self
            .buffers
            .iter()
            .filter(|buffer| buffer.size != size && !buffer.busy)
            .map(|buffer| buffer.buffer)
            .collect();
        for buffer in stale {
            self.send(buffer, wl_buffer::Request::Destroy)?;
            self.buffers.retain(|candidate| candidate.buffer != buffer);
        }
        if let Some(index) = self
            .buffers
            .iter()
            .position(|buffer| buffer.size == size && !buffer.busy)
        {
            return Ok(index);
        }
        let stride = size.0 * 4;
        let bytes = stride * size.1;
        let memory = rustix::fs::memfd_create("fulcrum-nested", rustix::fs::MemfdFlags::CLOEXEC)?;
        rustix::fs::ftruncate(&memory, bytes as u64)?;
        let pool = self.new_id(wl_shm_pool::NAME);
        self.send(
            self.shm,
            wl_shm::Request::CreatePool {
                id: pool,
                fd: memory.try_clone()?,
                size: bytes,
            },
        )?;
        let buffer = self.new_id(wl_buffer::NAME);
        self.send(
            pool.0,
            wl_shm_pool::Request::CreateBuffer {
                id: buffer,
                offset: 0,
                width: size.0,
                height: size.1,
                stride,
                format: wl_shm::format::XRGB8888,
            },
        )?;
        self.send(pool.0, wl_shm_pool::Request::Destroy)?;
        self.buffers.push(HostBuffer {
            memory,
            buffer: buffer.0,
            size,
            busy: false,
        });
        Ok(self.buffers.len() - 1)
    }

    fn show(&mut self, state: &State, output: u64) -> io::Result<Drawn> {
        let Some(image) = state.render_output_with_cursor(output) else {
            return Ok(Drawn::Nothing);
        };
        if (image.width, image.height) != self.size {
            return Ok(Drawn::Nothing);
        }
        let index = self.free_buffer()?;
        let mut bytes = Vec::with_capacity(image.pixels.len() * 4);
        for pixel in &image.pixels {
            bytes.extend_from_slice(&pixel.to_le_bytes());
        }
        rustix::io::pwrite(&self.buffers[index].memory, &bytes, 0)?;
        self.buffers[index].busy = true;
        let buffer = self.buffers[index].buffer;
        let frame = self.new_id(wl_callback::NAME);
        self.send(self.surface, wl_surface::Request::Frame { callback: frame })?;
        self.frame = Some(frame.0);
        self.send(
            self.surface,
            wl_surface::Request::Attach {
                buffer: Some(ObjectId(buffer)),
                x: 0,
                y: 0,
            },
        )?;
        self.send(
            self.surface,
            wl_surface::Request::DamageBuffer {
                x: 0,
                y: 0,
                width: self.size.0,
                height: self.size.1,
            },
        )?;
        self.send(self.surface, wl_surface::Request::Commit)?;
        Ok(Drawn::Flip)
    }
}

impl Backend for Nested {
    fn draw(&mut self, state: &State, output: u64) -> Drawn {
        if output != self.output || self.frame.is_some() {
            return Drawn::Nothing;
        }
        match self.show(state, output) {
            Ok(drawn) => drawn,
            Err(error) => {
                log::warning!("could not show a frame in the host's window: {error}");
                Drawn::Nothing
            }
        }
    }

    fn ready(&mut self, state: &mut State, source: u64) {
        if source != SOURCE_HOST {
            return;
        }
        if let Err(error) = self.read(state) {
            log::error!("lost the host session: {error}");
            state.stop();
        }
    }
}
