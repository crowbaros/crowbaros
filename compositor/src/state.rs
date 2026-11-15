use std::{collections::HashSet, ffi::OsString, os::unix::net::UnixStream, sync::Arc};

use smithay::{
    backend::input::Keycode,
    desktop::{PopupManager, Space, Window, WindowSurfaceType},
    input::{Seat, SeatState},
    reexports::{
        calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_server::{
            Display, DisplayHandle,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::wl_surface::WlSurface,
        },
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale, Subpixel},
    utils::{Logical, Point},
    wayland::{
        compositor::{CompositorClientState, CompositorState},
        fractional_scale::FractionalScaleManagerState,
        output::OutputManagerState,
        security_context::SecurityContextState,
        selection::data_device::DataDeviceState,
        shell::{wlr_layer::WlrLayerShellState, xdg::XdgShellState},
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

    pub seat: Seat<Self>,
    pub outputs: Vec<Output>,
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
        let compositor_state = CompositorState::new::<Self>(&dh);
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
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "winit");

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
            seat,
            outputs: Vec::new(),
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
        let _global = output.create_global::<State>(&self.display_handle);
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

    pub fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space.element_under(pos).and_then(|(window, location)| {
            window
                .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                .map(|(s, p)| (s, (p + location).to_f64()))
        })
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
