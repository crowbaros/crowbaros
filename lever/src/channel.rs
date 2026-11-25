//! Framed protocol v1 handling for Lever's surface and person sockets.

use crate::frames::{self, FrameReader, MAX_FRAME};
use crate::log::{Event, Log};
use crate::service::{Broadcast, Service};
use serde_json::{Value, json};
use std::collections::{HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

/// Lever's protocol version spoken on both the surface and person sockets.
pub const PROTOCOL_VERSION: u64 = 1;
/// Lever's package version reported in `status` replies.
pub const LEVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Once this many bytes of replies wait for a peer, Lever reads no more from
/// it, and answers no more of what it has read from it, until the peer has
/// taken them. A peer that reads slowly, or not at all, is then held where it
/// is: its own writes stall, nothing it sent is lost, and the replies Lever
/// keeps for it stay within one reply of this mark.
const WAITING_MARK: usize = 1024 * 1024;
/// Events are not held back as replies are: a thread goes on working whether
/// or not a peer that watches it reads. So a peer that lets this many bytes
/// of events wait, over and above the longest one still waiting, is closed.
/// The record has everything it missed, and `resume` reads it. The longest
/// event is set aside because a peer is behind for what it has not read, not
/// for the size of what it is sent: one event longer than this reaches a
/// peer that reads, with the events that follow it.
const EVENT_BACKLOG: usize = 4 * WAITING_MARK;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Channel {
    pub peer: &'static str,
    pub whose: &'static str,
    pub uid: u32,
}

pub struct Peer {
    id: u64,
    stream: UnixStream,
    channel: Channel,
    reader: FrameReader,
    write_buf: Vec<u8>,
    /// Bytes of events queued for this peer, less every byte it has taken
    /// since: how far behind the threads it watches it has fallen. A reply it
    /// takes counts too, so a peer still reading a long reply is given that
    /// much more room.
    events_waiting: usize,
    /// The events still waiting that are longer than every event queued
    /// after them, longest first: each with its length and where it ends in
    /// all that was ever queued for this peer. The first is the longest
    /// event still waiting, and it goes when the peer has taken it.
    longest_events: VecDeque<(usize, u64)>,
    /// Bytes ever queued for this peer, and bytes it has ever taken.
    queued: u64,
    taken: u64,
    /// Requests this peer sent may be waiting, read and unanswered, for it
    /// to take the replies before them.
    held: bool,
    /// This peer has one request waiting on an asynchronous reply from the
    /// harness (such as `models` or `set-model`). Further requests from this
    /// peer stay buffered until that reply is delivered.
    waiting_reply: bool,
    /// The peer has closed its sending side. It is still owed the replies to
    /// what it sent.
    input_ended: bool,
    /// Lever found it could send this peer nothing more.
    lost: bool,
    subscriptions: HashSet<String>,
    handshaked: bool,
    closing: bool,
    closed_logged: bool,
}

impl Peer {
    pub fn new(id: u64, stream: UnixStream, channel: Channel) -> Self {
        Self {
            id,
            stream,
            channel,
            reader: FrameReader::new(MAX_FRAME),
            write_buf: Vec::new(),
            events_waiting: 0,
            longest_events: VecDeque::new(),
            queued: 0,
            taken: 0,
            held: false,
            waiting_reply: false,
            input_ended: false,
            lost: false,
            subscriptions: HashSet::new(),
            handshaked: false,
            closing: false,
            closed_logged: false,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    pub fn wants_write(&self) -> bool {
        !self.write_buf.is_empty()
    }

    /// Whether Lever reads more from this peer: not while replies wait up
    /// to the mark, not while requests it has read are unanswered, and not
    /// while a deferred reply from the harness is in flight.
    pub fn wants_read(&self) -> bool {
        !self.closing && !self.input_ended && !self.held && !self.waiting_reply && self.write_buf.len() < WAITING_MARK
    }

    /// Whether requests this peer sent wait to be answered, and it has taken
    /// enough of its replies for Lever to answer more of them.
    pub fn is_held(&self) -> bool {
        self.held && !self.closing && !self.waiting_reply && self.write_buf.len() < WAITING_MARK
    }

    pub fn is_waiting_reply(&self) -> bool {
        self.waiting_reply
    }

    /// Delivers a deferred reply from the harness to this peer and lets any
    /// further buffered requests proceed on the next pass.
    pub fn deliver_reply(&mut self, msg: &Value, subscribe: Option<String>) {
        self.waiting_reply = false;
        if let Some(thread_id) = subscribe {
            self.subscriptions.insert(thread_id);
        }
        self.queue(msg);
        if !self.closing {
            self.held = true;
        }
    }

    pub fn is_done(&self) -> bool {
        self.closing && self.write_buf.is_empty()
    }

    pub fn is_on(&self, channel: Channel) -> bool {
        self.channel == channel
    }

    pub fn is_subscribed(&self, thread_id: &str) -> bool {
        self.handshaked && !self.closing && !self.lost && self.subscriptions.contains(thread_id)
    }

    pub fn queue(&mut self, msg: &Value) {
        if !self.closing {
            let frame = frames::encode(msg);
            self.queued += frame.len() as u64;
            self.write_buf.extend_from_slice(&frame);
        }
    }

    /// Queues an event of a thread this peer watches.
    pub fn queue_event(&mut self, msg: &Value) {
        if !self.closing {
            let frame = frames::encode(msg);
            self.queued += frame.len() as u64;
            while self
                .longest_events
                .back()
                .is_some_and(|(length, _)| *length <= frame.len())
            {
                self.longest_events.pop_back();
            }
            self.longest_events.push_back((frame.len(), self.queued));
            self.events_waiting += frame.len();
            self.write_buf.extend_from_slice(&frame);
        }
    }

    /// Whether this peer reads the events Lever has for it. Logs why when it
    /// does not, and Lever then drops it.
    pub fn keeps_up(&mut self, log: &mut Log) -> bool {
        let longest = self.longest_events.front().map_or(0, |(length, _)| *length);
        if self.events_waiting <= EVENT_BACKLOG + longest {
            return true;
        }
        if !self.closed_logged {
            self.closed_logged = true;
            let _ = log.write(
                Event::Connection,
                &format!(
                    "closed, {} left more than {} MiB of events unread",
                    self.channel.peer,
                    EVENT_BACKLOG / (1024 * 1024)
                ),
            );
        }
        false
    }

    /// Lever can send this peer nothing more, and will drop it at its next
    /// turn in the loop, once what it sent has been carried out.
    pub fn lose(&mut self) {
        self.lost = true;
    }

    pub fn is_lost(&self) -> bool {
        self.lost
    }

    /// Carries out the requests a peer sent whole before it went, with
    /// nobody to answer: a message it sent is still the person's message,
    /// and a stop it sent still stops the turn. That takes in what it sent
    /// and Lever had not yet read. Not for a peer Lever closed for what it
    /// sent, whose later frames mean nothing.
    pub fn finish_requests(&mut self, service: &mut Service, log: &mut Log) -> Vec<Broadcast> {
        // A peer that has gone left no more than its socket held. One that
        // is still there, and still sending, is not read without end.
        const MOST_READS: usize = 1024;
        let mut broadcasts = Vec::new();
        let mut buf = [0u8; 8192];
        let mut reads = 0;
        while !self.closing {
            match self.reader.next_frame() {
                Ok(Some(frame)) => {
                    self.handle_frame(&frame, service, log, &mut broadcasts);
                    self.write_buf.clear();
                    continue;
                }
                Ok(None) => {}
                Err(_) => break,
            }
            if self.input_ended || reads == MOST_READS {
                break;
            }
            reads += 1;
            match self.stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => self.reader.push(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        broadcasts
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
            // Requests it sent before it closed are still answered.
            self.input_ended = true;
            if !self.held && !self.waiting_reply {
                self.closing = true;
            }
            return (self.on_writable(), Vec::new());
        }

        self.reader.push(&buf[..n]);
        let broadcasts = self.dispatch(service, log);
        (self.on_writable(), broadcasts)
    }

    /// Answers the requests read from this peer, in order, until the replies
    /// that wait for it reach the mark. The rest wait, read and unanswered,
    /// until the peer has taken replies: a peer that sends many requests and
    /// reads no reply has Lever keep one reply past the mark for it, not one
    /// for every request.
    ///
    /// It also stops at a request that has something to tell who is
    /// watching, so that is told before the next request is answered: a
    /// `resume` that follows a `send` then reads a record whose new turn its
    /// peer has been told of once, not twice.
    pub fn dispatch(&mut self, service: &mut Service, log: &mut Log) -> Vec<Broadcast> {
        let mut broadcasts = Vec::new();
        self.held = false;
        while !self.closing && !self.waiting_reply {
            if self.write_buf.len() >= WAITING_MARK {
                self.held = true;
                break;
            }
            match self.reader.next_frame() {
                Ok(Some(frame)) => {
                    self.handle_frame(&frame, service, log, &mut broadcasts);
                    if self.waiting_reply {
                        break;
                    }
                    if !broadcasts.is_empty() {
                        self.held = true;
                        break;
                    }
                }
                Ok(None) => {
                    if self.input_ended {
                        self.closing = true;
                    }
                    break;
                }
                Err(err) => self.close_with_reason(&err.to_string(), log),
            }
        }
        broadcasts
    }

    /// Flushes queued outbound bytes. Returns `false` if the peer is finished or
    /// errored and should be removed.
    pub fn on_writable(&mut self) -> bool {
        while !self.write_buf.is_empty() {
            match self.stream.write(&self.write_buf) {
                Ok(0) => return false,
                Ok(n) => {
                    self.write_buf.drain(..n);
                    self.events_waiting = self.events_waiting.saturating_sub(n);
                    self.taken += n as u64;
                    while self.longest_events.front().is_some_and(|(_, ends)| *ends <= self.taken) {
                        self.longest_events.pop_front();
                    }
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

        let outcome = service.handle_request(self.id, &value, log);
        if let Some(thread_id) = outcome.subscribe {
            self.subscriptions.insert(thread_id);
        }
        match outcome.reply {
            Some(reply) => self.queue(&reply),
            None => self.waiting_reply = true,
        }
        broadcasts.extend(outcome.broadcasts);
    }
}

fn states_version(frame: &[u8]) -> bool {
    let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(frame) else {
        return false;
    };
    map.len() == 1 && map.get("version").and_then(Value::as_u64) == Some(PROTOCOL_VERSION)
}
