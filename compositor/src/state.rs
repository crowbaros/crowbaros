use std::{
    sync::Weak,
    time::{Duration, Instant},
};

use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    desktop::{PopupManager, Space, Window, layer_map_for_output},
    input::{Seat, SeatState},
    output::{Mode as OutputMode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction, generic::Generic},
        wayland_server::{Display, DisplayHandle, backend::GlobalId},
    },
    utils::{Logical, Point, Rectangle},
    wayland::{
        compositor::CompositorState,
        dmabuf::{DmabufFeedback, DmabufGlobal, DmabufState},
        security_context::SecurityContextState,
        shell::{wlr_layer::WlrLayerShellState, xdg::XdgShellState},
        shm::ShmState,
        viewporter::ViewporterState,
    },
};

use crate::{backend::Backend, identity::Compartment, policy, redraw::Redraw};

/// The compositor: one seat, its outputs, and what is shown on them.
pub struct State {
    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, State>,
    pub loop_signal: LoopSignal,
    pub(crate) start_time: Instant,

    pub space: Space<Window>,
    pub(crate) popups: PopupManager,
    pub seat: Seat<Self>,

    pub(crate) compositor_state: CompositorState,
    pub(crate) xdg_shell_state: XdgShellState,
    pub shm_state: ShmState,
    pub(crate) seat_state: SeatState<State>,
    pub(crate) layer_shell_state: WlrLayerShellState,
    pub(crate) dmabuf_state: DmabufState,
    // Each of these holds a global for as long as the compositor runs.
    _dmabuf_global: Option<DmabufGlobal>,
    _viewporter_state: ViewporterState,
    _security_context_state: SecurityContextState,

    pub(crate) screens: Vec<Screen>,
    pub(crate) backend: Option<Box<dyn Backend>>,
    /// The compartments whose listeners the Warden registered. An entry is
    /// dead once its listener is closed and its last client is gone.
    pub(crate) compartments: Vec<Weak<Compartment>>,

    /// How many frames reached a display, on all outputs together.
    pub frames_drawn: u64,
    /// How many times the compositor looked for something to draw. On a
    /// still desktop neither number moves.
    pub draw_passes: u64,
}

/// One output and where the compositor is in drawing it.
pub(crate) struct Screen {
    pub output: Output,
    pub global: GlobalId,
    pub redraw: Redraw,
}

impl State {
    /// Creates the compositor and its globals. It accepts no client until
    /// [`State::listen`] is called.
    pub fn new(event_loop: &mut EventLoop<'static, Self>, display: Display<Self>) -> Self {
        let dh = display.handle();

        // Version 6 lets the compositor tell each surface its scale.
        let compositor_state = CompositorState::new_v6::<Self>(&dh);
        let xdg_shell_state = XdgShellState::new::<Self>(&dh);
        let shm_state = ShmState::new::<Self>(&dh, vec![]);
        let layer_shell_state = WlrLayerShellState::new::<Self>(&dh);
        let viewporter_state = ViewporterState::new::<Self>(&dh);
        // Smithay asks for a filter of its own here. It is the table's
        // answer, which the dispatch in `handlers/mod.rs` also gives.
        let security_context_state = SecurityContextState::new::<Self, _>(&dh, |client| {
            policy::offered_to(client, "wp_security_context_manager_v1")
        });

        // One seat, with a keyboard and a pointer (ADR 0020).
        let mut seat_state = SeatState::new();
        let mut seat: Seat<Self> = seat_state.new_wl_seat(&dh, "seat0");
        seat.add_keyboard(Default::default(), 200, 25)
            .expect("the default keymap compiles");
        seat.add_pointer();

        event_loop
            .handle()
            .insert_source(
                Generic::new(display, Interest::READ, Mode::Level),
                |_, display, state| {
                    // Safety: the display is not dropped while the source lives.
                    unsafe { display.get_mut().dispatch_clients(state) }?;
                    Ok(PostAction::Continue)
                },
            )
            .expect("the display joins the event loop");

        Self {
            display_handle: dh,
            loop_handle: event_loop.handle(),
            loop_signal: event_loop.get_signal(),
            start_time: Instant::now(),
            space: Space::default(),
            popups: PopupManager::default(),
            seat,
            compositor_state,
            xdg_shell_state,
            shm_state,
            seat_state,
            layer_shell_state,
            dmabuf_state: DmabufState::new(),
            _dmabuf_global: None,
            _viewporter_state: viewporter_state,
            _security_context_state: security_context_state,
            screens: Vec::new(),
            backend: None,
            compartments: Vec::new(),
            frames_drawn: 0,
            draw_passes: 0,
        }
    }

