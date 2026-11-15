use std::{
    collections::HashSet,
    ffi::OsString,
    os::unix::{net::UnixStream, process::CommandExt},
    process::{Child, Command},
    sync::Arc,
};

use smithay::{
    backend::{allocator::dmabuf::Dmabuf, input::Keycode},
    desktop::{PopupManager, Space, Window, WindowSurfaceType, layer_map_for_output},
    input::{Seat, SeatState},
    reexports::{
        calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason, GlobalId},
            protocol::wl_surface::WlSurface,
        },
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale, Subpixel},
    utils::{Logical, Point},
    wayland::{
        compositor::{
            CompositorClientState, CompositorState, SurfaceData, get_parent, send_surface_state, with_states,
        },
        dmabuf::{DmabufGlobal, DmabufState},
        fractional_scale::{FractionalScaleManagerState, with_fractional_scale},
        output::OutputManagerState,
        security_context::SecurityContextState,
        selection::data_device::DataDeviceState,
        shell::{
            wlr_layer::{Layer, WlrLayerShellState},
            xdg::{XdgShellState, XdgToplevelSurfaceData},
        },
        shm::ShmState,
        socket::ListeningSocketSource,
        viewporter::ViewporterState,
    },
};

use crate::{
    policy::{self, Principal},
    windows::Managed,
};

pub struct State {
    pub start_time: std::time::Instant,
    pub socket_name: OsString,
    pub display_handle: DisplayHandle,

    pub space: Space<Window>,
    pub loop_signal: LoopSignal,
    pub loop_handle: LoopHandle<'static, State>,

    // Smithay State
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub output_manager_state: OutputManagerState,
    pub seat_state: SeatState<State>,
    pub data_device_state: DataDeviceState,
    pub popups: PopupManager,
    pub layer_shell_state: WlrLayerShellState,
    pub viewporter_state: ViewporterState,
    pub fractional_scale_state: FractionalScaleManagerState,
    pub security_context_state: SecurityContextState,
    pub dmabuf_state: DmabufState,
    /// Created by a backend that has a renderer. The tests have none.
    pub dmabuf_global: Option<DmabufGlobal>,
    /// How the backend's renderer takes a client's dmabuf.
    pub dmabuf_import: Option<Box<dyn FnMut(&Dmabuf) -> bool>>,
    /// A layer surface that asked for the keyboard and has it.
    pub layer_focus: Option<WlSurface>,
    /// Commits per window or layer surface, and frames drawn. Evidence only.
    pub commit_counts: std::collections::BTreeMap<String, u64>,
    pub frames: u64,
    pub last_commit_log: std::time::Instant,

    pub seat: Seat<Self>,
    pub outputs: Vec<Output>,
    output_globals: Vec<(Output, GlobalId)>,
    /// The TTY backend, when the compositor runs on one.
    pub tty: Option<crate::tty::Tty>,
    /// Scales by output name, from the command line.
    pub output_scales: Vec<(String, f64)>,
    pub windows: Vec<Managed>,
    pub next_window_id: u32,
    /// How many times the compositor took the confirm chord for itself.
    pub active_workspace: u32,
    /// The id of the window with the keyboard focus.
    pub active_window: Option<u32>,
    pub focus_counter: u64,
    /// The compositor's end of the shell's control channel.
    pub control: Option<UnixStream>,
    pub confirms_taken: u32,
    /// Keys whose press the compositor kept from every client.
    pub withheld_keys: HashSet<Keycode>,
}

