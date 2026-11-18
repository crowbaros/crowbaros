//! The TTY backend: the compositor on a virtual terminal, with the seat from
//! seatd, displays through the kernel's mode setting, and input through
//! libinput.
//!
//! Each connected monitor gets a CRTC, its primary plane, and two buffers.
//! The compositor draws an output into the buffer the display is not
//! showing, and an atomic commit flips to it; the kernel's flip event ends
//! the frame's wait.
//!
//! The buffers are the GPU's where it can: GBM buffers the display scans out,
//! drawn by the GLES renderer. Where GBM or EGL is missing, they are dumb
//! buffers drawn by the CPU renderer. `FULCRUM_RENDERER=cpu` asks for the CPU
//! renderer. Where the GPU cannot draw into a buffer one display shows, that
//! display gets dumb buffers too, and the GPU draws its frame to the side
//! for the CPU to copy: the GPU alone can show the dma-bufs it imported. The
//! compositor draws the pointer itself (see [`draw_pointer`]): a TTY has no
//! cursor of its own.
//!
//! The GPU draws into a GBM buffer only what changed since the frame that
//! buffer holds (see `damage.rs`). After a switch back, it draws all of it.
//!
//! Where the display has a cursor plane, the compositor's own pointer image
//! goes on it, above the frame, so that moving the pointer only moves the
//! plane and redraws no frame. A client's cursor surface is still drawn with
//! the frame, so that it covers no frame (ADR 0020), as is an image larger
//! than the plane takes. A display that refuses the plane has the pointer
//! drawn with the frame from then on.
//!
//! When the person switches to another virtual terminal, seatd takes the
//! devices back. The compositor stops drawing and reading input until it is
//! switched back to, and then sets every mode again.

use std::{
    cell::RefCell,
    io,
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    rc::Rc,
};

use fulcrum_sys::{
    drm::{
        Change, Device, DumbBuffer, FORMAT_ARGB8888, FORMAT_XRGB8888, ModeInfo, OBJECT_CONNECTOR, OBJECT_CRTC,
        OBJECT_PLANE,
    },
    gbm,
    gles::{Dmabuf, DmabufPlane, Gles, Target},
    libinput::{Input, InputEvent, Opener},
};

use crate::{
    State,
    backend::{
        Backend, Drawn,
        session::{Session, SessionEvent},
        uevent::Uevents,
    },
    cursor::PointerImage,
    damage::DamageTracker,
    geometry::Rect,
    log,
    render::Renderer,
    xcursor::CursorImage,
};

const SOURCE_SESSION: u64 = 1;
const SOURCE_DRM: u64 = 2;
const SOURCE_INPUT: u64 = 3;
const SOURCE_UEVENT: u64 = 4;

// evdev codes the backend reads itself.
const KEY_LEFTCTRL: u32 = 29;
const KEY_RIGHTCTRL: u32 = 97;
const KEY_LEFTALT: u32 = 56;
const KEY_RIGHTALT: u32 = 100;
const KEY_F1: u32 = 59;
const KEY_F10: u32 = 68;
const KEY_F11: u32 = 87;
const KEY_F12: u32 = 88;

/// The property IDs a monitor's commits set.
#[derive(Clone, Copy, Debug, Default)]
struct Props {
    crtc_mode: u32,
    crtc_active: u32,
    connector_crtc: u32,
    plane_fb: u32,
    plane_crtc: u32,
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
    crtc_x: u32,
    crtc_y: u32,
    crtc_w: u32,
    crtc_h: u32,
}

struct Monitor {
    output: u64,
    name: String,
    connector: u32,
    crtc: u32,
    plane: u32,
    mode: ModeInfo,
    mode_blob: u32,
    props: Props,
    buffers: Buffers,
    /// Which buffer the display shows.
    front: usize,
    /// The next commit must set the mode, as after a switch back.
    needs_modeset: bool,
    flip_pending: bool,
    /// What each frame changed, for the GPU to draw only that.
    damage: DamageTracker,
    /// The plane the pointer is shown on, where the display has one.
    cursor: Option<CursorPlane>,
    /// How many frames have been drawn.
    frames: u64,
    /// The modes the monitor can show.
    modes: Vec<ModeInfo>,
    /// The buffers of the mode it showed before, until a commit of the new
    /// one has replaced them on the display.
    retired: Option<Buffers>,
}

/// An image the cursor plane shows, and where its corner is.
type CursorShown = (Rc<CursorImage>, (i32, i32));

