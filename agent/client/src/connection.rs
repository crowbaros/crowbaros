//! One connection to the Harness's person socket, and everything the client
//! waits on at once: the Harness, the person's typing, and Ctrl-C.
//!
//! The Harness's framing (`agent/README.md`): a message is a frame, its
//! length in four bytes, the most significant first, and then that many bytes
//! of JSON. The first frame each way states the protocol version and nothing
//! else, and the client states it first.

use crate::sys;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// The version of the person socket's protocol this client speaks.
const VERSION: u32 = 1;
/// The longest frame the client takes from the Harness: a long conversation, resumed.
const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Where the Harness listens for the person: in their runtime directory,
/// which is `/run/user/<user ID>` where nothing says otherwise, as on a
/// console or over SSH.
pub fn socket_path() -> PathBuf {
    let runtime = match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(runtime) if !runtime.is_empty() => PathBuf::from(runtime),
        _ => PathBuf::from(format!("/run/user/{}", sys::uid())),
    };
    runtime.join("crowbar-agent/person")
}

/// A field of a message that is text, or nothing where it is not.
pub fn text<'a>(message: &'a Value, field: &str) -> &'a str {
    message.get(field).and_then(Value::as_str).unwrap_or_default()
}

/// What a client does when no Model answers: the Harness says why, and the
/// client says what to type.
pub const NO_MODEL: &str = "Sign in to one with `crowbar-agent login`, or choose one with `crowbar-agent model`.";

/// When a reply is an error, says it to the person, and gives its code.
pub fn refused(reply: &Value) -> Option<&str> {
    if text(reply, "reply") != "error" {
        return None;
    }
    let hint = if text(reply, "code") == "no-model" {
        format!(" {NO_MODEL}")
    } else {
        String::new()
    };
    eprintln!("crowbar-agent: {}{hint}", text(reply, "message"));
    Some(text(reply, "code"))
}

/// What the client waited for, and got.
pub enum Happening {
    /// An event from the Harness.
    Event(Value),
    /// A line the person typed, without its end.
    Line(String),
    /// The person's typing ended: Ctrl-D, or the end of what was piped in.
    EndOfInput,
    /// Ctrl-C.
    Interrupt,
}

pub struct Connection {
    stream: UnixStream,
    /// Bytes from the Harness that do not make a whole frame yet.
    received: Vec<u8>,
    /// Events that came while a reply was waited for.
    events: VecDeque<Value>,
    interrupts: RawFd,
    /// What the person typed that does not make a whole line yet.
    typed: Vec<u8>,
    typing_ended: bool,
}

/// One whole frame's message taken off the front of `received`, if it holds one.
fn framed(received: &mut Vec<u8>) -> Result<Option<Value>> {
    let Some(length) = received.first_chunk::<4>() else {
        return Ok(None);
    };
    let length = u32::from_be_bytes(*length) as usize;
    if length > MAX_FRAME {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "The Harness sent a frame too long to read",
        ));
    }
    if received.len() < 4 + length {
        return Ok(None);
    }
    let message = serde_json::from_slice(&received[4..4 + length])?;
    received.drain(..4 + length);
    Ok(Some(message))
}

impl Connection {
    /// Connects to the Harness and states the version. An error says why
    /// not, in words for the person.
    pub fn connect() -> std::result::Result<Connection, String> {
        let path = socket_path();
        let stream = UnixStream::connect(&path).map_err(|error| {
            format!(
                "The Harness is not running: nothing answers at {} ({error}).",
                path.display()
            )
        })?;
        let interrupts = sys::interrupts().map_err(|error| format!("Could not listen for Ctrl-C: {error}."))?;
        let mut connection = Connection {
            stream,
            received: Vec::new(),
            events: VecDeque::new(),
            interrupts,
            typed: Vec::new(),
            typing_ended: false,
        };
        let stated = json!({ "version": VERSION });
        let answered = connection.write(&stated).and_then(|()| connection.message());
        match answered {
            Ok(answer) if answer == stated => Ok(connection),
            // The Harness closes a peer it does not serve, or whose version it does not speak, and says nothing.
            _ => Err(format!(
                "The Harness at {} closed the connection: it serves another account, or speaks another version than {VERSION}.",
                path.display()
            )),
        }
    }

