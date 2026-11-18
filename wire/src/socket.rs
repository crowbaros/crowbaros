//! One end of a Wayland connection: bytes and descriptors in and out of a
//! Unix stream socket, without blocking.
//!
//! Descriptors ride beside the bytes as `SCM_RIGHTS`. The kernel attaches them
//! to the first byte of the `sendmsg` that carried them, so the receiver has
//! a message's descriptors once it has the message. Fulcrum keeps that
//! promise when sending: a descriptor goes out no later than the first byte
//! of its own message.
//!
//! Every queue is bounded. A peer that sends more descriptors than its
//! messages take, or reads nothing while its queue fills, gets a
//! [`ConnectionError`] and is disconnected.

use std::{
    collections::VecDeque,
    io::{self, IoSlice, IoSliceMut},
    mem::MaybeUninit,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::net::UnixStream,
    },
};

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer, SendAncillaryMessage, SendFlags,
};

use crate::codec::{DecodeError, HEADER_SIZE, Header, MAX_MESSAGE_SIZE};

/// The most descriptors one `sendmsg` carries, as in libwayland.
pub const MAX_FDS_PER_SEND: usize = 28;

/// The most descriptors that may wait to be taken by a message.
pub const MAX_FDS_IN: usize = 256;

/// The most bytes that may wait to be read as messages. Four whole
/// messages of the largest size.
const MAX_BYTES_IN: usize = 4 * MAX_MESSAGE_SIZE;

/// The most bytes that may wait for the peer to read them.
pub const MAX_BYTES_OUT: usize = 1024 * 1024;

#[derive(Debug)]
pub enum ConnectionError {
    Io(io::Error),
    Decode(DecodeError),
    /// More than [`MAX_FDS_IN`] descriptors waiting to be taken.
    TooManyFds,
    /// More than [`MAX_BYTES_OUT`] bytes waiting for the peer.
    PeerNotReading,
    /// The peer closed the connection.
    Closed,
}

impl std::fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectionError::Io(error) => write!(f, "{error}"),
            ConnectionError::Decode(error) => write!(f, "{error}"),
            ConnectionError::TooManyFds => write!(f, "more than {MAX_FDS_IN} descriptors waiting"),
            ConnectionError::PeerNotReading => write!(f, "more than {MAX_BYTES_OUT} bytes the peer has not read"),
            ConnectionError::Closed => f.write_str("the peer closed the connection"),
        }
    }
}

impl std::error::Error for ConnectionError {}

impl From<io::Error> for ConnectionError {
    fn from(error: io::Error) -> Self {
        ConnectionError::Io(error)
    }
}

impl From<DecodeError> for ConnectionError {
    fn from(error: DecodeError) -> Self {
        ConnectionError::Decode(error)
    }
}

/// One whole message, read.
#[derive(Debug)]
pub struct Incoming {
    pub header: Header,
    /// The message after its header.
    pub body: Vec<u8>,
}

pub struct Connection {
    stream: UnixStream,
    bytes_in: Vec<u8>,
    fds_in: VecDeque<OwnedFd>,
    bytes_out: VecDeque<u8>,
    /// Each waiting descriptor, with the position in the outgoing stream of
    /// the message that carries it.
    fds_out: VecDeque<(u64, OwnedFd)>,
    /// The position in the outgoing stream of `bytes_out[0]`.
    sent: u64,
}