/// The plane the display shows the pointer on, above the frame: moving the
/// pointer moves the plane, and redraws no frame.
struct CursorPlane {
    plane: u32,
    fb: u32,
    crtc: u32,
    src: [u32; 4],
    dst: [u32; 4],
    /// `HOTSPOT_X` and `HOTSPOT_Y`, which a virtual GPU's cursor plane has:
    /// its host draws the cursor, and needs the point that is the pointer.
    hotspot: Option<[u32; 2]>,
    /// The size of buffer it takes.
    size: (u32, u32),
    buffers: [DumbBuffer; 2],
    /// Which buffer it shows.
    front: usize,
    /// What it shows: none while it is off.
    showing: Option<CursorShown>,
}

impl CursorPlane {
    /// Whether it already shows what is wanted.
    fn shows(&self, wanted: Option<&CursorShown>) -> bool {
        match (&self.showing, wanted) {
            (None, None) => true,
            (Some((image, at)), Some((wanted, wanted_at))) => Rc::ptr_eq(image, wanted) && at == wanted_at,
            _ => false,
        }
    }

    /// The changes that make it show what is wanted, on `crtc`. A new image
    /// is written into the buffer it does not show: true if one was.
    fn changes(&mut self, crtc: u32, wanted: Option<&CursorShown>, changes: &mut Vec<Change>) -> bool {
        let Some((image, at)) = wanted else {
            changes.extend([(self.plane, self.fb, 0), (self.plane, self.crtc, 0)]);
            return false;
        };
        let same_image = self
            .showing
            .as_ref()
            .is_some_and(|(showing, _)| Rc::ptr_eq(showing, image));
        let mut buffer = self.front;
        if !same_image {
            buffer = 1 - self.front;
            let (width, height) = self.size;
            let mut pixels = vec![0u32; (width * height) as usize];
            let picture = &image.image;
            for row in 0..picture.height.min(height as i32) {
                let from = (row * picture.width) as usize;
                let to = row as usize * width as usize;
                let count = picture.width.min(width as i32) as usize;
                pixels[to..to + count].copy_from_slice(&picture.pixels[from..from + count]);
            }
            self.buffers[buffer].write(&pixels, width);
        }
        let (width, height) = (u64::from(self.size.0), u64::from(self.size.1));
        // Plane positions are signed: one off the top or the left is negative.
        let position = |value: i32| i64::from(value) as u64;
        changes.extend([
            (self.plane, self.fb, u64::from(self.buffers[buffer].framebuffer)),
            (self.plane, self.crtc, u64::from(crtc)),
            (self.plane, self.src[0], 0),
            (self.plane, self.src[1], 0),
            (self.plane, self.src[2], width << 16),
            (self.plane, self.src[3], height << 16),
            (self.plane, self.dst[0], position(at.0)),
            (self.plane, self.dst[1], position(at.1)),
            (self.plane, self.dst[2], width),
            (self.plane, self.dst[3], height),
        ]);
        if let Some([x, y]) = self.hotspot {
            changes.extend([
                (self.plane, x, image.hotspot.0.max(0) as u64),
                (self.plane, y, image.hotspot.1.max(0) as u64),
            ]);
        }
        !same_image
    }
}

/// A monitor's two buffers: the one it shows, and the one drawn next.
enum Buffers {
    /// Drawn by the CPU renderer.
    Dumb([DumbBuffer; 2]),
    /// Drawn by the GLES renderer.
    Gpu([GpuBuffer; 2]),
}

/// Lets go of a monitor's buffers and their framebuffers.
fn release(device: &Device, buffers: Buffers) {
    match buffers {
        Buffers::Dumb([first, second]) => {
            device.destroy(first);
            device.destroy(second);
        }
        Buffers::Gpu(buffers) => {
            for buffer in buffers {
                device.remove_framebuffer(buffer.framebuffer);
            }
        }
    }
}

/// A GBM buffer the display scans out, and the target the GPU draws it as.
struct GpuBuffer {
    // The target is dropped before the buffer it draws into.
    target: Target,
    _buffer: gbm::Buffer,
    framebuffer: u32,
    /// The frame it holds, by the monitor's count: none until drawn.
    holds: Option<u64>,
}

/// The GPU, where it draws the monitors' frames.
struct Gpu {
    gbm: gbm::Device,
    renderer: Renderer,
}

impl Gpu {
    fn new(device: &Device) -> io::Result<Gpu> {
        let gbm = gbm::Device::new(device.fd().try_clone_to_owned()?)?;
        let gles = Gles::on_gbm(&gbm)?;
        Ok(Gpu {
            gbm,
            renderer: Renderer::new(gles),
        })
    }

