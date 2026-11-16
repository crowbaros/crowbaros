//! The TTY backend: a seat from libseat, devices from udev, input from
//! libinput, and output through DRM/KMS with GBM buffers. This is the backend
//! Base runs.
//!
//! One GPU, any number of connectors. Adapted from Smithay's `anvil` example
//! (MIT, see `LICENSE-smithay.txt`), without its multi-GPU, leasing, and
//! direct-scanout tuning.
//!
//! The compositor never asks libseat to change the virtual terminal, so there
//! is no VT switch (ADR 0020).

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use smithay::{
    backend::{
        allocator::{
            Fourcc,
            dmabuf::Dmabuf,
            gbm::{GbmAllocator, GbmBufferFlags, GbmDevice},
        },
        drm::{
            DrmDevice, DrmDeviceFd, DrmEvent, DrmNode,
            compositor::{FrameFlags, PrimaryPlaneElement},
            exporter::gbm::GbmFramebufferExporter,
            output::{DrmOutput, DrmOutputManager, DrmOutputRenderElements},
        },
        egl::{EGLContext, EGLDevice, EGLDisplay},
        libinput::{LibinputInputBackend, LibinputSessionInterface},
        renderer::{
            ImportDma, ImportMemWl,
            element::{
                Id, Kind, render_elements, solid::SolidColorRenderElement, surface::WaylandSurfaceRenderElement,
            },
            gles::GlesRenderer,
            utils::CommitCounter,
        },
        session::{Event as SessionEvent, Session, libseat::LibSeatSession},
        udev::{UdevBackend, UdevEvent, all_gpus, primary_gpu},
    },
    desktop::space::{SpaceRenderElements, space_render_elements},
    output::{Mode as OutputMode, Output},
    reexports::{
        calloop::EventLoop,
        drm::{
            Device as _,
            control::{ModeTypeFlags, connector, crtc},
        },
        input::Libinput,
        rustix::fs::OFlags,
    },
    utils::{DeviceFd, Physical, Point, Rectangle, Scale},
    wayland::dmabuf::DmabufFeedbackBuilder,
};
use smithay_drm_extras::drm_scanner::{DrmScanEvent, DrmScanner};
use tracing::{info, warn};

use crate::{
    State,
    backend::{BACKGROUND, Backend, Drawn},
};

type GbmDrmOutput = DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;
type GbmDrmOutputManager =
    DrmOutputManager<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

render_elements! {
    OutputElement<=GlesRenderer>;
    Space=SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    Pointer=SolidColorRenderElement,
}

/// The name the kernel's connector has on the command line and in logs, such
/// as `Virtual-1`.
fn connector_name(connector: &connector::Info) -> String {
    format!("{}-{}", connector.interface().as_str(), connector.interface_id())
}

/// The side of the square drawn where the pointer is, in logical pixels.
/// Clients' own cursor images are not drawn yet (issue 78).
const POINTER_SIZE: i32 = 12;

/// One connected monitor.
struct Monitor {
    output: Output,
    drm_output: GbmDrmOutput,
    /// Whether the last frame drawn here had to wait for its drawing before
    /// the flip. Kept so that the log says so once, and again if it changes.
    waits_for_drawing: Option<bool>,
}

struct Tty {
    renderer: GlesRenderer,
    manager: GbmDrmOutputManager,
    scanner: DrmScanner,
    monitors: HashMap<crtc::Handle, Monitor>,
    pointer_id: Id,
    pointer_commit: CommitCounter,
    pointer_at: Point<i32, Physical>,
}

