//! Framed protocol v1 handling for Lever's surface and person sockets.

use crate::frames::{self, FrameReader, MAX_FRAME};
use crate::log::{Event, Log};
use crate::service::{Broadcast, Service};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

/// Lever's protocol version spoken on both the surface and person sockets.
pub const PROTOCOL_VERSION: u64 = 1;
/// Lever's package version reported in `status` replies.
pub const LEVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Once this many bytes of replies wait for a peer, Lever reads no more from
/// it until it has taken them. A peer that reads slowly, or not at all, is
/// then held where it is: its own writes stall, nothing it sent is lost, and
/// what Lever keeps for it stays near this mark.
const WAITING_MARK: usize = 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
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
    subscriptions: HashSet<String>,
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
            subscriptions: HashSet::new(),
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
        !self.closing && self.write_buf.len() < WAITING_MARK
    }

    pub fn is_done(&self) -> bool {
        self.closing && self.write_buf.is_empty()
    }

    pub fn is_on(&self, channel: Channel) -> bool {
        self.channel == channel
    }

    pub fn is_subscribed(&self, thread_id: &str) -> bool {
        self.handshaked && !self.closing && self.subscriptions.contains(thread_id)
    }

    pub fn queue(&mut self, msg: &Value) {
        if !self.closing {
            self.write_buf.extend_from_slice(&frames::encode(msg));
        }
    }

    /// Lever is about to drop this peer. Logs that it left, unless its close
    /// is in the log already, so each peer that goes has one line saying so.
    pub fn on_gone(&mut self, log: &mut Log) {
        if !self.closed_logged {
            self.closed_logged = true;
            let _ = log.write(Event::Connection, &format!("{} left", self.channel.peer));
        }
    }

    fn close_with_reason(&mut self, why: &str, log: &mut Log) {
        if !self.closed_logged {
            self.closed_logged = true;
            let _ = log.write(Event::Connection, &format!("closed, {why}"));
        }
        self.closing = true;
    }

    /// Reads once and answers every complete frame that read finished, then
    /// writes what it can. Returns `(alive, broadcasts)` where `broadcasts`
    /// should be delivered to every subscribed peer.
    pub fn on_readable(&mut self, service: &mut Service, log: &mut Log) -> (bool, Vec<Broadcast>) {
        let mut buf = [0u8; 8192];
        let n = loop {
            match self.stream.read(&mut buf) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return (self.on_writable(), Vec::new()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    if !self.closed_logged {
                        self.closed_logged = true;
                        let _ = log.write(Event::Connection, &format!("{} disconnected", self.channel.peer));
                    }
                    return (false, Vec::new());
                }
            }
        };

        if n == 0 {
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
            self.closing = true;
            return (self.on_writable(), Vec::new());
        }

        let mut broadcasts = Vec::new();
        self.reader.push(&buf[..n]);
        while !self.closing {
            match self.reader.next_frame() {
                Ok(Some(frame)) => self.handle_frame(&frame, service, log, &mut broadcasts),
                Ok(None) => break,
                Err(err) => self.close_with_reason(&err.to_string(), log),
            }
        }
        (self.on_writable(), broadcasts)
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

    fn handle_frame(&mut self, frame: &[u8], service: &mut Service, log: &mut Log, broadcasts: &mut Vec<Broadcast>) {
        if !self.handshaked {
            if !states_version(frame) {
                let peer = self.channel.peer;
                self.close_with_reason(
                    &format!("{peer}'s first frame did not state version {PROTOCOL_VERSION}"),
                    log,
                );
                return;
            }
            self.handshaked = true;
            let _ = log.write(
                Event::Connection,
                &format!("{} stated version {PROTOCOL_VERSION}", self.channel.peer),
            );
            self.queue(&json!({ "version": PROTOCOL_VERSION }));
            return;
        }

        let Ok(value) = serde_json::from_slice::<Value>(frame) else {
            let peer = self.channel.peer;
            self.close_with_reason(&format!("{peer} sent invalid JSON"), log);
            return;
        };

        let outcome = service.handle_request(&value, log);
        if let Some(thread_id) = outcome.subscribe {
            self.subscriptions.insert(thread_id);
        }
        self.queue(&outcome.reply);
        broadcasts.extend(outcome.broadcasts);
    }
}

fn states_version(frame: &[u8]) -> bool {
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(frame) else {
        return false;
    };
    map.len() == 1 && map.get("version").and_then(Value::as_u64) == Some(PROTOCOL_VERSION)
}
