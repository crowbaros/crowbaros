//! The shell's control channel: how the shell learns which windows exist and
//! on which workspace, and asks the compositor to show a workspace or a
//! window.
//!
//! It is a path socket of its own, checked against the shell's user ID as
//! the shell's Wayland socket is (ADR 0032). The wire format is the spike's,
//! which the shell already speaks: one JSON object per line. Each request
//! gets one reply. The set of requests is fixed, and none runs a command or
//! loads code.
//!
//! ```text
//! -> {"op":"snapshot"}
//! <- {"reply":"snapshot","workspace":1,"active":"0x2","windows":[{"id":"0x2","app_id":"foot","title":"~","workspace":1,"attention":false}]}
//! -> {"op":"focus_workspace","id":2}
//! <- {"reply":"ok"}
//! -> {"op":"focus_window","id":"0x2"}
//! <- {"reply":"ok"}
//! -> {"op":"launching"}
//! <- {"reply":"ok"}
//! <- {"event":"launcher"}
//! -> {"op":"lock"}
//! <- {"event":"locked"}
//! <- {"reply":"ok"}
//! -> {"op":"unlock"}
//! <- {"event":"unlocked"}
//! <- {"reply":"ok"}
//! -> {"op":"outputs"}
//! <- {"reply":"outputs","outputs":[{"name":"HDMI-A-1","identity":"DEL DELL U2723QE 8GZGCP3","width":3840,"height":2160,"refresh":60000,"x":0,"y":0,"scale":150,"modes":[{"width":3840,"height":2160,"refresh":60000}]}]}
//! -> {"op":"configure_output","name":"HDMI-A-1","width":2560,"height":1440,"refresh":59951,"x":0,"y":0,"scale":120}
//! <- {"reply":"ok"}
//! -> {"op":"anything else"}
//! <- {"reply":"error","message":"..."}
//! ```
//!
//! The compositor also sends events, at any time, one object per line with
//! no reply: `launcher` when the person taps Super alone, for the shell to
//! open or close its launcher (ADR 0020's keys). `snapshot` carries what the
//! snapshot reply does, whenever that changes from what the event last told
//! the connection, so the bar shows a workspace or window as it changes, with
//! no asking:
//!
//! ```text
//! <- {"event":"snapshot","workspace":2,"active":null,"locked":false,"windows":[...]}
//! ```
//!
//! A shell connection is told it once it connects, and again after each turn
//! of the event loop in which something it shows changed. A window that opened
//! without the keyboard has `attention` in the snapshot until it gets it,
//! for the bar to show. `launching` says the person just started an app
//! from the shell. It changes nothing, since every new window takes the
//! keyboard (see `windows.rs`), and is answered so that the shell's
//! messages stay as they are. `lock` and `unlock` lock and unlock the session,
//! and the events `locked` and `unlocked` say it changed, as Super+L and
//! the idle timer change it too (see `lock.rs`). The snapshot has `locked`.
//! Where an unlock authority listens, the shell's `unlock` is refused: the
//! authority has a socket of its own, checked against its user ID, where it
//! may send `lock` and `unlock`, and nothing else, and hears no events.
//! ADR 0032 accepts these.
//!
//! `outputs` and `configure_output` are the shell's interface to the layout
//! of displays (ADR 0020): a monitor's mode, place, and scale, the scale in
//! 120ths as fractional scale counts. What `configure_output` sets is kept
//! for the monitor (see `layout.rs`); a field it leaves out stays as it is.
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
    /// The unlock authority's connection, not the shell's: it may lock and
    /// unlock, and nothing else, and hears no events.
    authority: bool,
    stream: UnixStream,
    input: Vec<u8>,
    output: Vec<u8>,
    /// The socket is watched for room to write.
    writing: bool,
    /// The snapshot it was last sent as an event.
    told: Option<String>,
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
                    stream,
                    input: Vec::new(),
                    output: Vec::new(),
                    writing: false,
                    told: None,
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
            let authority = self
                .controls
                .connections
                .get(&id)
                .is_some_and(|connection| connection.authority);
            let reply = match std::str::from_utf8(&line) {
                Ok(text) if authority => self.authority_request(text.trim_end()),
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

    /// A request on the unlock authority's connection: `lock` or `unlock`.
    fn authority_request(&mut self, line: &str) -> String {
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

    /// Sends an event to every shell control connection: a line with no
    /// reply. The unlock authority hears none.
    pub(crate) fn control_event(&mut self, name: &str) {
        let line = format!("{{\"event\":{}}}\n", quote(name));
        let ids: Vec<u64> = self
            .controls
            .connections
            .iter()
            .filter(|(_, connection)| !connection.authority)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(connection) = self.controls.connections.get_mut(&id) {
                connection.output.extend_from_slice(line.as_bytes());
            }
            if !self.flush_control(id) {
                self.close_control(id);
            }
        }
    }

    /// Sends each shell connection the snapshot, where it changed from what
    /// the connection was last told. The event loop calls it once a turn.
    pub(crate) fn tell_shell_of_changes(&mut self) {
        if self
            .controls
            .connections
            .values()
            .all(|connection| connection.authority)
        {
            return;
        }
        let snapshot = self.snapshot_fields();
        let ids: Vec<u64> = self
            .controls
            .connections
            .iter()
            .filter(|(_, connection)| !connection.authority && connection.told.as_ref() != Some(&snapshot))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            if let Some(connection) = self.controls.connections.get_mut(&id) {
                connection
                    .output
                    .extend_from_slice(format!("{{\"event\":\"snapshot\",{snapshot}}}\n").as_bytes());
                connection.told = Some(snapshot.clone());
            }
            if !self.flush_control(id) {
                self.close_control(id);
            }
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
            "outputs" => only(&[], &request).map(|()| self.outputs_reply()),
            "lock" => only(&[], &request).map(|()| {
                self.lock("shell");
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