pub fn init(event_loop: &mut EventLoop<'static, State>, state: &mut State) -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, session_notifier) = LibSeatSession::new()?;
    let seat = session.seat();
    info!(%seat, "tty: session opened through libseat");

    let gpu_path = primary_gpu(&seat)?
        .or_else(|| all_gpus(&seat).ok().and_then(|gpus| gpus.into_iter().next()))
        .ok_or("no GPU on this seat")?;
    let node = DrmNode::from_path(&gpu_path)?;
    let fd = session.open(
        &gpu_path,
        OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
    )?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let (drm, drm_notifier) = DrmDevice::new(fd.clone(), true)?;
    let driver = drm
        .get_driver()
        .map(|driver| driver.name().to_string_lossy().into_owned())
        .unwrap_or_default();
    let gbm = GbmDevice::new(fd)?;

    // Safety: the GBM device outlives the display made from it.
    let display = unsafe { EGLDisplay::new(gbm.clone())? };
    let software = EGLDevice::device_for_display(&display)
        .map(|device| device.is_software())
        .unwrap_or(false);
    let context = EGLContext::new(&display)?;
    // Safety: the context is current on this thread only.
    let renderer = unsafe { GlesRenderer::new(context)? };
    info!(gpu = %gpu_path.display(), %driver, software, "tty: DRM device opened");

    let allocator = GbmAllocator::new(gbm.clone(), GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);
    let exporter = GbmFramebufferExporter::new(gbm.clone(), Some(node).into());
    let render_formats = renderer.egl_context().dmabuf_render_formats().clone();
    let manager: GbmDrmOutputManager = DrmOutputManager::new(
        drm,
        allocator,
        exporter,
        Some(gbm),
        [Fourcc::Abgr8888, Fourcc::Argb8888],
        render_formats,
    );

    state.shm_state.update_formats(renderer.shm_formats());
    let dmabuf_formats = renderer.dmabuf_formats();
    if dmabuf_formats.iter().next().is_some() {
        state.offer_dmabuf(&DmabufFeedbackBuilder::new(node.dev_id(), dmabuf_formats).build()?);
    }

    let tty = Rc::new(RefCell::new(Tty {
        renderer,
        manager,
        scanner: DrmScanner::new(),
        monitors: HashMap::new(),
        pointer_id: Id::new(),
        pointer_commit: CommitCounter::default(),
        pointer_at: Point::default(),
    }));
    // The compositor and the event sources below hold the same TTY.
    state.set_backend(tty.clone());

    let on_drm = tty.clone();
    event_loop
        .handle()
        .insert_source(drm_notifier, move |event, _, state| match event {
            DrmEvent::VBlank(crtc) => {
                // The borrow ends before the compositor draws again.
                let shown = on_drm.borrow_mut().frame_shown(crtc);
                if let Some(output) = shown {
                    state.frame_shown(&output);
                }
            }
            DrmEvent::Error(error) => warn!(?error, "tty: DRM error"),
        })?;

    let on_udev = tty.clone();
    event_loop
        .handle()
        .insert_source(UdevBackend::new(&seat)?, move |event, _, state| match event {
            UdevEvent::Changed { device_id } if device_id == node.dev_id() => {
                info!("tty: udev says the DRM device changed; scanning connectors");
                scan(&on_udev, state);
            }
            UdevEvent::Added { path, .. } => info!(path = %path.display(), "tty: another DRM device appeared; ignored"),
            _ => {}
        })?;

    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(session.clone().into());
    libinput
        .udev_assign_seat(&seat)
        .map_err(|()| "libinput could not take the seat")?;
    event_loop
        .handle()
        .insert_source(LibinputInputBackend::new(libinput.clone()), |event, _, state| {
            state.process_input_event(event)
        })?;

    let on_session = tty.clone();
    event_loop
        .handle()
        .insert_source(session_notifier, move |event, _, state| match event {
            SessionEvent::PauseSession => {
                info!("tty: session paused");
                libinput.suspend();
                on_session.borrow_mut().manager.pause();
            }
            SessionEvent::ActivateSession => {
                info!("tty: session resumed");
                if libinput.resume().is_err() {
                    warn!("tty: libinput did not resume");
                }
                let outputs: Vec<Output> = {
                    let mut tty = on_session.borrow_mut();
                    if let Err(error) = tty.manager.lock().activate(false) {
                        warn!(?error, "tty: DRM did not reactivate");
                    }
                    tty.monitors.values().map(|monitor| monitor.output.clone()).collect()
                };
                // A flip that was on its way when the session paused is never
                // reported. End that wait, and draw everything again.
                for output in outputs {
                    state.frame_shown(&output);
                    state.queue_redraw(&output);
                }
            }
        })?;

    scan(&tty, state);
    Ok(())
}

/// Looks at the connectors and adds or removes outputs to match.
fn scan(tty: &Rc<RefCell<Tty>>, state: &mut State) {
    let events = {
        let tty = &mut *tty.borrow_mut();
        match tty.scanner.scan_connectors(tty.manager.device()) {
            Ok(events) => events,
            Err(error) => {
                warn!(?error, "tty: connector scan failed");
                return;
            }
        }
    };
    for event in events {
        match event {
            DrmScanEvent::Connected {
                connector,
                crtc: Some(crtc),
            } => connect(tty, state, connector, crtc),
            DrmScanEvent::Disconnected {
                connector,
                crtc: Some(crtc),
            } => {
                let gone = tty.borrow_mut().monitors.remove(&crtc);
                if let Some(monitor) = gone {
                    info!(name = %connector_name(&connector), "output disconnected");
                    state.remove_output(&monitor.output);
                }
            }
            _ => {}
        }
    }
}