    /// Sets where frames go. Without a backend nothing is drawn.
    pub fn set_backend(&mut self, backend: impl Backend + 'static) {
        self.backend = Some(Box::new(backend));
        self.queue_redraw_all();
    }

    /// Offers linux-dmabuf, with the formats the backend's renderer takes.
    /// The table still decides which clients see it.
    pub fn offer_dmabuf(&mut self, feedback: &DmabufFeedback) {
        let global = self
            .dmabuf_state
            .create_global_with_default_feedback::<State>(&self.display_handle, feedback);
        self._dmabuf_global = Some(global);
    }

    /// Takes a client's dmabuf into the backend's renderer.
    pub(crate) fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        self.backend
            .as_mut()
            .is_some_and(|backend| backend.import_dmabuf(dmabuf))
    }

    pub fn outputs(&self) -> impl Iterator<Item = &Output> {
        self.screens.iter().map(|screen| &screen.output)
    }

    /// Adds an output to the right of the existing ones, at scale 1.
    /// `refresh` is in millihertz.
    pub fn add_output(&mut self, name: &str, size: (i32, i32), refresh: i32) -> Output {
        let mode = OutputMode {
            size: size.into(),
            refresh,
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
        let x = self.desktop_bounds().map_or(0, |bounds| bounds.loc.x + bounds.size.w);
        output.change_current_state(Some(mode), None, Some(Scale::Integer(1)), Some((x, 0).into()));
        output.set_preferred(mode);
        self.space.map_output(&output, (x, 0));
        self.screens.push(Screen {
            output: output.clone(),
            global,
            redraw: Redraw::default(),
        });
        self.queue_redraw(&output);
        output
    }

    /// Removes an output. A window that is then on no output moves to the
    /// first output that is left.
    pub fn remove_output(&mut self, output: &Output) {
        let Some(index) = self.screens.iter().position(|screen| &screen.output == output) else {
            return;
        };
        let screen = self.screens.remove(index);
        self.display_handle.remove_global::<State>(screen.global);
        self.space.unmap_output(output);
        if let Some(first) = self.screens.first().map(|screen| screen.output.clone()) {
            let area = self.usable_area(&first);
            let stranded: Vec<Window> = self
                .space
                .elements()
                .filter(|window| {
                    let at = self.space.element_location(window).unwrap_or_default();
                    !self.desktop_bounds().is_some_and(|bounds| bounds.contains(at))
                })
                .cloned()
                .collect();
            for window in stranded {
                self.space.map_element(window, area.loc + Point::from((48, 48)), false);
            }
        }
        self.queue_redraw_all();
    }

    /// The rectangle that holds every output.
    pub fn desktop_bounds(&self) -> Option<Rectangle<i32, Logical>> {
        self.outputs()
            .filter_map(|output| self.space.output_geometry(output))
            .reduce(|a, b| a.merge(b))
    }

    /// The part of an output that the shell's layer surfaces have not
    /// reserved, in global coordinates.
    pub fn usable_area(&self, output: &Output) -> Rectangle<i32, Logical> {
        let mut area = layer_map_for_output(output).non_exclusive_zone();
        if let Some(geometry) = self.space.output_geometry(output) {
            area.loc += geometry.loc;
        }
        area
    }

    /// How long one frame lasts on an output.
    pub(crate) fn frame_time(output: &Output) -> Duration {
        let millihertz = output.current_mode().map_or(60_000, |mode| mode.refresh.max(1_000));
        Duration::from_secs_f64(1_000.0 / f64::from(millihertz))
    }
}