    /// Two buffers the display can show and the GPU can draw into.
    fn buffers(&self, device: &Device, width: u32, height: u32) -> io::Result<[GpuBuffer; 2]> {
        let make = || -> io::Result<GpuBuffer> {
            let buffer = self.gbm.create_buffer(
                width,
                height,
                FORMAT_XRGB8888,
                &[],
                gbm::USE_SCANOUT | gbm::USE_RENDERING,
            )?;
            let planes = buffer.planes();
            let dmabuf = Dmabuf {
                width: width as i32,
                height: height as i32,
                format: FORMAT_XRGB8888,
                modifier: buffer.modifier(),
                planes: planes
                    .iter()
                    .enumerate()
                    .map(|(index, plane)| {
                        Ok(DmabufPlane {
                            fd: buffer.export(index)?,
                            offset: plane.offset,
                            stride: plane.stride,
                        })
                    })
                    .collect::<io::Result<_>>()?,
            };
            let target = self.renderer.gles().target(&dmabuf)?;
            let modifier =
                (device.supports_modifiers() && buffer.modifier() != gbm::MODIFIER_INVALID).then(|| buffer.modifier());
            let layout: Vec<(u32, u32, u32)> = planes
                .iter()
                .map(|plane| (plane.handle, plane.stride, plane.offset))
                .collect();
            let framebuffer = device.add_framebuffer(width, height, FORMAT_XRGB8888, &layout, modifier)?;
            Ok(GpuBuffer {
                target,
                _buffer: buffer,
                framebuffer,
                holds: None,
            })
        };
        let first = make()?;
        match make() {
            Ok(second) => Ok([first, second]),
            Err(error) => {
                device.remove_framebuffer(first.framebuffer);
                Err(error)
            }
        }
    }
}

/// Opens libinput's devices through the seat.
struct SeatOpener(Rc<RefCell<Session>>);

impl Opener for SeatOpener {
    fn open(&mut self, path: &Path, _flags: i32) -> Result<OwnedFd, i32> {
        self.0
            .borrow_mut()
            .open_device(path)
            .map_err(|error| error.raw_os_error().unwrap_or(rustix::io::Errno::NODEV.raw_os_error()))
    }

    fn close(&mut self, fd: OwnedFd) {
        self.0.borrow_mut().close_device(fd);
    }
}

pub struct Tty {
    session: Rc<RefCell<Session>>,
    device: Device,
    device_path: PathBuf,
    monitors: Vec<Monitor>,
    input: Input,
    uevents: Uevents,
    /// Ctrl and Alt held, for the switch to another virtual terminal.
    modifiers: Vec<u32>,
    /// The GPU, if it draws the frames.
    gpu: Option<Gpu>,
}

/// Every `/dev/dri/card*`, in order.
fn cards() -> Vec<PathBuf> {
    let mut cards: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("card"))
                })
                .collect()
        })
        .unwrap_or_default();
    cards.sort();
    cards
}

/// Every `/dev/input/event*`, in order.
fn input_devices() -> Vec<PathBuf> {
    let mut devices: Vec<PathBuf> = std::fs::read_dir("/dev/input")
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .filter(|path| {
                    path.file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with("event"))
                })
                .collect()
        })
        .unwrap_or_default();
    devices.sort();
    devices
}

/// Starts the TTY backend: opens the seat, the first DRM device that has a
/// monitor, and every input device, and lights each connected monitor.
pub fn init(state: &mut State) -> io::Result<()> {
    let session = Rc::new(RefCell::new(Session::open()?));
    log::info!("session opened through seatd seat={}", session.borrow().seat);

    let mut chosen = None;
    for path in cards() {
        let fd = match session.borrow_mut().open_device(&path) {
            Ok(fd) => fd,
            Err(error) => {
                log::warning!("could not open {}: {error}", path.display());
                continue;
            }
        };
        match Device::new(fd) {
            Ok(device) => {
                let has_monitor = device.resources().is_ok_and(|(_, connectors)| {
                    connectors
                        .iter()
                        .any(|id| device.connector(*id).is_ok_and(|connector| connector.connected))
                });
                if has_monitor || chosen.is_none() {
                    chosen = Some((path, device));
                }
                if has_monitor {
                    break;
                }
            }
            Err(error) => log::warning!("{} is not a display device: {error}", path.display()),
        }
    }
    let (device_path, device) = chosen.ok_or_else(|| io::Error::other("no DRM device with dumb buffers"))?;
    log::info!("DRM device opened path={}", device_path.display());

    let mut input = Input::new(Box::new(SeatOpener(session.clone())))?;
    for path in input_devices() {
        if !input.add_device(&path) {
            log::warning!("libinput did not take {}", path.display());
        }
    }
    let uevents = Uevents::open()?;
    let gpu = if std::env::var_os("FULCRUM_RENDERER").is_some_and(|renderer| renderer == "cpu") {
        log::info!("renderer=cpu reason=FULCRUM_RENDERER");
        None
    } else {
        match Gpu::new(&device) {
            Ok(gpu) => {
                log::info!("renderer=gles gpu={}", gpu.renderer.gles().renderer());
                Some(gpu)
            }
            Err(error) => {
                log::info!("renderer=cpu reason={error}");
                None
            }
        }
    };

    let mut tty = Tty {
        session,
        device,
        device_path,
        monitors: Vec::new(),
        input,
        uevents,
        modifiers: Vec::new(),
        gpu,
    };
    tty.scan(state);
    // Clients allocate their dma-bufs on the GPU's render node where EGL
    // names it, or else on the display's device.
    let render_node = tty
        .gpu
        .as_ref()
        .and_then(|gpu| gpu.renderer.gles().render_node())
        .and_then(|path| rustix::fs::stat(&path).ok());
    // The GPU imports clients' dma-bufs where it draws the monitors; or else
    // the CPU copies them.
    match render_node.or_else(|| rustix::fs::fstat(tty.device.fd()).ok()) {
        Some(stat) => match &tty.gpu {
            Some(gpu) => state.offer_dmabuf_on_gpu(stat.st_rdev, gpu.renderer.gles().clone()),
            None => state.offer_dmabuf(stat.st_rdev),
        },
        None => log::warning!("no linux-dmabuf: the DRM device could not be named"),
    }
    // Clients' timelines are imported on the display's device, whose
    // syncobjs every driver on it shares.
    match tty.device.fd().try_clone_to_owned() {
        Ok(device) => state.offer_explicit_sync(device),
        Err(error) => log::warning!("no explicit sync: {error}"),
    }
    state.watch(tty.session.borrow().fd(), SOURCE_SESSION)?;
    state.watch(tty.device.fd(), SOURCE_DRM)?;
    state.watch(tty.input.fd(), SOURCE_INPUT)?;
    state.watch(tty.uevents.fd(), SOURCE_UEVENT)?;
    state.set_backend(tty);
    Ok(())
}