    fn write(&mut self, message: &Value) -> Result<()> {
        let message = message.to_string();
        let length =
            u32::try_from(message.len()).map_err(|_| Error::new(ErrorKind::InvalidInput, "too long to frame"))?;
        self.stream
            .write_all(&[&length.to_be_bytes(), message.as_bytes()].concat())
    }

    /// Reads more from the Harness. An error at its end.
    fn receive(&mut self) -> Result<()> {
        let mut bytes = [0u8; 65536];
        let count = self.stream.read(&mut bytes)?;
        if count == 0 {
            return Err(Error::new(
                ErrorKind::UnexpectedEof,
                "The Harness closed the connection",
            ));
        }
        self.received.extend_from_slice(&bytes[..count]);
        Ok(())
    }

    /// The next message from the Harness, waited for.
    fn message(&mut self) -> Result<Value> {
        loop {
            if let Some(message) = framed(&mut self.received)? {
                return Ok(message);
            }
            self.receive()?;
        }
    }

    /// Sends a request, and gives its reply. Events that come first are
    /// kept for `next`, in their order.
    pub fn request(&mut self, request: Value) -> Result<Value> {
        self.write(&request)?;
        loop {
            let message = self.message()?;
            if message.get("reply").is_some() {
                return Ok(message);
            }
            self.events.push_back(message);
        }
    }

    /// Sends a request whose reply is not waited for here: `next` gives it
    /// as an event would be given, so nothing is missed while it comes.
    pub fn send(&mut self, request: Value) -> Result<()> {
        self.write(&request)
    }

    /// Waits for the next thing to happen: an event, Ctrl-C, or, when
    /// `typing` is wanted, a line from the person. What the person types
    /// while no line is wanted waits in the terminal.
    pub fn next(&mut self, typing: bool) -> Result<Happening> {
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(Happening::Event(event));
            }
            if let Some(message) = framed(&mut self.received)? {
                return Ok(Happening::Event(message));
            }
            if typing {
                if let Some(end) = self.typed.iter().position(|&byte| byte == b'\n') {
                    let line: Vec<u8> = self.typed.drain(..=end).collect();
                    return Ok(Happening::Line(
                        String::from_utf8_lossy(&line[..end]).trim_end_matches('\r').to_string(),
                    ));
                }
                if self.typing_ended {
                    // What was typed before the end, with no line end after it, is a line all the same.
                    if self.typed.is_empty() {
                        return Ok(Happening::EndOfInput);
                    }
                    let line = std::mem::take(&mut self.typed);
                    return Ok(Happening::Line(String::from_utf8_lossy(&line).to_string()));
                }
            }
            let mut waited = vec![self.interrupts, self.stream.as_raw_fd()];
            if typing {
                waited.push(0);
            }
            let ready = sys::readable(&waited)?;
            if ready[0] {
                let mut byte = [0u8; 64];
                sys::read(self.interrupts, &mut byte)?;
                return Ok(Happening::Interrupt);
            }
            if ready[1] {
                self.receive()?;
            }
            if ready.get(2) == Some(&true) {
                let mut bytes = [0u8; 4096];
                match sys::read(0, &mut bytes)? {
                    0 => self.typing_ended = true,
                    count => self.typed.extend_from_slice(&bytes[..count]),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_taken_whole_however_its_bytes_arrive() {
        // `{"reply":"ok"}` and then `{"event":"done"}`, each after its length, written out by hand.
        let bytes = b"\x00\x00\x00\x0e{\"reply\":\"ok\"}\x00\x00\x00\x10{\"event\":\"done\"}";
        let mut received = Vec::new();
        let mut messages = Vec::new();
        for byte in bytes {
            received.push(*byte);
            while let Some(message) = framed(&mut received).unwrap() {
                messages.push(message);
            }
        }
        assert_eq!(messages, [json!({"reply": "ok"}), json!({"event": "done"})]);
        assert!(received.is_empty());
    }

    #[test]
    fn a_frame_too_long_is_an_error_as_soon_as_its_length_is_read() {
        let mut received = vec![0xff, 0xff, 0xff, 0xff];
        assert_eq!(framed(&mut received).unwrap_err().kind(), ErrorKind::InvalidData);
    }
}