impl State {
    pub fn new(event_loop: &mut EventLoop<'static, Self>, display: Display<Self>) -> Self {
        let start_time = std::time::Instant::now();

        let dh = display.handle();

        // Here we initialize implementations of some wayland protocols
        // Some of them require us to implement traits on the State state,
        // you can find those implementations in the `crate::handlers` module

        // Initialize protocols needed for displaying windows
        // Version 6 lets the compositor tell each surface its scale.
        let compositor_state = CompositorState::new_v6::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let popups = PopupManager::default();

        let viewporter_state = ViewporterState::new::<Self>(&dh);
        let fractional_scale_state = FractionalScaleManagerState::new::<Self>(&dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let security_context_state = SecurityContextState::new::<Self, _>(&dh, |client| {
            policy::client_may_see(client, "wp_security_context_manager_v1")
        });

        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(&dh);

        // Data device is responsible for clipboard and drag-and-drop
        let data_device_state = DataDeviceState::new::<Self>(&dh);

        // A seat is a group of keyboards, pointer and touch devices.
        // A seat typically has a pointer and maintains a keyboard focus and a pointer focus.
        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "seat0");

        // Notify clients that we have a keyboard, for the sake of the example we assume that keyboard is always present.
        // You may want to track keyboard hot-plug in real compositor.
        seat.add_keyboard(Default::default(), 200, 25).unwrap();

        // Notify clients that we have a pointer (mouse)
        // Here we assume that there is always pointer plugged in
        seat.add_pointer();

        // A space represents a two-dimensional plane. Windows and Outputs can be mapped onto it.
        //
        // Windows get a position and stacking order through mapping.
        // Outputs become views of a part of the Space and can be rendered via Space::render_output.
        let space = Space::default();

        // Setup a wayland socket that will be used to accept clients
        let socket_name = Self::init_wayland_listener(display, event_loop);

        // Get the loop signal, used to stop the event loop
        let loop_signal = event_loop.get_signal();

        Self {
            start_time,
            display_handle: dh,

            space,
            loop_signal,
            loop_handle: event_loop.handle(),
            socket_name,

            compositor_state,
            xdg_shell_state,
            shm_state,
            output_manager_state,
            seat_state,
            data_device_state,
            popups,
            layer_shell_state,
            viewporter_state,
            fractional_scale_state,
            security_context_state,
            dmabuf_state: DmabufState::new(),
            dmabuf_global: None,
            dmabuf_import: None,
            layer_focus: None,
            commit_counts: Default::default(),
            frames: 0,
            last_commit_log: std::time::Instant::now(),
            seat,
            outputs: Vec::new(),
            output_globals: Vec::new(),
            tty: None,
            output_scales: Vec::new(),
            windows: Vec::new(),
            next_window_id: 0,
            active_workspace: 1,
            active_window: None,
            focus_counter: 0,
            control: None,
            confirms_taken: 0,
            withheld_keys: HashSet::new(),
        }
    }