impl Tty {
    /// Lights every connected monitor that is not lit, and lets go of every
    /// one that is gone.
    fn scan(&mut self, state: &mut State) {
        let Ok((crtcs, connectors)) = self.device.resources() else {
            return;
        };
        let connected: Vec<_> = connectors
            .iter()
            .filter_map(|id| self.device.connector(*id).ok())
            .filter(|connector| connector.connected && !connector.modes.is_empty())
            .collect();
        // Monitors whose connector is gone.
        let gone: Vec<usize> = self
            .monitors
            .iter()
            .enumerate()
            .filter(|(_, monitor)| !connected.iter().any(|connector| connector.id == monitor.connector))
            .map(|(index, _)| index)
            .rev()
            .collect();
        for index in gone {
            let monitor = self.monitors.remove(index);
            log::info!("output disconnected name={}", monitor.name);
            self.turn_off(&monitor);
            state.remove_output(monitor.output);
            release(&self.device, monitor.buffers);
            if let Some(retired) = monitor.retired {
                release(&self.device, retired);
            }
            if let Some(cursor) = monitor.cursor {
                for buffer in cursor.buffers {
                    self.device.destroy(buffer);
                }
            }
        }
        let planes = self.device.planes().unwrap_or_default();
        for connector in connected {
            if self.monitors.iter().any(|monitor| monitor.connector == connector.id) {
                continue;
            }
            let used: Vec<u32> = self.monitors.iter().map(|monitor| monitor.crtc).collect();
            let possible = connector
                .encoders
                .iter()
                .filter_map(|encoder| self.device.encoder_crtcs(*encoder).ok())
                .fold(0, |mask, crtcs| mask | crtcs);
            let Some((index, crtc)) = crtcs
                .iter()
                .copied()
                .enumerate()
                .find(|(index, crtc)| possible & (1 << index) != 0 && !used.contains(crtc))
            else {
                log::warning!("no CRTC is free for {}", connector.name());
                continue;
            };
            let primary = planes.iter().find(|plane| {
                plane.possible_crtcs & (1 << index) != 0
                    && self
                        .device
                        .properties(plane.id, OBJECT_PLANE)
                        .is_ok_and(|props| props.iter().any(|(name, _, value)| name == "type" && *value == 1))
            });
            let Some(plane) = primary.copied() else {
                log::warning!("no primary plane for {}", connector.name());
                continue;
            };
            let taken: Vec<u32> = self
                .monitors
                .iter()
                .filter_map(|monitor| monitor.cursor.as_ref().map(|cursor| cursor.plane))
                .collect();
            let cursor = planes
                .iter()
                .find(|plane| {
                    plane.possible_crtcs & (1 << index) != 0
                        && !taken.contains(&plane.id)
                        && self
                            .device
                            .properties(plane.id, OBJECT_PLANE)
                            .is_ok_and(|props| props.iter().any(|(name, _, value)| name == "type" && *value == 2))
                })
                .map(|plane| plane.id);
            // The monitor's identity keys its layout: the mode the person
            // chose for it, or else the one it prefers.
            let identity = self
                .device
                .edid(connector.id)
                .and_then(|edid| crate::edid::identity(&edid))
                .unwrap_or_else(|| connector.name());
            let saved = state.saved_mode(&identity).and_then(|(width, height, refresh)| {
                connector.modes.iter().find(|mode| {
                    (i32::from(mode.hdisplay), i32::from(mode.vdisplay)) == (width, height)
                        && (refresh == 0 || mode.refresh_millihertz() == refresh)
                })
            });
            let mode = *saved
                .or_else(|| connector.modes.iter().find(|mode| mode.preferred()))
                .unwrap_or(&connector.modes[0]);
            match self.light(state, &connector, crtc, (plane.id, cursor), mode, &identity) {
                Ok(monitor) => self.monitors.push(monitor),
                Err(error) => log::warning!("could not light {}: {error}", connector.name()),
            }
        }
    }

