//! Shared message framing: a 4-byte unsigned big-endian length prefix followed
//! by that many bytes of UTF-8 JSON.

use serde_json::Value;

/// Longest incoming frame Lever accepts (4 MiB, matching the shell).
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

/// Encodes one JSON message as a framed byte vector.
pub fn encode(message: &Value) -> Vec<u8> {
    let body = message.to_string();
    let len = u32::try_from(body.len()).expect("frame length fits in u32");
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(body.as_bytes());
    out
}

/// Framing error when a peer advertises a frame longer than the limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTooLong {
    pub length: usize,
    pub limit: usize,
}

impl std::fmt::Display for FrameTooLong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a frame of {} bytes is longer than the {} taken",
            self.length, self.limit
        )
    }
}

/// Incremental frame decoder that enforces the maximum length before buffering
/// an oversized frame.
pub struct FrameReader {
    limit: usize,
    waiting: Vec<u8>,
}

impl FrameReader {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            waiting: Vec::new(),
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.waiting.extend_from_slice(bytes);
    }

    /// Returns the next complete frame payload, `Ok(None)` if more bytes are
    /// needed, or `Err(FrameTooLong)` as soon as a header exceeds `limit`.
    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FrameTooLong> {
        if self.waiting.len() < 4 {
            return Ok(None);
        }
        let length = u32::from_be_bytes([self.waiting[0], self.waiting[1], self.waiting[2], self.waiting[3]]) as usize;
        if length > self.limit {
            return Err(FrameTooLong {
                length,
                limit: self.limit,
            });
        }
        if self.waiting.len() < 4 + length {
            return Ok(None);
        }
        let payload = self.waiting[4..4 + length].to_vec();
        self.waiting.drain(..4 + length);
        Ok(Some(payload))
    }
}
