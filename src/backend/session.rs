//! The seat, from seatd: the right to use the display and the input devices,
//! and the descriptors to them.
//!
//! seatd runs as root and hands its client, the compositor, descriptors to
//! the devices of the seat, and takes them back when the person switches to
//! another virtual terminal. Fulcrum speaks seatd's protocol itself rather
//! than linking libseat: a header of an opcode and a size, then the body,
//! and each device's descriptor beside its reply (seatd's
//! `include/protocol.h`, MIT).

use std::{
    collections::{HashMap, VecDeque},
    io::{self, IoSlice, IoSliceMut},
    mem::MaybeUninit,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd},
        unix::net::UnixStream,
    },
    path::Path,
    time::Duration,
};

use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer, SendFlags};

/// Where seatd listens.
pub const SEATD_SOCKET: &str = "/run/seatd.sock";

const CLIENT_OPEN_SEAT: u16 = 1;
const CLIENT_OPEN_DEVICE: u16 = 3;
const CLIENT_CLOSE_DEVICE: u16 = 4;
const CLIENT_DISABLE_SEAT: u16 = 5;
const CLIENT_SWITCH_SESSION: u16 = 6;
const SERVER: u16 = 1 << 15;
const SERVER_SEAT_OPENED: u16 = SERVER + 1;
const SERVER_DEVICE_OPENED: u16 = SERVER + 3;
const SERVER_DEVICE_CLOSED: u16 = SERVER + 4;
const SERVER_DISABLE_SEAT: u16 = SERVER + 5;
const SERVER_ENABLE_SEAT: u16 = SERVER + 6;
const SERVER_ERROR: u16 = SERVER + 0x7fff;

/// The longest message seatd sends: a header and a seat name or a path.
const MAX_MESSAGE: usize = 4 + 2 + 256;

/// What seatd says without being asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionEvent {
    /// The person switched to this session: the devices work again.
    Enabled,
    /// The person switched away: stop using the devices, then say so.
    Disabled,
}

pub struct Session {
    stream: UnixStream,
    pub seat: String,
    pub active: bool,
    events: VecDeque<SessionEvent>,
    /// The seatd device ID of each descriptor it opened.
    devices: HashMap<RawFd, i32>,
}

struct Message {
    opcode: u16,
    body: Vec<u8>,
    fds: Vec<OwnedFd>,
}

impl Session {
    /// Connects to seatd and opens the seat.
    pub fn open() -> io::Result<Self> {
        let stream = UnixStream::connect(SEATD_SOCKET)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut session = Session {
            stream,
            seat: String::new(),
            active: false,
            events: VecDeque::new(),
            devices: HashMap::new(),
        };
        let reply = session.request(CLIENT_OPEN_SEAT, &[], SERVER_SEAT_OPENED)?;
        let length = u16::from_ne_bytes(reply.body.get(..2).ok_or_else(short)?.try_into().expect("two bytes"));
        let name = reply.body.get(2..2 + length as usize).ok_or_else(short)?;
        session.seat = String::from_utf8_lossy(name).trim_end_matches('\0').to_string();
        // A seat that is opened while its session is active starts enabled.
        while let Some(event) = session.events.pop_front() {
            session.active = event == SessionEvent::Enabled;
        }
        session.active = true;
        Ok(session)
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.stream.as_fd()
    }

    fn send(&self, opcode: u16, body: &[u8]) -> io::Result<()> {
        let mut message = Vec::with_capacity(4 + body.len());
        message.extend_from_slice(&opcode.to_ne_bytes());
        message.extend_from_slice(&(body.len() as u16).to_ne_bytes());
        message.extend_from_slice(body);
        let mut control = SendAncillaryBuffer::default();
        let sent = rustix::net::sendmsg(
            &self.stream,
            &[IoSlice::new(&message)],
            &mut control,
            SendFlags::NOSIGNAL,
        )?;
        if sent != message.len() {
            return Err(io::Error::other("a short write to seatd"));
        }
        Ok(())
    }

