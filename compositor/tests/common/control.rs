//! The peer's end of one of Fulcrum's own channels: the shell's control
//! channel, or the unlock authority's. It frames each message by hand, as
//! the README has it, so that a test never checks the compositor's framing
//! against the compositor's own code.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

use super::Compositor;

/// The frame that states the version these tests speak.
pub const VERSION: &str = r#"{"version":1}"#;

const SNAPSHOT_EVENT: &str = r#"{"event":"snapshot","#;

pub struct Control {
    pub stream: UnixStream,
}

impl Control {
    /// The shell's control channel, past the exchange of versions.
    pub fn connect(compositor: &Compositor) -> Control {
        Control::at(&compositor.dir.join("control"))
    }

    /// The channel at `path`, past the exchange of versions.
    pub fn at(path: &Path) -> Control {
        let mut control = Control::silent(path);
        control.send(VERSION);
        assert_eq!(
            control.frame().as_deref(),
            Some(VERSION),
            "the compositor stated no version"
        );
        control
    }

    /// The channel at `path`, with nothing said yet.
    pub fn silent(path: &Path) -> Control {
        let stream = UnixStream::connect(path).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        Control { stream }
    }

    /// Sends one message as one frame: its length in four bytes, the most
    /// significant first, and then the message.
    pub fn send(&mut self, message: &str) {
        let mut frame = (message.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(message.as_bytes());
        self.stream.write_all(&frame).unwrap();
    }

    /// The next frame's message. None once the compositor has closed the
    /// connection, or when no whole frame comes before the read gives up.
    pub fn frame(&mut self) -> Option<String> {
        let mut length = [0u8; 4];
        self.stream.read_exact(&mut length).ok()?;
        let mut message = vec![0u8; u32::from_be_bytes(length) as usize];
        self.stream.read_exact(&mut message).ok()?;
        Some(String::from_utf8(message).unwrap())
    }

    /// The next frame, or none within `wait`.
    pub fn frame_within(&mut self, wait: Duration) -> Option<String> {
        self.stream.set_read_timeout(Some(wait)).unwrap();
        let frame = self.frame();
        self.stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        frame
    }

    /// The next message that is not a snapshot event: the shell is sent
    /// those unasked, and passes them over as it does any event.
    pub fn message(&mut self) -> String {
        loop {
            let message = self.frame().expect("the compositor sent no frame");
            if !message.starts_with(SNAPSHOT_EVENT) {
                return message;
            }
        }
    }

    /// The next snapshot event, passing over anything else.
    pub fn snapshot_event(&mut self) -> String {
        loop {
            let message = self.frame().expect("the compositor sent no frame");
            if message.starts_with(SNAPSHOT_EVENT) {
                return message;
            }
        }
    }

    /// Sends one request and reads its reply.
    pub fn ask(&mut self, request: &str) -> String {
        self.send(request);
        self.message()
    }

    /// Whether the compositor closes the connection, having sent nothing
    /// but snapshot events first.
    pub fn closed_with_no_reply(&mut self) -> bool {
        loop {
            let mut length = [0u8; 4];
            match self.stream.read(&mut length[..1]) {
                Ok(0) => return true,
                // A reset: the compositor closed with bytes of ours unread.
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => return true,
                Err(_) => return false,
                Ok(_) => {}
            }
            if self.stream.read_exact(&mut length[1..]).is_err() {
                return true;
            }
            let mut message = vec![0u8; u32::from_be_bytes(length) as usize];
            if self.stream.read_exact(&mut message).is_err() {
                return true;
            }
            if !message.starts_with(SNAPSHOT_EVENT.as_bytes()) {
                return false;
            }
        }
    }
}
