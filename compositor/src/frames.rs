//! The framing of Fulcrum's own channels (ADR 0035): each message is a
//! frame, a length in four bytes with the most significant first, and then
//! that many bytes of JSON.
//!
//! The first frame each way states the channel's protocol version and
//! nothing else, `{"version":1}`. The peer sends its first, and Fulcrum
//! answers with its own. A peer whose first frame is anything else is
//! closed.
//!
//! Nothing here reads or writes a socket, and nothing here can panic on
//! what a peer sends.

use crate::json::{Value, parse_object};

/// The bytes of a frame's length.
const LENGTH: usize = 4;

/// One message as a frame.
pub(crate) fn frame(message: &str) -> Vec<u8> {
    let mut frame = Vec::with_capacity(LENGTH + message.len());
    // No message Fulcrum sends comes near four gigabytes: a channel is
    // closed long before that much waits for it.
    frame.extend_from_slice(&u32::try_from(message.len()).unwrap_or(u32::MAX).to_be_bytes());
    frame.extend_from_slice(message.as_bytes());
    frame
}

/// The message that states a version.
pub(crate) fn version(version: u32) -> String {
    format!("{{\"version\":{version}}}")
}

/// The version a message states, if stating one is all it does.
pub(crate) fn stated_version(message: &[u8]) -> Option<i64> {
    let object = parse_object(std::str::from_utf8(message).ok()?).ok()?;
    match (object.len(), object.get("version")) {
        (1, Some(Value::Number(version))) => Some(*version),
        _ => None,
    }
}

/// A frame whose length says it is longer than the channel takes.
pub(crate) struct TooLong(pub usize);

/// What a peer has sent that no frame has been taken from yet.
#[derive(Default)]
pub(crate) struct Unread(Vec<u8>);

impl Unread {
    pub(crate) fn extend(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    /// The next whole frame's message, or none while it is still arriving.
    /// A frame longer than `max` is refused as soon as its length is read,
    /// before any of the message is.
    pub(crate) fn take(&mut self, max: usize) -> Result<Option<Vec<u8>>, TooLong> {
        let Some(length) = self.0.first_chunk::<LENGTH>() else {
            return Ok(None);
        };
        let length = u32::from_be_bytes(*length) as usize;
        if length > max {
            return Err(TooLong(length));
        }
        if self.0.len() < LENGTH + length {
            return Ok(None);
        }
        let message = self.0[LENGTH..LENGTH + length].to_vec();
        self.0.drain(..LENGTH + length);
        Ok(Some(message))
    }
}