    /// Reads one whole message, waiting for it.
    fn read(&self) -> io::Result<Message> {
        let mut header = [0u8; 4];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let received = rustix::net::recvmsg(
            &self.stream,
            &mut [IoSliceMut::new(&mut header)],
            &mut control,
            RecvFlags::WAITALL | RecvFlags::CMSG_CLOEXEC,
        )?;
        if received.bytes == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "seatd closed the connection",
            ));
        }
        if received.bytes != 4 {
            return Err(short());
        }
        let mut fds = Vec::new();
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(received) = message {
                fds.extend(received);
            }
        }
        let opcode = u16::from_ne_bytes([header[0], header[1]]);
        let size = u16::from_ne_bytes([header[2], header[3]]) as usize;
        if size + 4 > MAX_MESSAGE {
            return Err(io::Error::other(format!("a message of {size} bytes from seatd")));
        }
        let mut body = vec![0u8; size];
        if size > 0 {
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
            let mut control = RecvAncillaryBuffer::new(&mut space);
            let received = rustix::net::recvmsg(
                &self.stream,
                &mut [IoSliceMut::new(&mut body)],
                &mut control,
                RecvFlags::WAITALL | RecvFlags::CMSG_CLOEXEC,
            )?;
            if received.bytes != size {
                return Err(short());
            }
            for message in control.drain() {
                if let RecvAncillaryMessage::ScmRights(received) = message {
                    fds.extend(received);
                }
            }
        }
        Ok(Message { opcode, body, fds })
    }

    /// Sends a request and waits for its reply. Events that arrive first are
    /// kept for [`Session::dispatch`].
    fn request(&mut self, opcode: u16, body: &[u8], reply: u16) -> io::Result<Message> {
        self.send(opcode, body)?;
        loop {
            let message = self.read()?;
            match message.opcode {
                SERVER_ENABLE_SEAT => self.events.push_back(SessionEvent::Enabled),
                SERVER_DISABLE_SEAT => self.events.push_back(SessionEvent::Disabled),
                SERVER_ERROR => {
                    let code = message
                        .body
                        .get(..4)
                        .map_or(0, |bytes| i32::from_ne_bytes(bytes.try_into().expect("four bytes")));
                    return Err(io::Error::from_raw_os_error(code));
                }
                opcode if opcode == reply => return Ok(message),
                other => {
                    return Err(io::Error::other(format!(
                        "seatd answered {other:#x} where {reply:#x} was expected"
                    )));
                }
            }
        }
    }

    /// Asks seatd for a device of the seat.
    pub fn open_device(&mut self, path: &Path) -> io::Result<OwnedFd> {
        use std::os::unix::ffi::OsStrExt;
        let mut body = Vec::new();
        let bytes = path.as_os_str().as_bytes();
        body.extend_from_slice(&(bytes.len() as u16 + 1).to_ne_bytes());
        body.extend_from_slice(bytes);
        body.push(0);
        let reply = self.request(CLIENT_OPEN_DEVICE, &body, SERVER_DEVICE_OPENED)?;
        let id = i32::from_ne_bytes(reply.body.get(..4).ok_or_else(short)?.try_into().expect("four bytes"));
        let fd = reply
            .fds
            .into_iter()
            .next()
            .ok_or_else(|| io::Error::other("seatd sent no descriptor"))?;
        self.devices.insert(fd.as_raw_fd(), id);
        Ok(fd)
    }

    /// Gives a device back to seatd.
    pub fn close_device(&mut self, fd: OwnedFd) {
        if let Some(id) = self.devices.remove(&fd.as_raw_fd()) {
            let _ = self.request(CLIENT_CLOSE_DEVICE, &id.to_ne_bytes(), SERVER_DEVICE_CLOSED);
        }
        drop(fd);
    }

    /// Says the devices are no longer used, after [`SessionEvent::Disabled`].
    pub fn acknowledge_disable(&mut self) -> io::Result<()> {
        self.active = false;
        self.send(CLIENT_DISABLE_SEAT, &[])
    }

    /// Asks seatd to switch to another session, as Ctrl+Alt+F2 does.
    pub fn switch_session(&mut self, session: i32) -> io::Result<()> {
        self.send(CLIENT_SWITCH_SESSION, &session.to_ne_bytes())
    }

    /// The events seatd has sent, read without waiting.
    pub fn dispatch(&mut self) -> io::Result<Vec<SessionEvent>> {
        let mut events: Vec<SessionEvent> = self.events.drain(..).collect();
        loop {
            let mut ready = [rustix::event::PollFd::new(&self.stream, rustix::event::PollFlags::IN)];
            let zero = rustix::event::Timespec { tv_sec: 0, tv_nsec: 0 };
            if rustix::event::poll(&mut ready, Some(&zero))? == 0 {
                break;
            }
            let message = self.read()?;
            match message.opcode {
                SERVER_ENABLE_SEAT => events.push(SessionEvent::Enabled),
                SERVER_DISABLE_SEAT => events.push(SessionEvent::Disabled),
                _ => {}
            }
        }
        for event in &events {
            if *event == SessionEvent::Enabled {
                self.active = true;
            }
        }
        Ok(events)
    }
}