    fn light(
        &mut self,
        state: &mut State,
        connector: &fulcrum_sys::drm::Connector,
        crtc: u32,
        (plane, cursor): (u32, Option<u32>),
        mode: ModeInfo,
        identity: &str,
    ) -> io::Result<Monitor> {
        let find = |object, kind, name: &str| -> io::Result<u32> {
            self.device
                .properties(object, kind)?
                .into_iter()
                .find(|(property, _, _)| property == name)
                .map(|(_, id, _)| id)
                .ok_or_else(|| io::Error::other(format!("no {name} property")))
        };
        let props = Props {
            crtc_mode: find(crtc, OBJECT_CRTC, "MODE_ID")?,
            crtc_active: find(crtc, OBJECT_CRTC, "ACTIVE")?,
            connector_crtc: find(connector.id, OBJECT_CONNECTOR, "CRTC_ID")?,
            plane_fb: find(plane, OBJECT_PLANE, "FB_ID")?,
            plane_crtc: find(plane, OBJECT_PLANE, "CRTC_ID")?,
            src_x: find(plane, OBJECT_PLANE, "SRC_X")?,
            src_y: find(plane, OBJECT_PLANE, "SRC_Y")?,
            src_w: find(plane, OBJECT_PLANE, "SRC_W")?,
            src_h: find(plane, OBJECT_PLANE, "SRC_H")?,
            crtc_x: find(plane, OBJECT_PLANE, "CRTC_X")?,
            crtc_y: find(plane, OBJECT_PLANE, "CRTC_Y")?,
            crtc_w: find(plane, OBJECT_PLANE, "CRTC_W")?,
            crtc_h: find(plane, OBJECT_PLANE, "CRTC_H")?,
        };
        let (width, height) = (u32::from(mode.hdisplay), u32::from(mode.vdisplay));
        let gpu_buffers = self.gpu.as_ref().map(|gpu| gpu.buffers(&self.device, width, height));
        let buffers = match gpu_buffers {
            Some(Ok(buffers)) => Buffers::Gpu(buffers),
            other => {
                if let Some(Err(error)) = other {
                    log::warning!("the GPU cannot draw {}, so the CPU does: {error}", connector.name());
                }
                Buffers::Dumb([
                    self.device.dumb_buffer(width, height)?,
                    self.device.dumb_buffer(width, height)?,
                ])
            }
        };
        let cursor = match (cursor, &buffers) {
            (Some(id), Buffers::Gpu(_)) => match self.cursor_plane(id) {
                Ok(plane) => Some(plane),
                Err(error) => {
                    log::warning!("the pointer is drawn with the frame on {}: {error}", connector.name());
                    None
                }
            },
            _ => None,
        };
        let mode_blob = self.device.mode_blob(&mode)?;
        let name = connector.name();
        let refresh = mode.refresh_millihertz();
        let output = state.add_output(&name, (width as i32, height as i32), refresh);
        let modes = connector
            .modes
            .iter()
            .map(|mode| {
                (
                    i32::from(mode.hdisplay),
                    i32::from(mode.vdisplay),
                    mode.refresh_millihertz(),
                )
            })
            .collect();
        state.identify_output(output, identity, modes);
        let pointer = if cursor.is_some() { "cursor-plane" } else { "frame" };
        log::info!("output connected name={name} size={width}x{height} refresh={refresh} pointer={pointer}");
        Ok(Monitor {
            output,
            name,
            connector: connector.id,
            crtc,
            plane,
            mode,
            mode_blob,
            props,
            buffers,
            front: 1,
            needs_modeset: true,
            flip_pending: false,
            damage: DamageTracker::default(),
            frames: 0,
            cursor,
            modes: connector.modes.clone(),
            retired: None,
        })
    }

