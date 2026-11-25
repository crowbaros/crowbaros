//! Framed protocol v1 handling for Lever's surface and person sockets.

use crate::frames::{self, FrameReader, MAX_FRAME};
use crate::log::{Event, Log};
use serde_json::{Value, json};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

/// Lever's protocol version spoken on both the surface and person sockets.
pub const PROTOCOL_VERSION: u64 = 1;
/// Lever's package version reported in `status` replies.
pub const LEVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Maximum outbound bytes queued for a single slow peer before closing it.
const MAX_WAITING: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
pub struct Channel {
    pub peer: &'static str,
    pub whose: &'static str,
    pub uid: u32,
}

pub struct Peer {
    stream: UnixStream,
    channel: Channel,
    reader: FrameReader,
    write_buf: Vec<u8>,
    handshaked: bool,
    closing: bool,
    closed_logged: bool,
}

impl Peer {
    pub fn new(stream: UnixStream, channel: Channel) -> Self {
        Self {
            stream,
            channel,
            reader: FrameReader::new(MAX_FRAME),
            write_buf: Vec::new(),
            handshaked: false,
            closing: false,
            closed_logged: false,
        }
    }

    pub fn fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    pub fn wants_write(&self) -> bool {
        !self.write_buf.is_empty()
    }

    pub fn wants_read(&self) -> bool {
        !self.closing
    }

    pub fn is_done(&self) -> bool {
        self.closing && self.write_buf.is_empty()
    }

    fn queue(&mut self, msg: &Value, log: &mut Log) {
        if self.closing {
            return;
        }
        let frame = frames::encode(msg);
        if self.write_buf.len() + frame.len() > MAX_WAITING {
            let peer = self.channel.peer;
            self.close_with_reason(
                &format!("{peer} read nothing while {MAX_WAITING} bytes waited for it"),
                log,
            );
            return;
        }
        self.write_buf.extend_from_slice(&frame);
    }

    fn close_with_reason(&mut self, why: &str, log: &mut Log) {
        if !self.closed_logged {
            self.closed_logged = true;
            let _ = log.write(Event::Connection, &format!("closed, {why}"));
        }
        self.closing = true;
    }

    /// Reads available bytes and processes every complete frame. Returns `false`
    /// if the peer should be removed immediately.
    pub fn on_readable(&mut self, log: &mut Log) -> bool {
        let mut buf = [0u8; 8192];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => {
                    if !self.closed_logged {
                        self.closed_logged = true;
                        let peer = self.channel.peer;
                        let msg = if self.handshaked {
                            format!("{peer} left")
                        } else {
                            format!("{peer} left before it stated a version")
                        };
                        let _ = log.write(Event::Connection, &msg);
                    }
                    return false;
                }
                Ok(n) => {
                    self.reader.push(&buf[..n]);
                    loop {
                        match self.reader.next_frame() {
                            Ok(Some(frame)) => {
                                if !self.handle_frame(&frame, log) {
                                    break;
                                }
                            }
                            Ok(None) => break,
                            Err(err) => {
                                self.close_with_reason(&err.to_string(), log);
                                break;
                            }
                        }
                    }
                    if self.closing {
                        // Try to flush any already-queued replies (e.g. a valid
                        // frame right before an oversized frame) before closing.
                        return self.on_writable();
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    if !self.closed_logged {
                        self.closed_logged = true;
                        let _ = log.write(Event::Connection, &format!("{} disconnected", self.channel.peer));
                    }
                    return false;
                }
            }
        }
        // Flush replies promptly when the socket is already writable.
        self.on_writable()
    }

    /// Flushes queued outbound bytes. Returns `false` if the peer is finished or
    /// errored and should be removed.
    pub fn on_writable(&mut self) -> bool {
        while !self.write_buf.is_empty() {
            match self.stream.write(&self.write_buf) {
                Ok(0) => return false,
                Ok(n) => {
                    self.write_buf.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
        !self.is_done()
    }

    fn handle_frame(&mut self, frame: &[u8], log: &mut Log) -> bool {
        if !self.handshaked {
            if !states_version(frame) {
                let peer = self.channel.peer;
                self.close_with_reason(
                    &format!("{peer}'s first frame did not state version {PROTOCOL_VERSION}"),
                    log,
                );
                return false;
            }
            self.handshaked = true;
            let _ = log.write(
                Event::Connection,
                &format!("{} stated version {PROTOCOL_VERSION}", self.channel.peer),
            );
            self.queue(&json!({ "version": PROTOCOL_VERSION }), log);
            return true;
        }

        let Ok(value) = serde_json::from_slice::<Value>(frame) else {
            let peer = self.channel.peer;
            self.close_with_reason(&format!("{peer} sent invalid JSON"), log);
            return false;
        };
        let Some(op) = value.as_object().and_then(|o| o.get("op")).and_then(Value::as_str) else {
            self.queue(
                &json!({
                    "reply": "error",
                    "code": "invalid-request",
                    "message": "Expected a JSON object with a string op field."
                }),
                log,
            );
            return true;
        };

        let reply = match op {
            "status" => json!({
                "reply": "status",
                "version": LEVER_VERSION,
                "harness_started": false,
                "harnesses": []
            }),
            _ => json!({
                "reply": "error",
                "code": "unsupported",
                "message": "This request is not supported."
            }),
        };
        self.queue(&reply, log);
        true
    }
}

fn states_version(frame: &[u8]) -> bool {
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(frame) else {
        return false;
    };
    map.len() == 1 && map.get("version").and_then(Value::as_u64) == Some(PROTOCOL_VERSION)
}