/// Brings a connected display up in the mode its monitor prefers.
fn connect(tty: &Rc<RefCell<Tty>>, state: &mut State, connector: connector::Info, crtc: crtc::Handle) {
    let name = connector_name(&connector);
    let Some(mode) = connector
        .modes()
        .iter()
        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| connector.modes().first())
        .copied()
    else {
        warn!(%name, "tty: connector has no mode");
        return;
    };
    let wl_mode = OutputMode::from(mode);
    let output = state.add_output(&name, (wl_mode.size.w, wl_mode.size.h), wl_mode.refresh);

    let tty = &mut *tty.borrow_mut();
    let planes = tty.manager.device().planes(&crtc).ok();
    let drm_output = tty.manager.lock().initialize_output::<_, OutputElement>(
        crtc,
        mode,
        &[connector.handle()],
        &output,
        planes,
        &mut tty.renderer,
        &DrmOutputRenderElements::default(),
    );
    match drm_output {
        Ok(drm_output) => {
            info!(%name, width = wl_mode.size.w, height = wl_mode.size.h, "output connected");
            tty.monitors.insert(
                crtc,
                Monitor {
                    output,
                    drm_output,
                    waits_for_drawing: None,
                },
            );
        }
        Err(error) => {
            warn!(%name, %error, "tty: could not set the output up");
            state.remove_output(&output);
        }
    }
}

impl Tty {
    /// The display showed the frame that was on its way. Returns its output.
    fn frame_shown(&mut self, crtc: crtc::Handle) -> Option<Output> {
        let monitor = self.monitors.get_mut(&crtc)?;
        if let Err(error) = monitor.drm_output.frame_submitted() {
            warn!(%error, "tty: frame_submitted failed");
        }
        Some(monitor.output.clone())
    }

    fn draw(&mut self, state: &State, output: &Output) -> Drawn {
        let Some(monitor) = self.monitors.values_mut().find(|monitor| &monitor.output == output) else {
            return Drawn::Nothing;
        };
        let Some(geometry) = state.space.output_geometry(output) else {
            return Drawn::Nothing;
        };
        let scale = Scale::from(output.current_scale().fractional_scale());

        let mut elements: Vec<OutputElement> = Vec::new();
        let pointer = state.pointer().current_location();
        if geometry.to_f64().contains(pointer) {
            let at: Point<i32, Physical> = (pointer - geometry.loc.to_f64()).to_physical(scale).to_i32_round();
            if at != self.pointer_at {
                self.pointer_at = at;
                self.pointer_commit.increment();
            }
            let side = (f64::from(POINTER_SIZE) * scale.x).round() as i32;
            elements.push(OutputElement::Pointer(SolidColorRenderElement::new(
                self.pointer_id.clone(),
                Rectangle::new(at, (side, side).into()),
                self.pointer_commit,
                [1.0, 1.0, 1.0, 1.0],
                Kind::Cursor,
            )));
        }
        match space_render_elements(&mut self.renderer, [&state.space], output, 1.0) {
            Ok(space) => elements.extend(space.into_iter().map(OutputElement::Space)),
            Err(_) => return Drawn::Nothing,
        }

        let frame =
            match monitor
                .drm_output
                .render_frame(&mut self.renderer, &elements, BACKGROUND, FrameFlags::DEFAULT)
            {
                Ok(frame) => frame,
                Err(error) => {
                    warn!(%error, "tty: render failed");
                    return Drawn::Nothing;
                }
            };
        if frame.is_empty {
            return Drawn::Nothing;
        }
        // Where the driver cannot take the drawing's fence with the flip, the
        // flip must wait until the drawing is done: a software renderer shows
        // a half-drawn frame without this. Where it can, there is no wait.
        // `needs_sync` is Smithay's own test for which case this is.
        if let PrimaryPlaneElement::Swapchain(element) = &frame.primary_element {
            let waits = frame.needs_sync();
            if monitor.waits_for_drawing != Some(waits) {
                monitor.waits_for_drawing = Some(waits);
                info!(
                    output = %monitor.output.name(),
                    needs_sync = waits,
                    "tty: whether a flip waits for its drawing (Smithay's needs_sync)"
                );
            }
            if waits {
                let _ = element.sync.wait();
            }
        }
        drop(frame);
        match monitor.drm_output.queue_frame(()) {
            Ok(()) => Drawn::Flip,
            Err(error) => {
                warn!(%error, "tty: could not queue the frame");
                Drawn::Nothing
            }
        }
    }
}

impl Backend for Rc<RefCell<Tty>> {
    fn draw(&mut self, state: &State, output: &Output) -> Drawn {
        self.borrow_mut().draw(state, output)
    }

    fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> bool {
        self.borrow_mut().renderer.import_dmabuf(dmabuf, None).is_ok()
    }
}