    /// A cursor plane's properties and two buffers of the size it takes.
    fn cursor_plane(&self, plane: u32) -> io::Result<CursorPlane> {
        let find = |name: &str| -> io::Result<u32> {
            self.device
                .properties(plane, OBJECT_PLANE)?
                .into_iter()
                .find(|(property, _, _)| property == name)
                .map(|(_, id, _)| id)
                .ok_or_else(|| io::Error::other(format!("no {name} property")))
        };
        let size = self.device.cursor_size();
        Ok(CursorPlane {
            plane,
            fb: find("FB_ID")?,
            crtc: find("CRTC_ID")?,
            src: [find("SRC_X")?, find("SRC_Y")?, find("SRC_W")?, find("SRC_H")?],
            dst: [find("CRTC_X")?, find("CRTC_Y")?, find("CRTC_W")?, find("CRTC_H")?],
            hotspot: find("HOTSPOT_X").and_then(|x| Ok([x, find("HOTSPOT_Y")?])).ok(),
            size,
            buffers: [
                self.device.dumb_buffer_in(size.0, size.1, FORMAT_ARGB8888)?,
                self.device.dumb_buffer_in(size.0, size.1, FORMAT_ARGB8888)?,
            ],
            front: 0,
            showing: None,
        })
    }

    fn turn_off(&self, monitor: &Monitor) {
        let mut changes: Vec<Change> = vec![
            (monitor.connector, monitor.props.connector_crtc, 0),
            (monitor.crtc, monitor.props.crtc_active, 0),
            (monitor.crtc, monitor.props.crtc_mode, 0),
            (monitor.plane, monitor.props.plane_fb, 0),
            (monitor.plane, monitor.props.plane_crtc, 0),
        ];
        // A plane may not stay on a CRTC that is off.
        if let Some(cursor) = &monitor.cursor {
            changes.extend([(cursor.plane, cursor.fb, 0), (cursor.plane, cursor.crtc, 0)]);
        }
        if let Err(error) = self.device.commit(&changes, true, false) {
            log::warning!("could not turn {} off: {error}", monitor.name);
        }
    }

    fn handle_input(&mut self, state: &mut State, event: InputEvent) {
        match event {
            InputEvent::DeviceAdded { sysname, name, .. } => {
                log::info!("input device added name={name} sysname={sysname}");
            }
            InputEvent::DeviceRemoved { sysname } => log::info!("input device removed sysname={sysname}"),
            InputEvent::Key { key, pressed, time, .. } => {
                if pressed {
                    self.modifiers.push(key);
                } else {
                    self.modifiers.retain(|held| *held != key);
                }
                let control = self
                    .modifiers
                    .iter()
                    .any(|key| [KEY_LEFTCTRL, KEY_RIGHTCTRL].contains(key));
                let alt = self
                    .modifiers
                    .iter()
                    .any(|key| [KEY_LEFTALT, KEY_RIGHTALT].contains(key));
                let terminal = match key {
                    KEY_F1..=KEY_F10 => Some(key - KEY_F1 + 1),
                    KEY_F11 => Some(11),
                    KEY_F12 => Some(12),
                    _ => None,
                };
                if let (true, true, Some(terminal), true) = (control, alt, terminal, pressed) {
                    log::info!("switching to virtual terminal {terminal}");
                    if let Err(error) = self.session.borrow_mut().switch_session(terminal as i32) {
                        log::warning!("could not switch: {error}");
                    }
                    return;
                }
                state.handle_key(key, pressed, time);
            }
            InputEvent::Motion { time, dx, dy } => state.pointer_moved_by(dx, dy, time),
            InputEvent::MotionAbsolute { time, x, y } => {
                let bounds = state
                    .outputs()
                    .iter()
                    .map(crate::Output::rect)
                    .reduce(|a, b| a.union(&b))
                    .unwrap_or(Rect::new(0, 0, 1, 1));
                state.pointer_moved_at(
                    f64::from(bounds.x) + x * f64::from(bounds.width),
                    f64::from(bounds.y) + y * f64::from(bounds.height),
                    time,
                );
            }
            InputEvent::Button { time, button, pressed } => state.pointer_button(button, pressed, time),
            InputEvent::Scroll {
                time,
                horizontal,
                vertical,
            } => state.pointer_axis(horizontal, vertical, time),
        }
    }

    fn session_event(&mut self, state: &mut State, event: SessionEvent) {
        match event {
            SessionEvent::Disabled => {
                log::info!("the session is paused: another virtual terminal is showing");
                self.input.suspend();
                for monitor in &mut self.monitors {
                    monitor.needs_modeset = true;
                    // What the display shows when it comes back is not known.
                    if let Some(cursor) = &mut monitor.cursor {
                        cursor.showing = None;
                    }
                    if std::mem::take(&mut monitor.flip_pending) {
                        state.frame_shown(monitor.output);
                    }
                }
                if let Err(error) = self.session.borrow_mut().acknowledge_disable() {
                    log::warning!("could not tell seatd the session is paused: {error}");
                }
            }
            SessionEvent::Enabled => {
                log::info!("the session is back");
                self.session.borrow_mut().active = true;
                self.input.resume();
                for monitor in &mut self.monitors {
                    monitor.needs_modeset = true;
                }
                self.scan(state);
                state.queue_redraw_all();
            }
        }
    }
}

