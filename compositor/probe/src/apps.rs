//! Apps that copy, paste, drag, and drop text, for the desktop check to drive
//! as the person would, and for the compositor's tests.
//!
//! Each opens one window, maximized, with an app ID of its own and a fill of
//! its own colour, and says what happens in short lines:
//!
//! | App | App ID | Lines |
//! | --- | --- | --- |
//! | [`Role::Copy`] | `org.crowbaros.ProbeCopy` | `focused`, `copied` |
//! | [`Role::Paste`] | `org.crowbaros.ProbePaste` | `focused`, `pasted clipboard=TEXT`, `pasted primary=TEXT` |
//! | [`Role::Drag`] | `org.crowbaros.ProbeDrag` | `focused`, `drag started`, `drag finished`, `drag cancelled` |
//! | [`Role::Drop`] | `org.crowbaros.ProbeDrop` | `focused`, `dropped TEXT` |
//!
//! `focused` comes each time the window gains the keyboard. The copier then
//! sets the clipboard and the primary selection to its text, and serves it
//! until it ends. The paster then reads whichever of the two it is offered.
//! The dragger starts a drag of its text when a button is pressed on its
//! window. The dropper takes a text offer that comes over its window, and
//! reads it when it is dropped.

use std::{
    collections::HashMap,
    io::{Read, Write},
    os::fd::OwnedFd,
    time::{Duration, Instant},
};

use fulcrum_wire::{
    Arg, ObjectId,
    protocol::{
        primary_selection_unstable_v1::{
            zwp_primary_selection_device_manager_v1, zwp_primary_selection_device_v1, zwp_primary_selection_offer_v1,
            zwp_primary_selection_source_v1,
        },
        wayland::{wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_pointer, wl_seat},
    },
};

use crate::{Client, Error, Event, PATIENCE, Toplevel};

/// The MIME types the apps offer and look for, the first preferred.
pub const TEXT_TYPES: [&str; 2] = ["text/plain;charset=utf-8", "text/plain"];

/// The size a window asks for if the compositor leaves it the choice.
const FALLBACK_SIZE: (i32, i32) = (640, 480);

/// What an app does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Role {
    /// Copies its text to the clipboard and the primary selection.
    Copy(String),
    /// Pastes from both.
    Paste,
    /// Drags its text.
    Drag(String),
    /// Takes a drop.
    Drop,
}

impl Role {
    /// The window's app ID.
    pub fn app_id(&self) -> &'static str {
        match self {
            Role::Copy(_) => "org.crowbaros.ProbeCopy",
            Role::Paste => "org.crowbaros.ProbePaste",
            Role::Drag(_) => "org.crowbaros.ProbeDrag",
            Role::Drop => "org.crowbaros.ProbeDrop",
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Role::Copy(_) => "crowbar-probe copy",
            Role::Paste => "crowbar-probe paste",
            Role::Drag(_) => "crowbar-probe drag",
            Role::Drop => "crowbar-probe drop",
        }
    }

    fn fill(&self) -> u32 {
        match self {
            Role::Copy(_) => 0xff2e_7d32,
            Role::Paste => 0xff15_65c0,
            Role::Drag(_) => 0xffef_6c00,
            Role::Drop => 0xff6a_1b9a,
        }
    }

    fn text(&self) -> &str {
        match self {
            Role::Copy(text) | Role::Drag(text) => text,
            Role::Paste | Role::Drop => "",
        }
    }
}

/// An offer the compositor made, with the MIME types it has said so far.
#[derive(Default)]
struct Offer {
    mime_types: Vec<String>,
}

impl Offer {
    fn text_type(&self) -> Option<&'static str> {
        TEXT_TYPES
            .into_iter()
            .find(|wanted| self.mime_types.iter().any(|mime| mime == wanted))
    }
}

/// One app: its connection, its window, and what it holds.
pub struct App {
    client: Client,
    role: Role,
    window: Toplevel,
    data_manager: u32,
    data_device: u32,
    primary_manager: u32,
    primary_device: u32,
    /// Data offers by object, and primary selection offers by object.
    offers: HashMap<u32, Offer>,
    primary_offers: HashMap<u32, Offer>,
    selection: Option<u32>,
    primary_selection: Option<u32>,
    /// The data sources and primary sources this app made and still holds.
    sources: Vec<u32>,
    primary_sources: Vec<u32>,
    /// The drag offer over the window, for the dropper.
    drag_offer: Option<u32>,
}

