//! The shell's control channel: how the shell learns which windows exist and
//! on which workspace, and asks the compositor to show a workspace or a
//! window.
//!
//! It is a path socket of its own, checked against the shell's user ID as
//! the shell's Wayland socket is (ADR 0016). The wire format is the spike's,
//! which the shell already speaks: one JSON object per line. Each request
//! gets one reply. The set of requests is fixed, and none runs a command or
//! loads code.
//!
//! ```text
//! -> {"op":"snapshot"}
//! <- {"reply":"snapshot","workspace":1,"active":"0x2","windows":[{"id":"0x2","app_id":"foot","title":"~","workspace":1}]}
//! -> {"op":"focus_workspace","id":2}
//! <- {"reply":"ok"}
//! -> {"op":"focus_window","id":"0x2"}
//! <- {"reply":"ok"}
//! -> {"op":"anything else"}
//! <- {"reply":"error","message":"..."}
//! ```
//!
//! A request is at most [`MAX_LINE`] bytes. A connection that sends a longer
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
};

use crate::{
    State,
    connections::peer_uid,
    event_loop::{Readiness, Token},
    json::{Value, parse_object, quote},
    log,
    surface::SurfaceId,
    workspaces::WORKSPACES,
};

/// The longest request, in bytes.
pub const MAX_LINE: usize = 4096;
/// The most reply bytes that may wait for a connection that does not read.
pub const MAX_PENDING: usize = 1024 * 1024;
/// How many control connections the shell may hold at once.
pub const MAX_CONNECTIONS: usize = 4;

pub(crate) struct ControlListener {
    listener: UnixListener,
    uid: u32,
}

pub(crate) struct ControlConnection {
    stream: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    /// The socket is watched for room to write.
    writing: bool,
}

#[derive(Default)]
pub(crate) struct Controls {
    listener: Option<ControlListener>,
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
        match fs::remove_file(socket) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(socket)?;
        fs::set_permissions(socket, fs::Permissions::from_mode(0o660))?;
        listener.set_nonblocking(true)?;
        self.event_loop.add(listener.as_fd(), Token::ControlListener, false)?;
        self.controls.listener = Some(ControlListener { listener, uid });
        Ok(())
    }

    pub(crate) fn accept_control(&mut self) {
        loop {
            let Some(control) = &self.controls.listener else {
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
                    log::warning!("refused a peer with the wrong user ID {peer} (shell control)");
                    continue;
                }
                Err(error) => {
                    log::warning!("could not read a control peer's user ID: {error}");
                    continue;
                }
            }
            if self.controls.connections.len() >= MAX_CONNECTIONS {
                log::warning!("refused a control connection: the shell holds {MAX_CONNECTIONS}");
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
            log::info!("shell control connected id={id}");
            self.controls.connections.insert(
                id,
                ControlConnection {
                    stream,
                    input: Vec::new(),
                    output: Vec::new(),
                    writing: false,
                },
            );
        }
    }

    pub(crate) fn control_ready(&mut self, id: u64, readiness: Readiness) {
        let mut lines = Vec::new();
        let mut close = readiness.hung_up && !readiness.readable;
        if let Some(connection) = self.controls.connections.get_mut(&id) {
            let mut buffer = [0u8; 4096];
            loop {
                match connection.stream.read(&mut buffer) {
                    Ok(0) => {
                        close = true;
                        break;
                    }
                    Ok(read) => connection.input.extend_from_slice(&buffer[..read]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        close = true;
                        break;
                    }
                }
                if connection.input.len() > MAX_LINE * 4 {
                    break;
                }
            }
            while let Some(end) = connection.input.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = connection.input.drain(..=end).collect();
                lines.push(line);
            }
            if connection.input.len() > MAX_LINE {
                log::warning!("closed a control connection: a request is longer than {MAX_LINE} bytes");
                close = true;
            }
        }
        for line in lines {
            // The newline is not part of the request.
            if line.len() > MAX_LINE + 1 {
                log::warning!("closed a control connection: a request is longer than {MAX_LINE} bytes");
                close = true;
                break;
            }
            let reply = match std::str::from_utf8(&line) {
                Ok(text) => self.control_request(text.trim_end()),
                Err(_) => error_reply("the request is not UTF-8"),
            };
            if let Some(connection) = self.controls.connections.get_mut(&id) {
                connection.output.extend_from_slice(reply.as_bytes());
                connection.output.push(b'\n');
            }
        }
        if !close {
            close = !self.flush_control(id);
        }
        if close {
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
        if connection.output.len() > MAX_PENDING {
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
            log::info!("shell control disconnected id={id}");
        }
    }

    /// One request, and its reply.
    fn control_request(&mut self, line: &str) -> String {
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
                    "{{\"id\":{},\"app_id\":{},\"title\":{},\"workspace\":{}}}",
                    quote(&window_id(window.surface)),
                    quote(requests.map_or("", |requests| requests.app_id.as_str())),
                    quote(requests.map_or("", |requests| requests.title.as_str())),
                    window.workspace
                )
            })
            .collect();
        format!(
            "{{\"reply\":\"snapshot\",\"workspace\":{},\"active\":{active},\"windows\":[{}]}}",
            self.workspace,
            windows.join(",")
        )
    }
}

fn ok_reply() -> String {
    "{\"reply\":\"ok\"}".to_string()
}

fn error_reply(message: &str) -> String {
    format!("{{\"reply\":\"error\",\"message\":{}}}", quote(message))
}