    fn init_wayland_listener(display: Display<State>, event_loop: &mut EventLoop<'static, Self>) -> OsString {
        // Creates a new listening socket, automatically choosing the next available `wayland` socket name.
        let listening_socket = ListeningSocketSource::new_auto().unwrap();

        // Get the name of the listening socket.
        // Clients will connect to this socket.
        let socket_name = listening_socket.socket_name().to_os_string();

        let loop_handle = event_loop.handle();

        loop_handle
            .insert_source(listening_socket, move |client_stream, _, state| {
                // Inside the callback, you should insert the client into the display.
                //
                // You may also associate some data with the client when inserting the client.
                state
                    .display_handle
                    .insert_client(client_stream, Arc::new(ClientState::new(Principal::Ordinary)))
                    .unwrap();
            })
            .expect("Failed to init the wayland event source.");

        // You also need to add the display itself to the event loop, so that client events will be processed by wayland-server.
        loop_handle
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // Safety: we don't drop the display
                    unsafe {
                        display.get_mut().dispatch_clients(state).unwrap();
                    }
                    Ok(PostAction::Continue)
                },
            )
            .unwrap();

        socket_name
    }

    /// Opens the shell's connection. The compositor keeps one end and returns
    /// the other, which goes to the shell process and to nothing else.
    pub fn connect_shell(&mut self) -> std::io::Result<ShellChannels> {
        let (ours, wayland) = UnixStream::pair()?;
        self.display_handle
            .insert_client(ours, Arc::new(ClientState::new(Principal::Shell)))
            .map_err(std::io::Error::other)?;
        let (ours, control) = UnixStream::pair()?;
        self.serve_control(ours)?;
        Ok(ShellChannels { wayland, control })
    }

    /// The configured scale for an output, or 1.
    pub fn scale_for(&self, name: &str) -> f64 {
        self.output_scales
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, scale)| *scale)
            .unwrap_or(1.0)
    }

    /// Adds an output to the right of the existing ones. Every backend and
    /// the tests create outputs here.
    pub fn add_output(&mut self, name: &str, size: (i32, i32), scale: f64) -> Output {
        let mode = OutputMode {
            size: size.into(),
            refresh: 60_000,
        };
        let output = Output::new(
            name.to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "CrowbarOS".into(),
                model: name.into(),
                serial_number: "Unknown".into(),
            },
        );
        let global = output.create_global::<State>(&self.display_handle);
        self.output_globals.push((output.clone(), global));
        let x = self
            .space
            .outputs()
            .filter_map(|o| self.space.output_geometry(o))
            .map(|geo| geo.loc.x + geo.size.w)
            .max()
            .unwrap_or(0);
        output.change_current_state(
            Some(mode),
            None,
            Some(Scale::Fractional(scale)),
            Some((x, 0).into()),
        );
        output.set_preferred(mode);
        self.space.map_output(&output, (x, 0));
        self.outputs.push(output.clone());
        output
    }

    /// Removes an output. Its windows move to the first output that is left.
    pub fn remove_output(&mut self, output: &Output) {
        self.outputs.retain(|o| o != output);
        self.space.unmap_output(output);
        self.output_globals.retain(|(o, global)| {
            if o == output {
                self.display_handle.remove_global::<State>(global.clone());
            }
            o != output
        });
        if let Some(first) = self.outputs.first().cloned() {
            let area = self.usable_area(&first);
            for managed in &mut self.windows {
                if self.space.outputs_for_element(&managed.window).is_empty() {
                    managed.location = area.loc + Point::from((48, 48));
                    if managed.workspace == self.active_workspace {
                        self.space.map_element(managed.window.clone(), managed.location, false);
                    }
                }
            }
        }
    }

    /// The surface under a point, in stacking order: overlay and top layers,
    /// then windows, then bottom and background layers.
    pub fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        let output = self.space.output_under(pos).next()?;
        let output_loc = self.space.output_geometry(output)?.loc;
        let layers = layer_map_for_output(output);
        let on_layers = |kinds: [Layer; 2]| {
            kinds.into_iter().find_map(|kind| {
                let layer = layers.layer_under(kind, pos - output_loc.to_f64())?;
                let layer_loc = layers.layer_geometry(layer)?.loc + output_loc;
                layer
                    .surface_under(pos - layer_loc.to_f64(), WindowSurfaceType::ALL)
                    .map(|(surface, loc)| (surface, (loc + layer_loc).to_f64()))
            })
        };
        on_layers([Layer::Overlay, Layer::Top])
            .or_else(|| {
                self.space.element_under(pos).and_then(|(window, location)| {
                    window
                        .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                        .map(|(s, p)| (s, (p + location).to_f64()))
                })
            })
            .or_else(|| on_layers([Layer::Bottom, Layer::Background]))
    }

    /// Starts the shell and hands it its two sockets. `WAYLAND_SOCKET` is the
    /// standard variable a Wayland client reads for an already connected
    /// socket. `WAYLAND_DISPLAY` names the public socket, for the programs
    /// the shell starts.
    pub fn spawn_shell(&mut self, command: &[String]) -> std::io::Result<Child> {
        use std::os::fd::AsRawFd;
        let channels = self.connect_shell()?;
        let (wayland, control) = (channels.wayland.as_raw_fd(), channels.control.as_raw_fd());
        let mut child = Command::new(&command[0]);
        child
            .args(&command[1..])
            .env("WAYLAND_SOCKET", wayland.to_string())
            .env("CROWBAR_SHELL_CONTROL_FD", control.to_string())
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE");
        // Safety: only async-signal-safe calls between fork and exec.
        unsafe {
            child.pre_exec(move || {
                for fd in [wayland, control] {
                    let fd = std::os::fd::BorrowedFd::borrow_raw(fd);
                    rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::empty())?;
                }
                Ok(())
            });
        }
        child.spawn()
    }

    /// Starts a program as an ordinary client of the public socket.
    pub fn spawn_ordinary(&self, command: &[String]) -> std::io::Result<Child> {
        Command::new(&command[0])
            .args(&command[1..])
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env_remove("WAYLAND_SOCKET")
            .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
            .spawn()
    }

    /// Tells every surface the scale of the output it is on.
    pub fn update_surface_scales(&self) {
        let apply = |surface: &WlSurface, states: &SurfaceData, output: &Output| {
            let scale = output.current_scale();
            send_surface_state(surface, states, scale.integer_scale(), output.current_transform());
            with_fractional_scale(states, |fractional| {
                fractional.set_preferred_scale(scale.fractional_scale());
            });
        };
        for window in self.space.elements() {
            if let Some(output) = self.space.outputs_for_element(window).first() {
                window.with_surfaces(|surface, states| apply(surface, states, output));
            }
        }
        for output in &self.outputs {
            for layer in layer_map_for_output(output).layers() {
                layer.with_surfaces(|surface, states| apply(surface, states, output));
            }
        }
    }

    /// Sends frame callbacks to everything shown on an output.
    pub fn send_frames(&self, output: &Output) {
        let time = self.start_time.elapsed();
        let throttle = Some(std::time::Duration::ZERO);
        for window in self.space.elements() {
            window.send_frame(output, time, throttle, |_, _| Some(output.clone()));
        }
        for layer in layer_map_for_output(output).layers() {
            layer.send_frame(output, time, throttle, |_, _| Some(output.clone()));
        }
    }

    /// Counts a commit against the window or layer surface it belongs to.
    /// The counts are logged, so a client that stops drawing shows up.
    pub fn count_commit(&mut self, surface: &WlSurface) {
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        let label = if let Some(managed) = self.managed(&root) {
            let app_id = managed
                .window
                .toplevel()
                .and_then(|t| {
                    with_states(t.wl_surface(), |states| {
                        states
                            .data_map
                            .get::<XdgToplevelSurfaceData>()
                            .and_then(|data| data.lock().unwrap().app_id.clone())
                    })
                })
                .unwrap_or_default();
            format!("window {:#x} {app_id}", managed.id)
        } else if let Some(namespace) = self.outputs.iter().find_map(|output| {
            layer_map_for_output(output)
                .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
                .map(|layer| layer.namespace().to_owned())
        }) {
            format!("layer {namespace}")
        } else {
            return;
        };
        *self.commit_counts.entry(label).or_insert(0) += 1;
    }

    /// Logs the commit counts every few seconds.
    pub fn log_commit_counts(&mut self) {
        if self.last_commit_log.elapsed() < std::time::Duration::from_secs(5) {
            return;
        }
        self.last_commit_log = std::time::Instant::now();
        let mut counts: Vec<_> = self.commit_counts.iter().collect();
        counts.sort();
        let text: Vec<String> = counts.iter().map(|(label, n)| format!("{label}: {n}")).collect();
        tracing::info!(frames = self.frames, "commits so far: {}", text.join(", "));
    }

    /// Housekeeping after a frame.
    pub fn after_frame(&mut self) {
        self.space.refresh();
        self.popups.cleanup();
        for output in &self.outputs {
            layer_map_for_output(output).cleanup();
        }
        self.update_surface_scales();
        self.frames += 1;
        self.log_commit_counts();
        let _ = self.display_handle.flush_clients();
    }
}

/// The shell's ends of its two sockets. Whoever holds them is the shell.
pub struct ShellChannels {
    /// A connected Wayland socket. The shell is the only client that sees
    /// layer-shell.
    pub wayland: UnixStream,
    /// The control channel. See `control.rs`.
    pub control: UnixStream,
}

/// Data associated with a wayland client that connects to State.
/// One instance of this type per client.
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    /// Fixed when the connection is made. See `policy.rs`.
    pub principal: Principal,
}

impl ClientState {
    pub fn new(principal: Principal) -> Self {
        ClientState {
            compositor_state: CompositorClientState::default(),
            principal,
        }
    }
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}