impl App {
    /// Maps the app's window, maximized, on a client already connected.
    pub fn open(mut client: Client, role: Role) -> Result<Self, Error> {
        let seat = client.bind(wl_seat::NAME, 7)?;
        let data_manager = client.bind(wl_data_device_manager::NAME, 3)?;
        let data_device = client.new_id(wl_data_device::NAME);
        client.request(
            data_manager,
            wl_data_device_manager::Request::GetDataDevice {
                id: data_device,
                seat: ObjectId(seat),
            },
        )?;
        let primary_manager = client.bind(zwp_primary_selection_device_manager_v1::NAME, 1)?;
        let primary_device = client.new_id(zwp_primary_selection_device_v1::NAME);
        client.request(
            primary_manager,
            zwp_primary_selection_device_manager_v1::Request::GetDevice {
                id: primary_device,
                seat: ObjectId(seat),
            },
        )?;
        client.listen_to_keyboard()?;
        if matches!(role, Role::Drag(_)) {
            client.listen_to_pointer()?;
        }
        let window = client.open_toplevel_maximized(role.title(), role.app_id(), FALLBACK_SIZE, role.fill(), true)?;
        Ok(App {
            client,
            role,
            window,
            data_manager,
            data_device: data_device.0,
            primary_manager,
            primary_device: primary_device.0,
            offers: HashMap::new(),
            primary_offers: HashMap::new(),
            selection: None,
            primary_selection: None,
            sources: Vec::new(),
            primary_sources: Vec::new(),
            drag_offer: None,
        })
    }

    /// Handles what arrives until the compositor closes the connection,
    /// giving each line to `say`.
    pub fn run(&mut self, say: &mut dyn FnMut(&str)) -> Result<(), Error> {
        loop {
            if !self.step(Duration::from_secs(1), say)? {
                return Ok(());
            }
        }
    }

    /// Waits at most `timeout` for something to arrive, and handles it.
    /// Returns whether the connection is still open.
    pub fn step(&mut self, timeout: Duration, say: &mut dyn FnMut(&str)) -> Result<bool, Error> {
        self.client.wait_for_events(timeout)?;
        if self.client.is_closed() {
            return Ok(false);
        }
        while let Some(event) = self.client.events.pop_front() {
            self.handle(event, say)?;
        }
        Ok(true)
    }