impl Backend for Tty {
    fn draw(&mut self, state: &State, output: u64) -> Drawn {
        if !self.session.borrow().active {
            return Drawn::Nothing;
        }
        let Some(monitor) = self.monitors.iter_mut().find(|monitor| monitor.output == output) else {
            return Drawn::Nothing;
        };
        if monitor.flip_pending {
            return Drawn::Nothing;
        }
        let back = 1 - monitor.front;
        // The pointer is drawn too: a TTY does not draw one for the
        // compositor. The compositor's own image goes on the cursor plane
        // where there is one, so that moving it redraws no frame; a client's
        // surface is drawn with the frame, so that it covers no frame.
        let pointer = state.pointer_image(output);
        let on_plane: Option<Option<CursorShown>> = match (&monitor.cursor, pointer, &monitor.buffers) {
            (Some(plane), PointerImage::Own { cursor, at }, Buffers::Gpu(_))
                if cursor.image.width as u32 <= plane.size.0 && cursor.image.height as u32 <= plane.size.1 =>
            {
                Some(Some((cursor, at)))
            }
            (Some(_), PointerImage::None, Buffers::Gpu(_)) => Some(None),
            _ => None,
        };
        let framebuffer = match &mut monitor.buffers {
            Buffers::Dumb(buffers) => {
                // Where there is a GPU, it draws what only it can show, as a
                // client's dma-buf, and the CPU copies its frame.
                let image = match self.gpu.as_mut() {
                    Some(gpu) => state
                        .scene_with_cursor(output)
                        .and_then(|scene| gpu.renderer.draw_to_image(state, &scene).ok()),
                    None => state.render_output_with_cursor(output),
                };
                let Some(image) = image else {
                    return Drawn::Nothing;
                };
                buffers[back].write(&image.pixels, image.width as u32);
                Some(buffers[back].framebuffer)
            }
            Buffers::Gpu(buffers) => {
                let scene = if on_plane.is_some() {
                    state.scene(output)
                } else {
                    state.scene_with_cursor(output)
                };
                let (Some(scene), Some(gpu)) = (scene, self.gpu.as_mut()) else {
                    return Drawn::Nothing;
                };
                if monitor.needs_modeset || monitor.damage.changes(state, &scene) {
                    // The back buffer holds an older frame: only what changed
                    // since is drawn. After a switch back, all of it is.
                    let buffer = &mut buffers[back];
                    let age = match buffer.holds {
                        Some(held) if !monitor.needs_modeset => (monitor.frames + 1 - held) as usize,
                        _ => 0,
                    };
                    let damage = monitor.damage.damage(state, &scene, age);
                    gpu.renderer.draw(state, &scene, &buffer.target, &damage);
                    monitor.frames += 1;
                    buffer.holds = Some(monitor.frames);
                    Some(buffer.framebuffer)
                } else {
                    None
                }
            }
        };
        let (width, height) = (u64::from(monitor.mode.hdisplay), u64::from(monitor.mode.vdisplay));
        let props = monitor.props;
        let mut changes: Vec<Change> = Vec::new();
        if let Some(framebuffer) = framebuffer {
            changes.push((monitor.plane, props.plane_fb, u64::from(framebuffer)));
        }
        if monitor.needs_modeset {
            changes.extend([
                (monitor.crtc, props.crtc_mode, u64::from(monitor.mode_blob)),
                (monitor.crtc, props.crtc_active, 1),
                (monitor.connector, props.connector_crtc, u64::from(monitor.crtc)),
                (monitor.plane, props.plane_crtc, u64::from(monitor.crtc)),
                (monitor.plane, props.src_x, 0),
                (monitor.plane, props.src_y, 0),
                (monitor.plane, props.src_w, width << 16),
                (monitor.plane, props.src_h, height << 16),
                (monitor.plane, props.crtc_x, 0),
                (monitor.plane, props.crtc_y, 0),
                (monitor.plane, props.crtc_w, width),
                (monitor.plane, props.crtc_h, height),
            ]);
        }
        // The cursor plane shows what is wanted of it: the compositor's own
        // image, or nothing while the pointer is elsewhere or drawn with the
        // frame.
        let wanted = on_plane.flatten();
        let crtc = monitor.crtc;
        let mut wrote = false;
        if let Some(plane) = &mut monitor.cursor
            && !plane.shows(wanted.as_ref())
        {
            wrote = plane.changes(crtc, wanted.as_ref(), &mut changes);
        }
        if changes.is_empty() {
            return Drawn::Nothing;
        }
        match self.device.commit(&changes, monitor.needs_modeset, false) {
            Ok(()) => {
                if framebuffer.is_some() {
                    monitor.front = back;
                }
                // The display shows the new mode's buffer now.
                if let Some(retired) = monitor.retired.take() {
                    release(&self.device, retired);
                }
                if let Some(plane) = &mut monitor.cursor {
                    if wrote {
                        plane.front = 1 - plane.front;
                    }
                    plane.showing = wanted;
                }
                monitor.needs_modeset = false;
                monitor.flip_pending = true;
                Drawn::Flip
            }
            Err(error) => {
                log::warning!("could not show a frame on {}: {error}", monitor.name);
                // A cursor plane the display refuses is given up: the pointer
                // is drawn with the frame from now on.
                if monitor.cursor.take().is_some() {
                    log::warning!("the pointer is drawn with the frame on {}", monitor.name);
                }
                Drawn::Nothing
            }
        }
    }