fn short() -> io::Error {
    io::Error::other("a short message from seatd")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for seatd: answers one open-seat and one open-device.
    #[test]
    fn the_protocol_opens_a_seat_and_a_device_with_its_descriptor() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let seatd = std::thread::spawn(move || {
            let read = |stream: &UnixStream| {
                let mut header = [0u8; 4];
                rustix::io::read(stream, &mut header).unwrap();
                let size = u16::from_ne_bytes([header[2], header[3]]) as usize;
                let mut body = vec![0u8; size];
                if size > 0 {
                    rustix::io::read(stream, &mut body).unwrap();
                }
                (u16::from_ne_bytes([header[0], header[1]]), body)
            };
            let send = |stream: &UnixStream, opcode: u16, body: &[u8], fd: Option<BorrowedFd<'_>>| {
                let mut message = opcode.to_ne_bytes().to_vec();
                message.extend_from_slice(&(body.len() as u16).to_ne_bytes());
                message.extend_from_slice(body);
                let fds: Vec<BorrowedFd<'_>> = fd.into_iter().collect();
                let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
                let mut control = SendAncillaryBuffer::new(&mut space);
                if !fds.is_empty() {
                    control.push(rustix::net::SendAncillaryMessage::ScmRights(&fds));
                }
                rustix::net::sendmsg(stream, &[IoSlice::new(&message)], &mut control, SendFlags::empty()).unwrap();
            };
            assert_eq!(read(&theirs).0, CLIENT_OPEN_SEAT);
            send(&theirs, SERVER_ENABLE_SEAT, &[], None);
            let mut name = 6u16.to_ne_bytes().to_vec();
            name.extend_from_slice(b"seat0\0");
            send(&theirs, SERVER_SEAT_OPENED, &name, None);
            let (opcode, body) = read(&theirs);
            assert_eq!(opcode, CLIENT_OPEN_DEVICE);
            assert!(body.ends_with(b"/dev/dri/card0\0"));
            let device = rustix::pipe::pipe().unwrap().0;
            send(&theirs, SERVER_DEVICE_OPENED, &7i32.to_ne_bytes(), Some(device.as_fd()));
        });
        let mut session = Session {
            stream: ours,
            seat: String::new(),
            active: false,
            events: VecDeque::new(),
            devices: HashMap::new(),
        };
        let reply = session.request(CLIENT_OPEN_SEAT, &[], SERVER_SEAT_OPENED).unwrap();
        assert_eq!(&reply.body[2..8], b"seat0\0");
        assert_eq!(session.events.pop_front(), Some(SessionEvent::Enabled));
        let fd = session.open_device(Path::new("/dev/dri/card0")).unwrap();
        assert_eq!(session.devices.get(&fd.as_raw_fd()), Some(&7));
        seatd.join().unwrap();
    }
}