impl Connection {
    pub fn new(stream: UnixStream) -> io::Result<Self> {
        stream.set_nonblocking(true)?;
        Ok(Connection {
            stream,
            bytes_in: Vec::new(),
            fds_in: VecDeque::new(),
            bytes_out: VecDeque::new(),
            fds_out: VecDeque::new(),
            sent: 0,
        })
    }

    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }

    /// Reads what has arrived, once. Returns how many bytes were read:
    /// zero when nothing was waiting. A closed connection is an error.
    pub fn fill(&mut self) -> Result<usize, ConnectionError> {
        let room = MAX_BYTES_IN - self.bytes_in.len();
        if room == 0 {
            return Ok(0);
        }
        let mut buffer = vec![0u8; room];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS_PER_SEND))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let received = rustix::net::recvmsg(
            &self.stream,
            &mut [IoSliceMut::new(&mut buffer)],
            &mut control,
            RecvFlags::DONTWAIT | RecvFlags::CMSG_CLOEXEC,
        );
        let received = match received {
            Ok(received) => received,
            Err(rustix::io::Errno::AGAIN) => return Ok(0),
            Err(rustix::io::Errno::INTR) => return Ok(0),
            Err(errno) => return Err(ConnectionError::Io(errno.into())),
        };
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(fds) = message {
                self.fds_in.extend(fds);
            }
        }
        if received.bytes == 0 {
            return Err(ConnectionError::Closed);
        }
        if self.fds_in.len() > MAX_FDS_IN {
            return Err(ConnectionError::TooManyFds);
        }
        self.bytes_in.extend_from_slice(&buffer[..received.bytes]);
        Ok(received.bytes)
    }

    /// The next whole message, if one has arrived. A header with an
    /// impossible size is refused before its body is read.
    pub fn next_message(&mut self) -> Result<Option<Incoming>, DecodeError> {
        let Some(header) = self.bytes_in.first_chunk::<HEADER_SIZE>() else {
            return Ok(None);
        };
        let header = Header::parse(header)?;
        if self.bytes_in.len() < header.size {
            return Ok(None);
        }
        let body = self.bytes_in[HEADER_SIZE..header.size].to_vec();
        self.bytes_in.drain(..header.size);
        Ok(Some(Incoming { header, body }))
    }

    /// The descriptors that have arrived and no message has taken yet.
    pub fn fds(&mut self) -> &mut VecDeque<OwnedFd> {
        &mut self.fds_in
    }

    /// Queues one message, from [`crate::ArgWriter::finish`].
    pub fn queue(&mut self, bytes: Vec<u8>, fds: Vec<OwnedFd>) -> Result<(), ConnectionError> {
        if self.bytes_out.len() + bytes.len() > MAX_BYTES_OUT {
            return Err(ConnectionError::PeerNotReading);
        }
        let at = self.sent + self.bytes_out.len() as u64;
        self.fds_out.extend(fds.into_iter().map(|fd| (at, fd)));
        self.bytes_out.extend(bytes);
        Ok(())
    }

    /// How many bytes wait for the peer.
    pub fn waiting(&self) -> usize {
        self.bytes_out.len()
    }

    /// Writes as much as the socket takes. Returns whether everything went.
    pub fn flush(&mut self) -> Result<bool, ConnectionError> {
        while !self.bytes_out.is_empty() {
            let (sent, carried) = {
                // Send at most MAX_FDS_PER_SEND descriptors, and no byte of a
                // message whose descriptors cannot ride along this time.
                let fds: Vec<BorrowedFd<'_>> = self
                    .fds_out
                    .iter()
                    .take(MAX_FDS_PER_SEND)
                    .map(|(_, fd)| fd.as_fd())
                    .collect();
                let mut limit = self.bytes_out.len();
                if let Some((at, _)) = self.fds_out.get(MAX_FDS_PER_SEND) {
                    limit = limit.min((at - self.sent) as usize);
                }
                let (front, back) = self.bytes_out.as_slices();
                let front_len = front.len().min(limit);
                let back_len = back.len().min(limit - front_len);
                let iov = [IoSlice::new(&front[..front_len]), IoSlice::new(&back[..back_len])];
                let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS_PER_SEND))];
                let mut control = SendAncillaryBuffer::new(&mut space);
                if !fds.is_empty() {
                    control.push(SendAncillaryMessage::ScmRights(&fds));
                }
                let sent = rustix::net::sendmsg(
                    &self.stream,
                    &iov,
                    &mut control,
                    SendFlags::DONTWAIT | SendFlags::NOSIGNAL,
                );
                match sent {
                    Ok(sent) => (sent, fds.len()),
                    Err(rustix::io::Errno::AGAIN) => return Ok(false),
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(errno) => return Err(ConnectionError::Io(errno.into())),
                }
            };
            if sent > 0 {
                // The descriptors went with the first byte.
                self.fds_out.drain(..carried);
            }
            self.bytes_out.drain(..sent);
            self.sent += sent as u64;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{ArgReader, ArgWriter};

    fn pair() -> (Connection, Connection) {
        let (a, b) = UnixStream::pair().unwrap();
        (Connection::new(a).unwrap(), Connection::new(b).unwrap())
    }

    fn pipe_fd() -> OwnedFd {
        rustix::pipe::pipe().unwrap().0
    }

    fn message(object: u32, fds: usize) -> (Vec<u8>, Vec<OwnedFd>) {
        let mut out = ArgWriter::default();
        out.uint(object);
        for _ in 0..fds {
            out.fd(pipe_fd());
        }
        out.finish(object, 0).unwrap()
    }

    fn receive_all(peer: &mut Connection, count: usize) -> Vec<(u32, usize)> {
        let mut seen = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while seen.len() < count {
            peer.fill().unwrap();
            while let Some(incoming) = peer.next_message().unwrap() {
                let mut fds = std::mem::take(peer.fds());
                let mut reader = ArgReader::new(&incoming.body, &mut fds);
                let value = reader.uint().unwrap();
                seen.push((value, fds.len()));
                *peer.fds() = fds;
            }
            assert!(std::time::Instant::now() < deadline, "messages did not arrive");
        }
        seen
    }

    #[test]
    fn messages_and_descriptors_arrive_together() {
        let (mut a, mut b) = pair();
        for object in 1..=3 {
            let (bytes, fds) = message(object, 1);
            a.queue(bytes, fds).unwrap();
        }
        assert!(a.flush().unwrap());
        let seen = receive_all(&mut b, 3);
        assert_eq!(seen.iter().map(|(object, _)| *object).collect::<Vec<_>>(), [1, 2, 3]);
        // Each message's descriptor had arrived by the time it was read.
        assert!(seen.iter().all(|(_, waiting)| *waiting >= 1));
    }

    #[test]
    fn more_descriptors_than_one_send_carries_are_split_by_message() {
        let (mut a, mut b) = pair();
        for object in 1..=5 {
            let (bytes, fds) = message(object, 10);
            a.queue(bytes, fds).unwrap();
        }
        assert!(a.flush().unwrap());
        let mut fds_seen = 0;
        let mut read = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while read < 5 {
            b.fill().unwrap();
            while let Some(_incoming) = b.next_message().unwrap() {
                read += 1;
                // Each message's ten descriptors are already here.
                assert!(b.fds().len() >= 10, "message {read} arrived before its descriptors");
                b.fds().drain(..10);
                fds_seen += 10;
            }
            assert!(std::time::Instant::now() < deadline);
        }
        assert_eq!(fds_seen, 50);
    }

    #[test]
    fn a_closed_peer_is_reported() {
        let (a, mut b) = pair();
        drop(a);
        assert!(matches!(b.fill(), Err(ConnectionError::Closed)));
    }

    #[test]
    fn a_peer_that_does_not_read_fills_a_bounded_queue() {
        let (mut a, _b) = pair();
        let mut refused = false;
        for object in 0..1_000_000u32 {
            let (bytes, fds) = message(object.max(1), 0);
            if a.queue(bytes, fds).is_err() {
                refused = true;
                break;
            }
            let _ = a.flush();
        }
        assert!(refused);
    }

    #[test]
    fn an_impossible_header_is_refused() {
        let (mut a, mut b) = pair();
        let mut bytes = Header {
            object: 1,
            opcode: 0,
            size: 8,
        }
        .encode()
        .to_vec();
        bytes[4..8].copy_from_slice(&(6u32 << 16).to_ne_bytes());
        a.queue(bytes, Vec::new()).unwrap();
        a.flush().unwrap();
        while b.fill().unwrap() == 0 {}
        assert!(b.next_message().is_err());
    }
}
