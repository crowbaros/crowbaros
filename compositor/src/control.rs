//! The shell's control channel: how the shell learns which windows exist and
//! on which workspace, and asks the compositor to show a workspace or a
//! window. The unlock authority's socket is served here too.
//!
//! Each is a path socket of its own, checked against one user ID as the
//! shell's Wayland socket is (ADR 0032). Both speak the framing in
//! `frames.rs` (ADR 0035): each message a length and then JSON, and the
//! version stated in the first frame. The set of requests is fixed, and none
//! runs a command or loads code. The README lists every message, under "The
//! control channel".
//!
//! Each request gets one reply. The compositor also sends the shell events,
//! at any time, with no reply. The unlock authority may send `lock` and
//! `unlock`, and nothing else, and is sent no events. Where it listens, the
//! shell's `unlock` is refused. A `lock` is answered once every display
//! shows the session locked, and the connection is sent nothing in between
//! (see `lock.rs`).
//!
//! A request is at most [`MAX_FRAME`] bytes. A connection that sends a longer
//! one, or reads no replies until [`MAX_PENDING`] bytes wait for it, is
//! closed. The shell may hold [`MAX_CONNECTIONS`] at once.

use std::{
    collections::{BTreeMap, HashMap},
    fs, io,
    io::{Read, Write},
    os::{
        fd::AsFd,
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::Path,
    time::Instant,
};

use crate::{
    State,
    connections::peer_uid,
    event_loop::{Readiness, TimerId, TimerKey, Token},
    frames::{self, TooLong, Unread},
    json::{Value, parse_object, quote},
    lock::LOCK_SHOWN_WITHIN,
    log,
    surface::SurfaceId,
    workspaces::WORKSPACES,
};

/// The protocol version the control channel and the unlock authority's
/// speak. A peer states it in its first frame, and is closed if it states
/// another.
pub const VERSION: u32 = 1;
/// The longest request, in bytes, not counting its length.
pub const MAX_FRAME: usize = 4096;
/// The most reply bytes that may wait for a connection that does not read.
pub const MAX_PENDING: usize = 1024 * 1024;
/// How many control connections the shell may hold at once.
pub const MAX_CONNECTIONS: usize = 4;

pub(crate) struct ControlListener {
    listener: UnixListener,
    uid: u32,
}

pub(crate) struct ControlConnection {
    /// The unlock authority's connection, not the shell's: it may lock and
    /// unlock, and nothing else, and hears no events.
    authority: bool,
    /// It stated the version. Until it has, it is sent nothing.
    versioned: bool,
    stream: UnixStream,
    input: Unread,
    output: Vec<u8>,
    /// The socket is watched for room to write.
    writing: bool,
    /// The snapshot it was last sent as an event.
    told: Option<String>,
    /// What it is sent while its `lock` waits for the displays to show the
    /// session locked, from that `lock`'s reply on, and the timer that ends
    /// the wait (see `lock.rs`).
    held: Option<(Vec<u8>, TimerId)>,
}

impl ControlConnection {
    /// Whether it is sent events: the shell's, once it has stated the
    /// version.
    fn hears_events(&self) -> bool {
        self.versioned && !self.authority
    }
}

#[derive(Default)]
pub(crate) struct Controls {
    listener: Option<ControlListener>,
    /// The unlock authority's listener, where one is named.
    authority: Option<ControlListener>,
    connections: HashMap<u64, ControlConnection>,
    next: u64,
}

/// A window's name on the control channel.
pub fn window_id(surface: SurfaceId) -> String {
    format!("0x{surface:x}")
}

fn parse_window_id(text: &str) -> Option<SurfaceId> {
    let digits = text.strip_prefix("0x")?;
    (!digits.is_empty() && digits.len() <= 16)
        .then(|| u64::from_str_radix(digits, 16).ok())
        .flatten()
}

impl State {
    /// Listens for the shell's control channel at `socket`, from the user ID
    /// `uid` alone.
    pub fn listen_for_control(&mut self, socket: &Path, uid: u32) -> io::Result<()> {
        let listener = control_listener(socket)?;
        self.event_loop.add(listener.as_fd(), Token::ControlListener, false)?;
        self.controls.listener = Some(ControlListener { listener, uid });
        Ok(())
    }

    /// Listens for the unlock authority at `socket`, from the user ID `uid`
    /// alone. Once it does, only the authority unlocks the session: the
    /// shell's word is not enough (ADR 0020, see `lock.rs`).
    pub fn listen_for_unlock_authority(&mut self, socket: &Path, uid: u32) -> io::Result<()> {
        let listener = control_listener(socket)?;
        self.event_loop.add(listener.as_fd(), Token::AuthorityListener, false)?;
        self.controls.authority = Some(ControlListener { listener, uid });
        Ok(())
    }

    pub(crate) fn accept_control(&mut self, authority: bool) {
        let part = if authority { "unlock authority" } else { "shell control" };
        loop {
            let listening = if authority {
                &self.controls.authority
            } else {
                &self.controls.listener
            };
            let Some(control) = listening else {
                return;
            };
            let (stream, _) = match control.listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                Err(error) => {
                    log::warning!("the control channel could not accept: {error}");
                    return;
                }
            };
            match peer_uid(&stream) {
                Ok(peer) if peer == control.uid => {}
                Ok(peer) => {
                    log::warning!("refused a peer with the wrong user ID {peer} ({part})");
                    continue;
                }
                Err(error) => {
                    log::warning!("could not read a control peer's user ID: {error}");
                    continue;
                }
            }
            let held = self
                .controls
                .connections
                .values()
                .filter(|connection| connection.authority == authority)
                .count();
            if held >= MAX_CONNECTIONS {
                log::warning!("refused a {part} connection: it holds {MAX_CONNECTIONS}");
                continue;
            }
            if stream.set_nonblocking(true).is_err() {
                continue;
            }
            self.controls.next += 1;
            let id = self.controls.next;
            if self.event_loop.add(stream.as_fd(), Token::Control(id), false).is_err() {
                continue;
            }
            log::info!("{part} connected id={id}");
            self.controls.connections.insert(
                id,
                ControlConnection {
                    authority,
                    versioned: false,
                    stream,
                    input: Unread::default(),
                    output: Vec::new(),
                    writing: false,
                    told: None,
                    held: None,
                },
            );
        }
    }

    pub(crate) fn control_ready(&mut self, id: u64, readiness: Readiness) {
        let mut messages = Vec::new();
        let mut close = readiness.hung_up && !readiness.readable;
        if let Some(connection) = self.controls.connections.get_mut(&id) {
            let mut buffer = [0u8; 4096];
            loop {
                match connection.stream.read(&mut buffer) {
                    Ok(0) => {
                        close = true;
                        break;
                    }
                    Ok(read) => connection.input.extend(&buffer[..read]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        close = true;
                        break;
                    }
                }
                if connection.input.len() > MAX_FRAME * 4 {
                    break;
                }
            }
            loop {
                match connection.input.take(MAX_FRAME) {
                    Ok(Some(message)) => messages.push(message),
                    Ok(None) => break,
                    Err(TooLong(length)) => {
                        log::warning!(
                            "closed a control connection: a frame of {length} bytes is longer than {MAX_FRAME}"
                        );
                        close = true;
                        break;
                    }
                }
            }
        }
        for message in messages {
            let Some(connection) = self.controls.connections.get_mut(&id) else {
                break;
            };
            if !connection.versioned {
                // The first frame states the version, and nothing is done
                // for a peer that states another or none.
                if frames::stated_version(&message) != Some(i64::from(VERSION)) {
                    log::warning!("closed a control connection: its first frame did not state version {VERSION}");
                    close = true;
                    break;
                }
                connection.versioned = true;
                self.send_control(id, &frames::version(VERSION));
                continue;
            }
            let authority = connection.authority;
            let reply = match std::str::from_utf8(&message) {
                Ok(text) if authority => self.authority_request(id, text),
                Ok(text) => self.control_request(id, text),
                Err(_) => error_reply("the request is not UTF-8"),
            };
            self.send_control(id, &reply);
        }
        if !close {
            close = !self.flush_control(id);
        }
        if close {
            self.close_control(id);
        }
    }

    /// A request on the unlock authority's connection: `lock` or `unlock`.
    fn authority_request(&mut self, id: u64, line: &str) -> String {
        let op = match parse_object(line) {
            Ok(request) if request.len() == 1 => match request.get("op") {
                Some(Value::String(op)) => op.clone(),
                _ => return error_reply("a request names its op, and nothing else"),
            },
            Ok(_) => return error_reply("a request names its op, and nothing else"),
            Err(error) => return error_reply(&format!("not a request: {error}")),
        };
        let result = match op.as_str() {
            "lock" => {
                self.lock("authority");
                self.hold_until_lock_shown(id);
                Ok(())
            }
            "unlock" => {
                self.unlock();
                Ok(())
            }
            _ => Err(format!("the unlock authority may lock and unlock, and not {op:?}")),
        };
        result.map_or_else(|message| error_reply(&message), |()| ok_reply())
    }

    /// Sends an event to every shell control connection that has stated the
    /// version: a message with no reply. The unlock authority hears none.
    pub(crate) fn control_event(&mut self, name: &str) {
        let event = format!("{{\"event\":{}}}", quote(name));
        let ids: Vec<u64> = self
            .controls
            .connections
            .iter()
            .filter(|(_, connection)| connection.hears_events())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.send_control(id, &event);
            if !self.flush_control(id) {
                self.close_control(id);
            }
        }
    }

    /// Sends each shell connection the snapshot, where it changed from what
    /// the connection was last told. The event loop calls it once a turn.
    pub(crate) fn tell_shell_of_changes(&mut self) {
        if !self.controls.connections.values().any(ControlConnection::hears_events) {
            return;
        }
        let snapshot = self.snapshot_fields();
        let ids: Vec<u64> = self
            .controls
            .connections
            .iter()
            .filter(|(_, connection)| connection.hears_events() && connection.told.as_ref() != Some(&snapshot))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.send_control(id, &format!("{{\"event\":\"snapshot\",{snapshot}}}"));
            if let Some(connection) = self.controls.connections.get_mut(&id) {
                connection.told = Some(snapshot.clone());
            }
            if !self.flush_control(id) {
                self.close_control(id);
            }
        }
    }

    /// Queues a message for a connection, as one frame: behind its held
    /// `lock` reply, if one waits, so that what it hears keeps its order.
    fn send_control(&mut self, id: u64, message: &str) {
        if let Some(connection) = self.controls.connections.get_mut(&id) {
            let frame = frames::frame(message);
            match &mut connection.held {
                Some((held, _)) => held.extend_from_slice(&frame),
                None => connection.output.extend_from_slice(&frame),
            }
        }
    }

    /// A connection asked to lock: until every display shows the session
    /// locked, or [`LOCK_SHOWN_WITHIN`] has passed, it hears nothing more,
    /// starting with this request's reply (see `lock.rs`).
    fn hold_until_lock_shown(&mut self, id: u64) {
        if self.lock_shown() {
            return;
        }
        let Some(connection) = self.controls.connections.get(&id) else {
            return;
        };
        if connection.held.is_some() {
            return;
        }
        let timer = self
            .event_loop
            .set_timer(Instant::now() + LOCK_SHOWN_WITHIN, TimerKey::LockShown(id));
        if let Some(connection) = self.controls.connections.get_mut(&id) {
            connection.held = Some((Vec::new(), timer));
        }
    }

    /// Sends the connections that wait on a `lock` what they were held,
    /// once every display shows the session locked. The event loop calls it
    /// once a turn, after its draws.
    pub(crate) fn answer_locks_shown(&mut self) {
        let ids: Vec<u64> = self
            .controls
            .connections
            .iter()
            .filter(|(_, connection)| connection.held.is_some())
            .map(|(id, _)| *id)
            .collect();
        if ids.is_empty() || !self.lock_shown() {
            return;
        }
        for id in ids {
            self.release_control(id);
        }
    }

    /// A connection's `lock` waited [`LOCK_SHOWN_WITHIN`]: it is answered
    /// anyway, as the session is locked, though a display may still show
    /// what it showed before.
    pub(crate) fn answer_lock_anyway(&mut self, id: u64) {
        if self
            .controls
            .connections
            .get(&id)
            .is_none_or(|connection| connection.held.is_none())
        {
            return;
        }
        log::warning!(
            "answered a lock before these displays showed it, after {LOCK_SHOWN_WITHIN:?}: {}",
            self.lock_not_shown_on().join(", ")
        );
        self.release_control(id);
    }

    fn release_control(&mut self, id: u64) {
        let Some(connection) = self.controls.connections.get_mut(&id) else {
            return;
        };
        let Some((held, timer)) = connection.held.take() else {
            return;
        };
        connection.output.extend_from_slice(&held);
        self.event_loop.cancel_timer(timer);
        if !self.flush_control(id) {
            self.close_control(id);
        }
    }

    /// Writes what waits for a connection. False if it must be closed.
    fn flush_control(&mut self, id: u64) -> bool {
        let Some(connection) = self.controls.connections.get_mut(&id) else {
            return true;
        };
        while !connection.output.is_empty() {
            match connection.stream.write(&connection.output) {
                Ok(0) => return false,
                Ok(written) => {
                    connection.output.drain(..written);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        let held = connection.held.as_ref().map_or(0, |(held, _)| held.len());
        if connection.output.len() + held > MAX_PENDING {
            log::warning!("closed a control connection that reads no replies");
            return false;
        }
        let waiting = !connection.output.is_empty();
        if waiting != connection.writing {
            connection.writing = waiting;
            let _ = self
                .event_loop
                .set_writable(connection.stream.as_fd(), Token::Control(id), waiting);
        }
        true
    }

    fn close_control(&mut self, id: u64) {
        if let Some(connection) = self.controls.connections.remove(&id) {
            self.event_loop.remove(connection.stream.as_fd());
            if let Some((_, timer)) = connection.held {
                self.event_loop.cancel_timer(timer);
            }
            log::info!("shell control disconnected id={id}");
        }
    }

    /// One request, and its reply.
    fn control_request(&mut self, id: u64, line: &str) -> String {
        let request = match parse_object(line) {
            Ok(request) => request,
            Err(error) => return error_reply(&format!("not a request: {error}")),
        };
        let op = match request.get("op") {
            Some(Value::String(op)) => op.clone(),
            _ => return error_reply("a request names its op"),
        };
        let only = |fields: &[&str], request: &BTreeMap<String, Value>| -> Result<(), String> {
            match request
                .keys()
                .find(|key| *key != "op" && !fields.contains(&key.as_str()))
            {
                Some(key) => Err(format!("{op} takes no field {key:?}")),
                None => Ok(()),
            }
        };
        let result = match op.as_str() {
            "snapshot" => only(&[], &request).map(|()| self.snapshot_reply()),
            "focus_workspace" => only(&["id"], &request).and_then(|()| match request.get("id") {
                Some(Value::Number(id)) if (1..=i64::from(WORKSPACES)).contains(id) => {
                    self.show_workspace(*id as u32);
                    Ok(ok_reply())
                }
                _ => Err(format!("a workspace is a number from 1 to {WORKSPACES}")),
            }),
            "focus_window" => only(&["id"], &request).and_then(|()| {
                let surface = match request.get("id") {
                    Some(Value::String(id)) => parse_window_id(id),
                    _ => None,
                }
                .filter(|surface| self.windows.iter().any(|window| window.surface == *surface))
                .ok_or_else(|| "no such window".to_string())?;
                self.focus_window(surface);
                Ok(ok_reply())
            }),
            "outputs" => only(&[], &request).map(|()| self.outputs_reply()),
            "lock" => only(&[], &request).map(|()| {
                self.lock("shell");
                self.hold_until_lock_shown(id);
                ok_reply()
            }),
            "unlock" => only(&[], &request).and_then(|()| {
                if self.controls.authority.is_some() {
                    return Err("the shell's word is not enough: the unlock authority unlocks".into());
                }
                self.unlock();
                Ok(ok_reply())
            }),
            "launching" => only(&[], &request).map(|()| ok_reply()),
            "configure_output" => only(&["name", "width", "height", "refresh", "x", "y", "scale"], &request)
                .and_then(|()| self.configure_output(&request))
                .map(|()| ok_reply()),
            _ => Err(format!("no op {op:?}")),
        };
        result.unwrap_or_else(|message| error_reply(&message))
    }

    /// Shows a window's workspace, raises the window, and gives it the
    /// keyboard.
    pub(crate) fn focus_window(&mut self, surface: SurfaceId) {
        if let Some(workspace) = self.workspace_of(surface) {
            self.show_workspace(workspace);
        }
        self.raise(surface);
        self.set_keyboard_focus(Some(surface));
    }

    fn snapshot_reply(&self) -> String {
        format!("{{\"reply\":\"snapshot\",{}}}", self.snapshot_fields())
    }

    /// The snapshot's fields, as the reply and the event carry them.
    fn snapshot_fields(&self) -> String {
        let active = self
            .seat
            .keyboard_focus
            .map(|focus| self.root_of(focus))
            .filter(|root| {
                self.windows
                    .iter()
                    .any(|window| window.surface == *root && window.workspace == self.workspace)
            })
            .map_or_else(|| "null".to_string(), |root| quote(&window_id(root)));
        let windows: Vec<String> = self
            .windows
            .iter()
            .map(|window| {
                let requests = self.xdg.get(&window.surface).map(|xdg| &xdg.toplevel);
                format!(
                    "{{\"id\":{},\"app_id\":{},\"title\":{},\"workspace\":{},\"attention\":{}}}",
                    quote(&window_id(window.surface)),
                    quote(requests.map_or("", |requests| requests.app_id.as_str())),
                    quote(requests.map_or("", |requests| requests.title.as_str())),
                    window.workspace,
                    window.attention
                )
            })
            .collect();
        format!(
            "\"workspace\":{},\"active\":{active},\"locked\":{},\"windows\":[{}]",
            self.workspace,
            self.locked(),
            windows.join(",")
        )
    }
}

impl State {
    fn outputs_reply(&self) -> String {
        let outputs: Vec<String> = self
            .outputs
            .iter()
            .map(|output| {
                let modes: Vec<String> = output
                    .modes
                    .iter()
                    .map(|(width, height, refresh)| {
                        format!("{{\"width\":{width},\"height\":{height},\"refresh\":{refresh}}}")
                    })
                    .collect();
                format!(
                    "{{\"name\":{},\"identity\":{},\"width\":{},\"height\":{},\"refresh\":{},\"x\":{},\"y\":{},\"scale\":{},\"modes\":[{}]}}",
                    quote(&output.name),
                    quote(&output.identity),
                    output.size.0,
                    output.size.1,
                    output.refresh,
                    output.position.0,
                    output.position.1,
                    output.scale_120,
                    modes.join(",")
                )
            })
            .collect();
        format!("{{\"reply\":\"outputs\",\"outputs\":[{}]}}", outputs.join(","))
    }

    /// Sets what a request names of an output's mode, place, and scale, and
    /// keeps the output's whole placement for its monitor.
    fn configure_output(&mut self, request: &BTreeMap<String, Value>) -> Result<(), String> {
        let name = match request.get("name") {
            Some(Value::String(name)) => name.clone(),
            _ => return Err("an output is named by its name".into()),
        };
        let mut fields = request.clone();
        fields.remove("name");
        let wanted = crate::layout::placement_of(&fields)?;
        let Some(output) = self.outputs.iter().find(|output| output.name == name) else {
            return Err(format!("no output {name:?}"));
        };
        let number = output.number;
        if let Some(scale) = wanted.scale_120
            && (!(120..=360).contains(&scale) || scale % 30 != 0)
        {
            return Err(format!("a scale is from 120 to 360 in steps of 30, not {scale}"));
        }
        if let Some((width, height, refresh)) = wanted.mode {
            let known = output
                .modes
                .iter()
                .find(|(w, h, r)| (*w, *h) == (width, height) && (refresh == 0 || *r == refresh))
                .copied()
                .ok_or_else(|| format!("{name} has no mode {width}x{height} at {refresh} mHz"))?;
            if known != (output.size.0, output.size.1, output.refresh) {
                let Some(mut backend) = self.backend.take() else {
                    return Err("no backend to show the mode".into());
                };
                let result = backend.set_mode(self, number, known);
                if self.backend.is_none() {
                    self.backend = Some(backend);
                }
                result?;
            }
        }
        if let Some(scale) = wanted.scale_120 {
            self.set_output_scale(number, scale)?;
        }
        if let Some(position) = wanted.position {
            self.set_output_position(number, position);
        }
        // What is kept is the output as it now is.
        let Some(output) = self.outputs.iter().find(|output| output.number == number) else {
            return Ok(());
        };
        let identity = output.identity.clone();
        let placement = crate::layout::Placement {
            mode: Some((output.size.0, output.size.1, output.refresh)),
            position: Some(output.position),
            scale_120: Some(output.scale_120),
        };
        self.layout.set(&identity, placement);
        Ok(())
    }
}

/// A listening socket at a path, which it replaces, that the owner and the
/// group may connect to.
fn control_listener(socket: &Path) -> io::Result<UnixListener> {
    match fs::remove_file(socket) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o660))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn ok_reply() -> String {
    "{\"reply\":\"ok\"}".to_string()
}

fn error_reply(message: &str) -> String {
    format!("{{\"reply\":\"error\",\"message\":{}}}", quote(message))
}
