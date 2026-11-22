//! The compositor's framing, from a helper's end of one of its channels
//! (ADR 0035; `compositor/README.md`, "The control channel").
//!
//! Each message is a frame: a length in four bytes, the most significant
//! first, and then that many bytes of JSON. The first frame each way states
//! the protocol version and nothing else. The helper states it first, and
//! asks nothing until the compositor has stated the same.

use serde_json::{Value, json};
use std::io::{Error, ErrorKind, Read, Result, Write};

/// The longest message a helper reads from the compositor, in bytes.
pub const MAX_MESSAGE: usize = 4096;

/// Sends one message as one frame.
pub fn write(stream: &mut impl Write, message: &Value) -> Result<()> {
    let message = message.to_string();
    let length = u32::try_from(message.len()).map_err(|_| invalid("A message is too long to frame"))?;
    stream.write_all(&[&length.to_be_bytes(), message.as_bytes()].concat())
}

/// Reads one frame's message. A frame longer than [`MAX_MESSAGE`] is an
/// error as soon as its length is read.
pub fn read(stream: &mut impl Read) -> Result<Value> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_MESSAGE {
        return Err(invalid("The compositor sent a frame too long to read"));
    }
    let mut message = vec![0u8; length];
    stream.read_exact(&mut message)?;
    serde_json::from_slice(&message).map_err(Error::from)
}

/// States `version`, and reads the compositor's own. An error unless the
/// compositor states the same.
pub fn state_version(stream: &mut (impl Read + Write), version: u32) -> Result<()> {
    let stated = json!({ "version": version });
    write(stream, &stated)?;
    if read(stream)? != stated {
        return Err(invalid("The compositor does not speak this helper's protocol version"));
    }
    Ok(())
}

fn invalid(why: &str) -> Error {
    Error::new(ErrorKind::InvalidData, why)
}