    /// Shows a monitor in another of its modes: new buffers of its size,
    /// set with the next frame's commit.
    fn set_mode(&mut self, state: &mut State, output: u64, wanted: (i32, i32, i32)) -> Result<(), String> {
        let Some(index) = self.monitors.iter().position(|monitor| monitor.output == output) else {
            return Err("no such monitor".into());
        };
        let mode = self.monitors[index]
            .modes
            .iter()
            .find(|mode| {
                (
                    i32::from(mode.hdisplay),
                    i32::from(mode.vdisplay),
                    mode.refresh_millihertz(),
                ) == wanted
            })
            .copied()
            .ok_or("the monitor has no such mode")?;
        let (width, height) = (u32::from(mode.hdisplay), u32::from(mode.vdisplay));
        let buffers = match self.gpu.as_ref().map(|gpu| gpu.buffers(&self.device, width, height)) {
            Some(Ok(buffers)) => Buffers::Gpu(buffers),
            _ => Buffers::Dumb([
                self.device
                    .dumb_buffer(width, height)
                    .map_err(|error| error.to_string())?,
                self.device
                    .dumb_buffer(width, height)
                    .map_err(|error| error.to_string())?,
            ]),
        };
        let blob = self.device.mode_blob(&mode).map_err(|error| error.to_string())?;
        let monitor = &mut self.monitors[index];
        let old = std::mem::replace(&mut monitor.buffers, buffers);
        if let Some(older) = monitor.retired.replace(old) {
            release(&self.device, older);
        }
        let monitor = &mut self.monitors[index];
        monitor.mode = mode;
        monitor.mode_blob = blob;
        monitor.needs_modeset = true;
        monitor.front = 1;
        monitor.damage = DamageTracker::default();
        monitor.frames = 0;
        log::info!(
            "output mode name={} size={width}x{height} refresh={}",
            monitor.name,
            wanted.2
        );
        state.set_output_mode(output, wanted);
        Ok(())
    }

    fn ready(&mut self, state: &mut State, source: u64) {
        match source {
            SOURCE_SESSION => {
                let events = match self.session.borrow_mut().dispatch() {
                    Ok(events) => events,
                    Err(error) => {
                        log::error!("lost the seat: {error}");
                        state.stop();
                        return;
                    }
                };
                for event in events {
                    self.session_event(state, event);
                }
            }
            SOURCE_DRM => {
                let flips = self.device.read_flips().unwrap_or_default();
                for flip in flips {
                    if let Some(monitor) = self.monitors.iter_mut().find(|monitor| monitor.crtc == flip.crtc) {
                        monitor.flip_pending = false;
                        let shown = crate::presentation::Shown::flip(flip.seconds, flip.microseconds, flip.sequence);
                        state.frame_shown_at(monitor.output, shown);
                    }
                }
            }
            SOURCE_INPUT => {
                for event in self.input.dispatch() {
                    self.handle_input(state, event);
                }
            }
            SOURCE_UEVENT => {
                for event in self.uevents.read() {
                    let subsystem = event.get("SUBSYSTEM").unwrap_or_default();
                    if subsystem == "drm" && event.get("HOTPLUG") == Some("1") {
                        log::info!("udev says the DRM device changed path={}", self.device_path.display());
                        self.scan(state);
                    } else if subsystem == "input"
                        && let Some(name) = event.get("DEVNAME").filter(|name| name.starts_with("input/event"))
                    {
                        let path = Path::new("/dev").join(name);
                        let sysname = name.trim_start_matches("input/").to_string();
                        match event.action.as_str() {
                            "add" if !self.input.has_device(&sysname) => {
                                if !self.input.add_device(&path) {
                                    log::warning!("libinput did not take {}", path.display());
                                }
                            }
                            "remove" => self.input.remove_device(&sysname),
                            _ => {}
                        }
                        for event in self.input.dispatch() {
                            self.handle_input(state, event);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
