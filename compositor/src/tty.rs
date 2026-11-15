//! The TTY backend: a seat from libseat, devices from udev, input from
//! libinput, and output through DRM/KMS with GBM buffers.
//!
//! One GPU, any number of connectors. Adapted from Smithay's `anvil` example
//! (MIT), without its multi-GPU, leasing, and direct-scanout tuning.

use std::{collections::HashMap, time::Duration};

use smithay::{
    backend::{
        allocator::{
            Fourcc,
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
    output::Output,
    reexports::{
        calloop::{
            EventLoop,
            timer::{TimeoutAction, Timer},
        },
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

use crate::State;

type GbmDrmOutput = DrmOutput<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;
type GbmDrmOutputManager =
    DrmOutputManager<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

render_elements! {
    pub OutputElement<=GlesRenderer>;
    Space=SpaceRenderElements<GlesRenderer, WaylandSurfaceRenderElement<GlesRenderer>>,
    Cursor=SolidColorRenderElement,
}

/// The name the kernel's connector has on the command line and in logs, such
/// as `Virtual-1`.
fn connector_name(connector: &connector::Info) -> String {
    format!("{}-{}", connector.interface().as_str(), connector.interface_id())
}

/// The side of the pointer square, in logical pixels. Clients' own cursor
/// images are not drawn in this spike.
const CURSOR_SIZE: i32 = 12;

struct Surface {
    output: Output,
    drm_output: GbmDrmOutput,
    /// A page flip is queued and its vblank has not arrived.
    flip_pending: bool,
    /// A timer is already set to draw this output again.
    retry_pending: bool,
}

pub struct Tty {
    pub renderer: GlesRenderer,
    manager: GbmDrmOutputManager,
    scanner: DrmScanner,
    surfaces: HashMap<crtc::Handle, Surface>,
    cursor_id: Id,
    cursor_commit: CommitCounter,
    cursor_at: Point<i32, Physical>,
}

pub fn init_tty(
    event_loop: &mut EventLoop<'static, State>,
    state: &mut State,
) -> Result<(), Box<dyn std::error::Error>> {
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
        .map(|d| d.name().to_string_lossy().into_owned())
        .unwrap_or_default();
    let gbm = GbmDevice::new(fd)?;

    let display = unsafe { EGLDisplay::new(gbm.clone())? };
    let software = EGLDevice::device_for_display(&display)
        .map(|device| device.is_software())
        .unwrap_or(false);
    let context = EGLContext::new(&display)?;
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
        let feedback = DmabufFeedbackBuilder::new(node.dev_id(), dmabuf_formats).build()?;
        let global = state
            .dmabuf_state
            .create_global_with_default_feedback::<State>(&state.display_handle, &feedback);
        state.dmabuf_global = Some(global);
    }

    event_loop
        .handle()
        .insert_source(drm_notifier, |event, _, state| match event {
            DrmEvent::VBlank(crtc) => state.tty_frame_finished(crtc),
            DrmEvent::Error(error) => warn!(?error, "tty: DRM error"),
        })?;

    let udev = UdevBackend::new(&seat)?;
    event_loop
        .handle()
        .insert_source(udev, move |event, _, state| match event {
            UdevEvent::Changed { device_id } if device_id == node.dev_id() => {
                info!("tty: udev says the DRM device changed; scanning connectors");
                state.tty_scan();
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

    event_loop
        .handle()
        .insert_source(session_notifier, move |event, _, state| match event {
            SessionEvent::PauseSession => {
                info!("tty: session paused");
                libinput.suspend();
                if let Some(tty) = state.tty.as_mut() {
                    tty.manager.pause();
                }
            }
            SessionEvent::ActivateSession => {
                info!("tty: session resumed");
                if libinput.resume().is_err() {
                    warn!("tty: libinput did not resume");
                }
                let crtcs: Vec<_> = match state.tty.as_mut() {
                    Some(tty) => {
                        if let Err(error) = tty.manager.lock().activate(false) {
                            warn!(?error, "tty: DRM did not reactivate");
                        }
                        tty.surfaces.values_mut().for_each(|s| s.flip_pending = false);
                        tty.surfaces.keys().copied().collect()
                    }
                    None => Vec::new(),
                };
                for crtc in crtcs {
                    state.tty_render(crtc);
                }
            }
        })?;

    state.tty = Some(Tty {
        renderer,
        manager,
        scanner: DrmScanner::new(),
        surfaces: HashMap::new(),
        cursor_id: Id::new(),
        cursor_commit: CommitCounter::default(),
        cursor_at: Point::default(),
    });
    state.tty_scan();
    Ok(())
}

impl State {
    /// Looks at the connectors and adds or removes outputs to match.
    pub fn tty_scan(&mut self) {
        let Some(tty) = self.tty.as_mut() else {
            return;
        };
        let events = match tty.scanner.scan_connectors(tty.manager.device()) {
            Ok(events) => events,
            Err(error) => {
                warn!(?error, "tty: connector scan failed");
                return;
            }
        };
        for event in events {
            match event {
                DrmScanEvent::Connected {
                    connector,
                    crtc: Some(crtc),
                } => self.tty_connect(connector, crtc),
                DrmScanEvent::Disconnected {
                    connector,
                    crtc: Some(crtc),
                } => self.tty_disconnect(connector, crtc),
                _ => {}
            }
        }
    }

    fn tty_connect(&mut self, connector: connector::Info, crtc: crtc::Handle) {
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
        let (width, height) = mode.size();
        let scale = self.scale_for(&name);
        let output = self.add_output(&name, (width as i32, height as i32), scale);

        let Some(mut tty) = self.tty.take() else {
            return;
        };
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
                info!(%name, width, height, scale, "output connected");
                tty.surfaces.insert(
                    crtc,
                    Surface {
                        output,
                        drm_output,
                        flip_pending: false,
                        retry_pending: false,
                    },
                );
                self.loop_handle.insert_idle(move |state| state.tty_render(crtc));
            }
            Err(error) => {
                warn!(%name, %error, "tty: could not set the output up");
                self.remove_output(&output);
            }
        }
        self.tty = Some(tty);
    }

    fn tty_disconnect(&mut self, connector: connector::Info, crtc: crtc::Handle) {
        let name = connector_name(&connector);
        let surface = self.tty.as_mut().and_then(|tty| tty.surfaces.remove(&crtc));
        if let Some(surface) = surface {
            info!(%name, "output disconnected");
            self.remove_output(&surface.output);
        }
    }

    fn tty_frame_finished(&mut self, crtc: crtc::Handle) {
        let Some(surface) = self.tty.as_mut().and_then(|tty| tty.surfaces.get_mut(&crtc)) else {
            return;
        };
        surface.flip_pending = false;
        if let Err(error) = surface.drm_output.frame_submitted() {
            warn!(%error, "tty: frame_submitted failed");
        }
        self.tty_render(crtc);
    }

    /// Draws one output and queues the page flip. When nothing changed there
    /// is no flip and so no vblank, and a timer asks again.
    pub fn tty_render(&mut self, crtc: crtc::Handle) {
        let Some(mut tty) = self.tty.take() else {
            return;
        };
        let mut retry = self.tty_render_surface(&mut tty, crtc);
        // One timer per output, however many callers asked for a redraw.
        if let Some(surface) = tty.surfaces.get_mut(&crtc) {
            retry = retry && !surface.retry_pending;
            surface.retry_pending |= retry;
        }
        self.tty = Some(tty);
        if retry {
            let _ =
                self.loop_handle
                    .insert_source(Timer::from_duration(Duration::from_millis(16)), move |_, _, state| {
                        if let Some(surface) = state.tty.as_mut().and_then(|tty| tty.surfaces.get_mut(&crtc)) {
                            surface.retry_pending = false;
                        }
                        state.tty_render(crtc);
                        TimeoutAction::Drop
                    });
        }
    }

    /// Returns true when the caller should try again on a timer.
    fn tty_render_surface(&mut self, tty: &mut Tty, crtc: crtc::Handle) -> bool {
        let Some(surface) = tty.surfaces.get_mut(&crtc) else {
            return false;
        };
        if surface.flip_pending {
            return false;
        }
        let output = surface.output.clone();
        let Some(geometry) = self.space.output_geometry(&output) else {
            return false;
        };
        let scale = Scale::from(output.current_scale().fractional_scale());

        let mut elements: Vec<OutputElement> = Vec::new();
        let pointer = self.seat.get_pointer().unwrap().current_location();
        if geometry.to_f64().contains(pointer) {
            let at: Point<i32, Physical> = (pointer - geometry.loc.to_f64()).to_physical(scale).to_i32_round();
            if at != tty.cursor_at {
                tty.cursor_at = at;
                tty.cursor_commit.increment();
            }
            let side = (CURSOR_SIZE as f64 * scale.x).round() as i32;
            elements.push(OutputElement::Cursor(SolidColorRenderElement::new(
                tty.cursor_id.clone(),
                Rectangle::new(at, (side, side).into()),
                tty.cursor_commit,
                [1.0, 1.0, 1.0, 1.0],
                Kind::Cursor,
            )));
        }
        match space_render_elements(&mut tty.renderer, [&self.space], &output, 1.0) {
            Ok(space) => elements.extend(space.into_iter().map(OutputElement::Space)),
            Err(_) => return true,
        }

        let rendered =
            surface
                .drm_output
                .render_frame(&mut tty.renderer, &elements, [0.1, 0.1, 0.1, 1.0], FrameFlags::DEFAULT);
        // A software renderer attaches no fence to its buffer, so the flip
        // must wait until the drawing is done. Without this the screen shows
        // a half-drawn frame.
        if let Ok(frame) = &rendered
            && let PrimaryPlaneElement::Swapchain(element) = &frame.primary_element
        {
            let _ = element.sync.wait();
        }
        let retry = match rendered {
            Ok(frame) if !frame.is_empty => match surface.drm_output.queue_frame(()) {
                Ok(()) => {
                    surface.flip_pending = true;
                    false
                }
                Err(error) => {
                    warn!(%error, "tty: could not queue the frame");
                    true
                }
            },
            Ok(_) => true,
            Err(error) => {
                warn!(%error, "tty: render failed");
                true
            }
        };
        self.send_frames(&output);
        self.after_frame();
        retry
    }

    /// Takes a client's dmabuf into the TTY renderer.
    pub fn tty_import_dmabuf(&mut self, dmabuf: &smithay::backend::allocator::dmabuf::Dmabuf) -> Option<bool> {
        let tty = self.tty.as_mut()?;
        Some(tty.renderer.import_dmabuf(dmabuf, None).is_ok())
    }
}