    fn handle(&mut self, event: Event, say: &mut dyn FnMut(&str)) -> Result<(), Error> {
        match (event.interface, event.name, event.args) {
            // The compositor gave the window a new size, as when it is
            // snapped: draw it at that size.
            ("xdg_surface", "configure", _) if event.object == self.window.xdg_surface => {
                self.client.redraw(&self.window, FALLBACK_SIZE, self.role.fill())?;
            }
            ("wl_keyboard", "enter", args) => {
                let [Arg::Uint(serial), Arg::Object(Some(surface)), ..] = args.as_slice() else {
                    return Ok(());
                };
                if surface.0 != self.window.surface {
                    return Ok(());
                }
                say("focused");
                match self.role {
                    Role::Copy(_) => self.copy(*serial, say)?,
                    Role::Paste => self.paste(say)?,
                    Role::Drag(_) | Role::Drop => {}
                }
            }
            ("wl_pointer", "button", args) => {
                if let [Arg::Uint(serial), _, _, Arg::Uint(state)] = args.as_slice()
                    && *state == wl_pointer::button_state::PRESSED
                    && matches!(self.role, Role::Drag(_))
                {
                    self.start_drag(*serial, say)?;
                }
            }
            ("wl_data_device", "data_offer", args) => {
                if let [Arg::NewId(offer)] = args.as_slice() {
                    self.offers.insert(offer.0, Offer::default());
                }
            }
            ("wl_data_offer", "offer", args) => {
                if let ([Arg::String(Some(mime))], Some(offer)) = (args.as_slice(), self.offers.get_mut(&event.object))
                {
                    offer.mime_types.push(mime.clone());
                }
            }
            ("wl_data_device", "selection", args) => {
                let new = match args.as_slice() {
                    [Arg::Object(offer)] => offer.map(|offer| offer.0),
                    _ => None,
                };
                if let Some(old) = std::mem::replace(&mut self.selection, new)
                    && Some(old) != new
                {
                    self.destroy_offer(old)?;
                }
            }
            ("wl_data_device", "enter", args) => {
                let [Arg::Uint(serial), _, _, _, Arg::Object(offer)] = args.as_slice() else {
                    return Ok(());
                };
                self.drag_offer = offer.map(|offer| offer.0);
                let Some(offer) = self.drag_offer else {
                    return Ok(());
                };
                let mime_type = self.offers.get(&offer).and_then(Offer::text_type);
                self.client.request(
                    offer,
                    wl_data_offer::Request::Accept {
                        serial: *serial,
                        mime_type: mime_type.map(str::to_owned),
                    },
                )?;
                if mime_type.is_some() {
                    let copy = wl_data_device_manager::dnd_action::COPY;
                    self.client.request(
                        offer,
                        wl_data_offer::Request::SetActions {
                            dnd_actions: copy | wl_data_device_manager::dnd_action::MOVE,
                            preferred_action: copy,
                        },
                    )?;
                }
            }
            ("wl_data_device", "leave", _) => {
                if let Some(offer) = self.drag_offer.take() {
                    self.destroy_offer(offer)?;
                }
            }
            ("wl_data_device", "drop", _) => {
                if let Some(offer) = self.drag_offer.take() {
                    if let Some(mime_type) = self.offers.get(&offer).and_then(Offer::text_type) {
                        let text = self.receive(offer, mime_type, false)?;
                        say(&format!("dropped {text}"));
                        self.client.request(offer, wl_data_offer::Request::Finish)?;
                    }
                    self.destroy_offer(offer)?;
                }
            }
            ("zwp_primary_selection_device_v1", "data_offer", args) => {
                if let [Arg::NewId(offer)] = args.as_slice() {
                    self.primary_offers.insert(offer.0, Offer::default());
                }
            }
            ("zwp_primary_selection_offer_v1", "offer", args) => {
                if let ([Arg::String(Some(mime))], Some(offer)) =
                    (args.as_slice(), self.primary_offers.get_mut(&event.object))
                {
                    offer.mime_types.push(mime.clone());
                }
            }
            ("zwp_primary_selection_device_v1", "selection", args) => {
                let new = match args.as_slice() {
                    [Arg::Object(offer)] => offer.map(|offer| offer.0),
                    _ => None,
                };
                if let Some(old) = std::mem::replace(&mut self.primary_selection, new)
                    && Some(old) != new
                {
                    self.primary_offers.remove(&old);
                    self.client
                        .request(old, zwp_primary_selection_offer_v1::Request::Destroy)?;
                }
            }
            ("wl_data_source", "send", mut args) | ("zwp_primary_selection_source_v1", "send", mut args) => {
                if let Some(Arg::Fd(fd)) = args.pop() {
                    // A reader that went away is no harm to this app.
                    let _ = std::fs::File::from(fd).write_all(self.role.text().as_bytes());
                }
            }
            ("wl_data_source", "dnd_finished", _) => {
                say("drag finished");
                self.destroy_source(event.object)?;
            }
            ("wl_data_source", "cancelled", _) => {
                if matches!(self.role, Role::Drag(_)) {
                    say("drag cancelled");
                }
                self.destroy_source(event.object)?;
            }
            ("zwp_primary_selection_source_v1", "cancelled", _) => {
                self.primary_sources.retain(|source| *source != event.object);
                self.client
                    .request(event.object, zwp_primary_selection_source_v1::Request::Destroy)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn data_source(&mut self) -> Result<u32, Error> {
        let source = self.client.new_id(wl_data_source::NAME);
        self.client.request(
            self.data_manager,
            wl_data_device_manager::Request::CreateDataSource { id: source },
        )?;
        for mime_type in TEXT_TYPES {
            self.client.request(
                source.0,
                wl_data_source::Request::Offer {
                    mime_type: mime_type.into(),
                },
            )?;
        }
        self.sources.push(source.0);
        Ok(source.0)
    }

    fn copy(&mut self, serial: u32, say: &mut dyn FnMut(&str)) -> Result<(), Error> {
        let source = self.data_source()?;
        self.client.request(
            self.data_device,
            wl_data_device::Request::SetSelection {
                source: Some(ObjectId(source)),
                serial,
            },
        )?;
        let primary = self.client.new_id(zwp_primary_selection_source_v1::NAME);
        self.client.request(
            self.primary_manager,
            zwp_primary_selection_device_manager_v1::Request::CreateSource { id: primary },
        )?;
        for mime_type in TEXT_TYPES {
            self.client.request(
                primary.0,
                zwp_primary_selection_source_v1::Request::Offer {
                    mime_type: mime_type.into(),
                },
            )?;
        }
        self.primary_sources.push(primary.0);
        self.client.request(
            self.primary_device,
            zwp_primary_selection_device_v1::Request::SetSelection {
                source: Some(ObjectId(primary.0)),
                serial,
            },
        )?;
        self.client.roundtrip()?;
        say("copied");
        Ok(())
    }

    fn paste(&mut self, say: &mut dyn FnMut(&str)) -> Result<(), Error> {
        if let Some(offer) = self.selection
            && let Some(mime_type) = self.offers.get(&offer).and_then(Offer::text_type)
        {
            let text = self.receive(offer, mime_type, false)?;
            say(&format!("pasted clipboard={text}"));
        }
        if let Some(offer) = self.primary_selection
            && let Some(mime_type) = self.primary_offers.get(&offer).and_then(Offer::text_type)
        {
            let text = self.receive(offer, mime_type, true)?;
            say(&format!("pasted primary={text}"));
        }
        Ok(())
    }

    fn start_drag(&mut self, serial: u32, say: &mut dyn FnMut(&str)) -> Result<(), Error> {
        let source = self.data_source()?;
        self.client.request(
            source,
            wl_data_source::Request::SetActions {
                dnd_actions: wl_data_device_manager::dnd_action::COPY | wl_data_device_manager::dnd_action::MOVE,
            },
        )?;
        self.client.request(
            self.data_device,
            wl_data_device::Request::StartDrag {
                source: Some(ObjectId(source)),
                origin: ObjectId(self.window.surface),
                icon: None,
                serial,
            },
        )?;
        self.client.roundtrip()?;
        say("drag started");
        Ok(())
    }

    /// Asks for an offer's data and reads it. The app that offered it is
    /// another process, which writes it and closes its end.
    fn receive(&mut self, offer: u32, mime_type: &str, primary: bool) -> Result<String, Error> {
        let (read_end, write_end) = std::io::pipe()?;
        let fd: OwnedFd = write_end.into();
        if primary {
            self.client.request(
                offer,
                zwp_primary_selection_offer_v1::Request::Receive {
                    mime_type: mime_type.into(),
                    fd,
                },
            )?;
        } else {
            self.client.request(
                offer,
                wl_data_offer::Request::Receive {
                    mime_type: mime_type.into(),
                    fd,
                },
            )?;
        }
        read_text(read_end)
    }

    fn destroy_offer(&mut self, offer: u32) -> Result<(), Error> {
        if self.offers.remove(&offer).is_some() {
            self.client.request(offer, wl_data_offer::Request::Destroy)?;
        }
        Ok(())
    }

    fn destroy_source(&mut self, source: u32) -> Result<(), Error> {
        if let Some(index) = self.sources.iter().position(|held| *held == source) {
            self.sources.remove(index);
            self.client.request(source, wl_data_source::Request::Destroy)?;
        }
        Ok(())
    }
}

/// Reads a pipe to its end, for at most [`PATIENCE`].
fn read_text(mut reader: std::io::PipeReader) -> Result<String, Error> {
    let deadline = Instant::now() + PATIENCE;
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err("the app that offered the data did not send it".into());
        }
        let mut fds = [rustix::event::PollFd::new(&reader, rustix::event::PollFlags::IN)];
        let timeout = rustix::event::Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: i64::from(left.subsec_nanos()) as _,
        };
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) | Err(rustix::io::Errno::INTR) => continue,
            Ok(_) => {}
            Err(errno) => return Err(errno.into()),
        }
        match reader.read(&mut chunk)? {
            0 => break,
            read => bytes.extend_from_slice(&chunk[..read]),
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
