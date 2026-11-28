// SPDX-License-Identifier: MIT
//! Compositor control channel (Fulcrum) and Hyprland fallback behind an
//! injectable `Adapter` trait and `CompositorClient` handle — no process-global
//! `OnceLock` singletons.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::net::UnixStream,
    },
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use crate::events::{Event, MediaKey, Sender};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub id: i32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Client {
    pub address: String,
    pub class: String,
    pub title: String,
    pub workspace: Workspace,
    /// It opened without the keyboard and has not had it since: the bar
    /// shows it wants attention. Only the CrowbarOS compositor says so.
    pub attention: bool,
    /// Whether the window is fullscreen (covering the bar).
    pub fullscreen: bool,
    /// The display this window is on, as Fulcrum names it.
    pub output: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Snapshot {
    pub workspace: i32,
    pub clients: Vec<Client>,
    pub active: Client,
    pub connected: bool,
    /// The compositor locked the session: the lock screen shows.
    pub locked: bool,
}

/// The summon gesture, Super+Space, as the compositor tells of it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Summon {
    /// The display the pointer was on.
    pub output: String,
    /// That display's width on the desktop, in logical pixels.
    pub width: f32,
    /// The keyboard is in the Agent's surface.
    pub keyboard: bool,
}

/// What the shell asks of the compositor it runs under: the windows and
/// workspaces, focus, lock, clipboard history, outputs, and settings.
pub trait Adapter: Send + Sync {
    fn clipboard_start(&self) -> Result<()> {
        Ok(())
    }
    fn clipboard_history(&self, _clear: bool) -> Result<Vec<String>> {
        bail!("Clipboard history needs Fulcrum")
    }
    fn set_theme(&self, _colors: &crowbar_theme::ThemeColors) -> Result<()> {
        Ok(())
    }
    fn snapshot(&self) -> Result<Snapshot>;
    fn focus_workspace(&self, id: i32) -> Result<()>;
    fn focus_window(&self, address: &str) -> Result<()>;
    /// Says the person is launching an app, whose first window then takes
    /// the keyboard. A compositor that does not ask for it is not told.
    fn launching(&self) -> Result<()> {
        Ok(())
    }
    /// Reads the events the compositor sent while no request was waiting.
    fn pump(&self) {}
    /// Waits up to `timeout` for the compositor to send something.
    fn wait(&self, timeout: Duration) {
        std::thread::sleep(timeout);
    }
    /// The channel to the compositor is gone for good, and only a new
    /// connection, from a shell started again, would talk to it.
    fn closed(&self) -> bool {
        false
    }
    /// Asks the compositor to lock the session, or to unlock it.
    fn lock(&self, _locked: bool) -> Result<()> {
        bail!("This compositor does not lock")
    }
    /// Asks the compositor to give the Agent's surface the keyboard, or to
    /// give it back to the window that had it.
    fn agent_keyboard(&self, _to_surface: bool) -> Result<()> {
        bail!("This compositor has no Agent's surface")
    }
    /// Whether `outputs` answers. A compositor that does not is taken to
    /// have a display.
    fn reports_outputs(&self) -> bool {
        false
    }
    /// Lists connected displays and their modes, positions, and scales.
    fn outputs(&self) -> Result<serde_json::Value> {
        bail!("This compositor does not report display outputs")
    }
    /// Applies a display's mode, position, or scale.
    fn configure_output(&self, _request: serde_json::Value) -> Result<()> {
        bail!("This compositor does not configure display outputs")
    }
    /// Reads live input settings from the compositor.
    fn input_config(&self) -> Result<serde_json::Value> {
        bail!("This compositor does not report input settings")
    }
    /// Applies keyboard/pointer input settings live to the compositor.
    fn configure_input(&self, _request: serde_json::Value) -> Result<()> {
        bail!("This compositor does not configure input settings")
    }
    /// Reads live idle lock/suspend timeouts from the compositor.
    fn power_config(&self) -> Result<serde_json::Value> {
        bail!("This compositor does not report power settings")
    }
    /// Applies idle lock/suspend timeouts live to the compositor.
    fn configure_power(&self, _request: serde_json::Value) -> Result<()> {
        bail!("This compositor does not configure power settings")
    }
}

/// One event the compositor sent, passed on to the interface.
fn deliver_compositor_event(events: Option<&Sender>, message: &serde_json::Value) {
    let event = &message["event"];
    if event.as_str() != Some("snapshot") {
        log::info!("SHELL event=compositor-event name={event}");
    }
    if let (Some(tx), Some(event)) = (events, shell_event(message)) {
        let _ = tx.send(event);
    }
}

/// What an event of the compositor's is to the shell: `launcher` opens or
/// closes the launcher, as Super tapped alone asks (ADR 0032); `summon` is
/// the summon gesture, for the Agent's surface (ADR 0035); and `snapshot`
/// says what the windows and workspaces are now, so the bar shows a change
/// at once. None for an event the shell does not know, or cannot read.
pub(crate) fn shell_event(message: &serde_json::Value) -> Option<Event> {
    let unread = |what: &str, error: &dyn std::fmt::Display| {
        log::warn!("Compositor: a {what} event: {error}");
        None
    };
    match message["event"].as_str()? {
        "clipboard_changed" => Some(Event::ClipboardChanged),
        "clipboard" => Some(Event::Picker(crate::clipboard_picker::Kind::History)),
        "characters" => Some(Event::Picker(crate::clipboard_picker::Kind::Characters)),
        "launcher" => Some(Event::Toggle),
        "locked" => Some(Event::Lock(true)),
        "unlocked" => Some(Event::Lock(false)),
        "volume_up" => Some(Event::MediaKey(MediaKey::VolumeUp)),
        "volume_down" => Some(Event::MediaKey(MediaKey::VolumeDown)),
        "volume_mute" => Some(Event::MediaKey(MediaKey::VolumeMute)),
        "mic_mute" => Some(Event::MediaKey(MediaKey::MicMute)),
        "brightness_up" => Some(Event::MediaKey(MediaKey::BrightnessUp)),
        "brightness_down" => Some(Event::MediaKey(MediaKey::BrightnessDown)),
        "summon" => match serde_json::from_value(message.clone()) {
            Ok(summon) => Some(Event::Summon(summon)),
            Err(e) => unread("summon", &e),
        },
        "snapshot" => match snapshot_from(message.clone()) {
            Ok(snapshot) => Some(Event::Snapshot(snapshot)),
            Err(e) => unread("snapshot", &e),
        },
        _ => None,
    }
}

/// A snapshot, from the compositor's snapshot reply or event.
fn snapshot_from(message: serde_json::Value) -> Result<Snapshot> {
    #[derive(Deserialize)]
    struct Window {
        id: String,
        app_id: String,
        title: String,
        workspace: i32,
        #[serde(default)]
        attention: bool,
        #[serde(default)]
        fullscreen: bool,
        #[serde(default)]
        output: String,
    }
    #[derive(Deserialize)]
    struct Reply {
        workspace: i32,
        active: Option<String>,
        #[serde(default)]
        locked: bool,
        windows: Vec<Window>,
    }
    let reply: Reply = serde_json::from_value(message)?;
    let clients: Vec<Client> = reply
        .windows
        .into_iter()
        .map(|w| Client {
            address: w.id,
            class: w.app_id,
            title: w.title,
            workspace: Workspace { id: w.workspace },
            attention: w.attention,
            fullscreen: w.fullscreen,
            output: w.output,
        })
        .collect();
    let active = clients
        .iter()
        .find(|c| Some(&c.address) == reply.active.as_ref())
        .cloned()
        .unwrap_or_default();
    Ok(Snapshot {
        workspace: reply.workspace,
        clients,
        active,
        connected: true,
        locked: reply.locked,
    })
}

/// The protocol version of the compositor's control channel that the shell
/// speaks. Each end states it in its first frame.
const CONTROL_VERSION: u32 = 1;

/// One message as the compositor's control channel frames it: its length in
/// four bytes, the most significant first, and then the JSON
/// (`compositor/README.md`, "The control channel").
fn frame(message: &serde_json::Value) -> Vec<u8> {
    let message = message.to_string();
    let mut frame = (message.len() as u32).to_be_bytes().to_vec();
    frame.extend_from_slice(message.as_bytes());
    frame
}

/// The frames the compositor sends, read whole: what has arrived of a frame
/// waits here for the rest, whichever of a request or the pump reads next.
struct Frames {
    stream: UnixStream,
    pending: Vec<u8>,
    /// The compositor stated the version: its first frame, which is not
    /// handed on.
    versioned: bool,
}

impl Frames {
    /// The longest message read.
    const MAX: usize = 4 * 1024 * 1024;

    /// The next whole message that waits, if one does. An error for a frame
    /// longer than [`Self::MAX`], as soon as its length has come, and for a
    /// first frame that does not state the shell's version.
    fn take(&mut self) -> Result<Option<String>> {
        loop {
            let Some(length) = self.pending.first_chunk::<4>() else {
                return Ok(None);
            };
            let length = u32::from_be_bytes(*length) as usize;
            if length > Self::MAX {
                bail!("A frame from the compositor is too long")
            }
            if self.pending.len() < 4 + length {
                return Ok(None);
            }
            let frame: Vec<u8> = self.pending.drain(..4 + length).collect();
            let message = String::from_utf8_lossy(&frame[4..]).into_owned();
            if self.versioned {
                return Ok(Some(message));
            }
            let stated = serde_json::from_str::<serde_json::Value>(&message).ok();
            if stated != Some(serde_json::json!({ "version": CONTROL_VERSION })) {
                bail!(
                    "The compositor does not speak version {CONTROL_VERSION} of the control channel"
                )
            }
            self.versioned = true;
        }
    }

    /// The next message, as long as the stream's read timeout waits for it.
    fn next(&mut self) -> Result<String> {
        loop {
            if let Some(message) = self.take()? {
                return Ok(message);
            }
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk)?;
            if read == 0 {
                bail!("The compositor closed the control channel")
            }
            self.pending.extend_from_slice(&chunk[..read]);
        }
    }

    /// The whole messages that wait now, without waiting for more, and why
    /// no more can be read after them, if the frames can no longer be told
    /// apart.
    fn waiting(&mut self) -> (Vec<String>, Option<anyhow::Error>) {
        if self.stream.set_nonblocking(true).is_ok() {
            let mut chunk = [0u8; 4096];
            while self.pending.len() <= Self::MAX {
                match self.stream.read(&mut chunk) {
                    Ok(read) if read > 0 => self.pending.extend_from_slice(&chunk[..read]),
                    _ => break,
                }
            }
            let _ = self.stream.set_nonblocking(false);
        }
        let mut messages = Vec::new();
        loop {
            match self.take() {
                Ok(Some(message)) => messages.push(message),
                Ok(None) => return (messages, None),
                Err(error) => return (messages, Some(error)),
            }
        }
    }
}

/// The Hyprland socket. Retained for non-Fulcrum development environments.
pub struct Hyprland {
    socket: PathBuf,
}

impl Hyprland {
    /// Where the running Hyprland session listens, from its environment.
    fn socket_path() -> Result<PathBuf> {
        let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .context("No Hyprland session signature")?;
        if sig.contains('/') || sig.contains("..") {
            bail!("Invalid Hyprland signature")
        }
        Ok(
            PathBuf::from(
                std::env::var_os("XDG_RUNTIME_DIR").context("Missing runtime directory")?,
            )
            .join("hypr")
            .join(sig)
            .join(".socket.sock"),
        )
    }

    fn request(&self, command: &str) -> Result<String> {
        let mut s = UnixStream::connect(&self.socket)?;
        s.set_read_timeout(Some(Duration::from_millis(500)))?;
        s.set_write_timeout(Some(Duration::from_millis(500)))?;
        s.write_all(command.as_bytes())?;
        let mut out = String::new();
        s.take(4 * 1024 * 1024).read_to_string(&mut out)?;
        Ok(out)
    }
}

impl Adapter for Hyprland {
    fn snapshot(&self) -> Result<Snapshot> {
        Ok(Snapshot {
            workspace: serde_json::from_str::<Workspace>(&self.request("j/activeworkspace")?)?.id,
            clients: serde_json::from_str(&self.request("j/clients")?)?,
            active: serde_json::from_str(&self.request("j/activewindow")?)?,
            connected: true,
            locked: false,
        })
    }

    fn focus_workspace(&self, id: i32) -> Result<()> {
        self.request(&format!(
            "/eval hl.dispatch(hl.dsp.focus({{workspace=\"{id}\"}}))"
        ))
        .map(drop)
    }

    fn focus_window(&self, address: &str) -> Result<()> {
        self.request(&format!(
            "/eval hl.dispatch(hl.dsp.focus({{window=\"address:{address}\"}}))"
        ))
        .map(drop)
    }
}

/// The shell's end of the compositor's control channel.
///
/// The shell's wrapper connects to the compositor's control channel and hands
/// the shell the socket in `CROWBAR_SHELL_CONTROL_FD`. Each message is a
/// frame, and each end's first frame states the version. A request gets one
/// reply, and the compositor may send events at any time. See "The control
/// channel" in `compositor/README.md`.
pub struct Compositor {
    /// The socket. `None` once it has failed.
    channel: Mutex<Option<Channel>>,
    /// Where unsolicited compositor events are delivered.
    events: Option<Sender>,
}

struct Channel {
    reader: Frames,
    writer: UnixStream,
    /// Replies still to come to requests that gave up waiting for them.
    /// Replies come in the order of the requests, so these are the next
    /// ones read, and each is skipped.
    owed: usize,
}

/// How an exchange failed.
enum Failure {
    /// The request was sent and its reply did not come in time. It will
    /// come later, as the compositor can be busy for a moment, as when it
    /// sets a display's mode on the way back from the console.
    Late,
    /// The channel cannot be trusted any more.
    Broken(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(error: anyhow::Error) -> Self {
        Failure::Broken(error)
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Failure::Broken(error.into())
    }
}

impl Compositor {
    /// Takes the socket and states the shell's version on it. The
    /// compositor's own version is checked when its first frame is read. If
    /// the version cannot be sent, the channel is closed from the start.
    pub fn new(stream: UnixStream, events: Option<Sender>) -> Result<Self> {
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        stream.set_write_timeout(Some(Duration::from_millis(500)))?;
        let mut channel = Channel {
            reader: Frames {
                stream: stream.try_clone()?,
                pending: Vec::new(),
                versioned: false,
            },
            writer: stream,
            owed: 0,
        };
        let stated = channel
            .writer
            .write_all(&frame(&serde_json::json!({ "version": CONTROL_VERSION })))
            .inspect_err(|e| log::warn!("Compositor: could not state the version: {e}"));
        Ok(Self {
            channel: Mutex::new(stated.ok().map(|()| channel)),
            events,
        })
    }

    /// Adopts the socket the compositor handed over, named by the value of
    /// `CROWBAR_SHELL_CONTROL_FD`. The descriptor is made close-on-exec, so
    /// the programs the shell starts do not inherit it.
    fn inherited(descriptor: &str, events: Option<Sender>) -> Result<Self> {
        let fd: RawFd = descriptor
            .parse()
            .context("Invalid compositor control channel")?;
        // Safety: the compositor passed this descriptor to this process for
        // this purpose, and `CompositorClient::connect()` adopts it once.
        let stream = unsafe {
            if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
                bail!("Invalid compositor control channel")
            }
            UnixStream::from_raw_fd(fd)
        };
        Self::new(stream, events)
    }

    /// The most replies owed before the channel is given up: a compositor
    /// that far behind is stuck.
    const MAX_OWED: usize = 8;

    /// Sends one request and returns its reply. Events that arrive first are
    /// delivered and skipped.
    fn request(&self, request: serde_json::Value) -> Result<serde_json::Value> {
        let mut guard = self
            .channel
            .lock()
            .map_err(|_| anyhow::anyhow!("Compositor channel failed"))?;
        let channel = guard.as_mut().context("The compositor channel is closed")?;
        let message = match channel.exchange(request, self.events.as_ref()) {
            Ok(message) => message,
            Err(Failure::Late) if channel.owed < Self::MAX_OWED => {
                channel.owed += 1;
                bail!("The compositor's reply is late")
            }
            Err(Failure::Late) => {
                *guard = None;
                bail!(
                    "The compositor has not replied to {} requests",
                    Self::MAX_OWED + 1
                )
            }
            Err(Failure::Broken(error)) => {
                *guard = None;
                return Err(error);
            }
        };
        match message["reply"].as_str() {
            Some("error") => bail!("The compositor refused: {}", message["message"]),
            _ => Ok(message),
        }
    }
}

impl Channel {
    fn exchange(
        &mut self,
        request: serde_json::Value,
        events: Option<&Sender>,
    ) -> Result<serde_json::Value, Failure> {
        // A write that fails may have sent part of the frame.
        self.writer
            .write_all(&frame(&request))
            .map_err(|error| Failure::Broken(error.into()))?;
        loop {
            let text = self.reader.next().map_err(|error| {
                let late = error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    )
                });
                if late {
                    Failure::Late
                } else {
                    Failure::Broken(error)
                }
            })?;
            let message: serde_json::Value = serde_json::from_str(&text)?;
            if message.get("event").is_some() {
                deliver_compositor_event(events, &message);
                continue;
            }
            if message["reply"].is_string() {
                if self.owed > 0 {
                    self.owed -= 1;
                    continue;
                }
                return Ok(message);
            }
            return Err(Failure::Broken(anyhow::anyhow!(
                "Unexpected message from the compositor"
            )));
        }
    }
}

impl Adapter for Compositor {
    fn clipboard_start(&self) -> Result<()> {
        self.request(serde_json::json!({"op":"clipboard_start"}))
            .map(drop)
    }

    fn clipboard_history(&self, clear: bool) -> Result<Vec<String>> {
        if clear {
            self.request(serde_json::json!({"op":"clipboard_clear"}))?;
        }
        let reply = self.request(serde_json::json!({"op":"clipboard_history"}))?;
        Ok(serde_json::from_value(reply["items"].clone())?)
    }

    fn set_theme(&self, colors: &crowbar_theme::ThemeColors) -> Result<()> {
        self.request(serde_json::json!({
            "op": "set_theme",
            "focused": crowbar_theme::hex(colors.focus),
            "unfocused": crowbar_theme::hex(colors.line),
            "background": crowbar_theme::hex(colors.canvas),
        }))
        .map(drop)
    }

    fn snapshot(&self) -> Result<Snapshot> {
        snapshot_from(self.request(serde_json::json!({"op": "snapshot"}))?)
    }

    fn focus_workspace(&self, id: i32) -> Result<()> {
        self.request(serde_json::json!({"op": "focus_workspace", "id": id}))
            .map(drop)
    }

    fn focus_window(&self, address: &str) -> Result<()> {
        self.request(serde_json::json!({"op": "focus_window", "id": address}))
            .map(drop)
    }

    fn launching(&self) -> Result<()> {
        self.request(serde_json::json!({"op": "launching"}))
            .map(drop)
    }

    fn lock(&self, locked: bool) -> Result<()> {
        let op = if locked { "lock" } else { "unlock" };
        self.request(serde_json::json!({ "op": op })).map(drop)
    }

    fn agent_keyboard(&self, to_surface: bool) -> Result<()> {
        let op = if to_surface {
            "focus_agent"
        } else {
            "unfocus_agent"
        };
        self.request(serde_json::json!({ "op": op })).map(drop)
    }

    fn reports_outputs(&self) -> bool {
        true
    }

    fn outputs(&self) -> Result<serde_json::Value> {
        self.request(serde_json::json!({ "op": "outputs" }))
    }

    fn configure_output(&self, request: serde_json::Value) -> Result<()> {
        self.request(request).map(drop)
    }

    fn input_config(&self) -> Result<serde_json::Value> {
        self.request(serde_json::json!({ "op": "input_config" }))
    }

    fn configure_input(&self, request: serde_json::Value) -> Result<()> {
        self.request(request).map(drop)
    }

    fn power_config(&self) -> Result<serde_json::Value> {
        self.request(serde_json::json!({ "op": "power_config" }))
    }

    fn configure_power(&self, request: serde_json::Value) -> Result<()> {
        self.request(request).map(drop)
    }

    fn pump(&self) {
        let Ok(mut guard) = self.channel.lock() else {
            return;
        };
        let Some(channel) = guard.as_mut() else {
            return;
        };
        let (waiting, broken) = channel.reader.waiting();
        for text in waiting {
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(message) => {
                    if message.get("event").is_some() {
                        deliver_compositor_event(self.events.as_ref(), &message);
                    } else if message["reply"].is_string() && channel.owed > 0 {
                        // A late reply, come while no request waited.
                        channel.owed -= 1;
                    }
                }
                Err(e) => log::warn!("Compositor: {e}"),
            }
        }
        if let Some(e) = broken {
            // The frames cannot be told apart any more.
            log::warn!("Compositor: {e}");
            *guard = None;
        }
    }

    fn closed(&self) -> bool {
        self.channel
            .lock()
            .map_or(true, |channel| channel.is_none())
    }

    /// Waits on a copy of the socket, so that a request on another thread
    /// does not wait for this. A request may read what woke it first.
    fn wait(&self, timeout: Duration) {
        let copy = match self.channel.lock() {
            Ok(guard) => guard
                .as_ref()
                .and_then(|channel| channel.reader.stream.try_clone().ok()),
            Err(_) => None,
        };
        let Some(copy) = copy else {
            std::thread::sleep(timeout);
            return;
        };
        let mut poll = libc::pollfd {
            fd: copy.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let millis = timeout.as_millis().min(i32::MAX as u128) as i32;
        // Safety: one valid pollfd, and the descriptor is open while `copy` lives.
        unsafe {
            libc::poll(&mut poll, 1, millis);
        }
    }
}

/// Picks the adapter for the compositor the shell runs under.
///
/// A control channel means the CrowbarOS compositor started the shell, and
/// only that channel is used: if it is unusable, the shell does not fall back
/// to Hyprland. With no control channel, the shell uses the Hyprland socket.
fn adapter_for(
    control_descriptor: Option<String>,
    hyprland_socket: Result<PathBuf>,
    events: Option<Sender>,
) -> Result<Arc<dyn Adapter>> {
    match control_descriptor {
        Some(descriptor) => Ok(Arc::new(Compositor::inherited(&descriptor, events)?)),
        None => Ok(Arc::new(Hyprland {
            socket: hyprland_socket.context("No compositor control channel")?,
        })),
    }
}

/// Injected handle to the compositor adapter and its ordered Agent keyboard
/// request queue. Replaces the former global `CONNECTION`, `EVENTS`, and
/// `REQUESTS` `OnceLock` singletons.
#[derive(Clone)]
pub struct CompositorClient {
    adapter: Option<Arc<dyn Adapter>>,
    keyboard_tx: Option<mpsc::Sender<bool>>,
}

impl CompositorClient {
    /// Picks the compositor adapter, adopts `CROWBAR_SHELL_CONTROL_FD` (making
    /// it close-on-exec before any child process or thread is started), and
    /// enables clipboard capture.
    pub fn connect(events: Sender) -> Self {
        let adapter = adapter_for(
            std::env::var("CROWBAR_SHELL_CONTROL_FD").ok(),
            Hyprland::socket_path(),
            Some(events),
        )
        .inspect_err(|e| log::warn!("Compositor: {e}"))
        .ok();
        let client = match adapter {
            Some(adapter) => Self::from_adapter(adapter),
            None => Self::disconnected(),
        };
        if let Err(error) = client.adapter().and_then(|c| c.clipboard_start()) {
            log::warn!("Starting clipboard history: {error}");
        }
        client
    }

    /// Wraps an injected compositor adapter (used by both production and tests).
    pub fn from_adapter(adapter: Arc<dyn Adapter>) -> Self {
        let (tx, rx) = mpsc::channel::<bool>();
        let worker_adapter = adapter.clone();
        std::thread::spawn(move || {
            for to_surface in rx {
                if let Err(e) = worker_adapter.agent_keyboard(to_surface) {
                    log::warn!("Agent's surface keyboard: {e}");
                }
            }
        });
        Self {
            adapter: Some(adapter),
            keyboard_tx: Some(tx),
        }
    }

    pub fn disconnected() -> Self {
        Self {
            adapter: None,
            keyboard_tx: None,
        }
    }

    pub fn adapter(&self) -> Result<&Arc<dyn Adapter>> {
        self.adapter.as_ref().context("No compositor to talk to")
    }

    pub fn clipboard_history(&self, clear: bool) -> Result<Vec<String>> {
        self.adapter()?.clipboard_history(clear)
    }

    pub fn set_theme(&self, colors: &crowbar_theme::ThemeColors) -> Result<()> {
        self.adapter()?.set_theme(colors)
    }

    pub fn snapshot(&self) -> Result<Snapshot> {
        self.adapter()?.snapshot()
    }

    pub fn launching(&self) -> Result<()> {
        self.adapter()?.launching()
    }

    /// The compositor's control channel is gone for good.
    pub fn closed(&self) -> bool {
        self.adapter
            .as_ref()
            .is_some_and(|connection| connection.closed())
    }

    /// Asks the compositor to lock or unlock the session on a worker thread.
    pub fn lock(&self, locked: bool) {
        let Some(adapter) = self.adapter.clone() else {
            return;
        };
        std::thread::spawn(move || {
            if let Err(e) = adapter.lock(locked) {
                log::warn!("Lock: {e}");
            }
        });
    }

    /// Asks the compositor to give the Agent's surface the keyboard or return
    /// it to the window that had it (ADR 0035), strictly in call order.
    pub fn agent_keyboard(&self, to_surface: bool) {
        if let Some(tx) = &self.keyboard_tx {
            let _ = tx.send(to_surface);
        }
    }

    /// Waits until the compositor has a display to show the shell's layers on.
    pub fn wait_for_a_display(&self) -> Result<()> {
        wait_for_a_display_on(self.adapter()?.as_ref(), || {
            std::thread::sleep(DISPLAY_POLL)
        })
    }

    /// Reads the compositor's events as they come, for `period`.
    pub fn read_events_for(&self, period: Duration) {
        for_a_while(
            period,
            |timeout| match self.adapter.as_ref() {
                Some(connection) => connection.wait(timeout),
                None => std::thread::sleep(timeout),
            },
            || self.pump(),
        );
    }

    /// Reads the compositor's events that wait, as Super tapped alone.
    pub fn pump(&self) {
        if let Some(connection) = self.adapter.as_ref() {
            connection.pump();
        }
    }

    pub fn focus_workspace(&self, id: i32) {
        if id > 0 {
            let Some(adapter) = self.adapter.clone() else {
                return;
            };
            std::thread::spawn(move || {
                if let Err(e) = adapter.focus_workspace(id) {
                    log::warn!("Workspace: {e}");
                }
            });
        }
    }

    pub fn focus_window(&self, address: String) {
        if address.starts_with("0x") && address[2..].chars().all(|c| c.is_ascii_hexdigit()) {
            let Some(adapter) = self.adapter.clone() else {
                return;
            };
            std::thread::spawn(move || {
                if let Err(e) = adapter.focus_window(&address) {
                    log::warn!("Focus: {e}");
                }
            });
        }
    }
}

/// How often the shell asks again while there is no display. The compositor
/// sends no event when a display comes, and this is only while there is none.
const DISPLAY_POLL: Duration = Duration::from_millis(250);

fn wait_for_a_display_on(adapter: &dyn Adapter, mut pause: impl FnMut()) -> Result<()> {
    if !adapter.reports_outputs() {
        return Ok(());
    }
    let mut said = false;
    loop {
        match adapter.outputs() {
            Ok(reply)
                if reply["outputs"]
                    .as_array()
                    .is_some_and(|outputs| !outputs.is_empty()) =>
            {
                if said {
                    log::info!("SHELL display connected");
                }
                return Ok(());
            }
            Ok(_) => {}
            Err(e) if adapter.closed() => return Err(e),
            // A late reply: ask again.
            Err(e) => log::debug!("Outputs: {e}"),
        }
        if !said {
            log::info!("SHELL waits for a display");
            said = true;
        }
        pause();
    }
}

/// Waits, then reads, until `period` is over. A wait may end late.
fn for_a_while(period: Duration, mut wait: impl FnMut(Duration), mut read: impl FnMut()) {
    let start = Instant::now();
    while start.elapsed() < period {
        wait(period.saturating_sub(start.elapsed()));
        read();
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::thread::JoinHandle;

    pub fn framed(message: &str) -> Vec<u8> {
        let mut frame = (message.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(message.as_bytes());
        frame
    }

    pub fn read_frame(stream: &mut UnixStream) -> String {
        let mut length = [0u8; 4];
        stream.read_exact(&mut length).unwrap();
        let mut message = vec![0u8; u32::from_be_bytes(length) as usize];
        stream.read_exact(&mut message).unwrap();
        String::from_utf8(message).unwrap()
    }

    pub const VERSION: &str = r#"{"version":1}"#;

    pub fn exchange_versions(stream: &mut UnixStream) {
        assert_eq!(read_frame(stream), VERSION);
        stream.write_all(&framed(VERSION)).unwrap();
    }

    pub fn scripted(
        script: Vec<(&'static str, &'static [&'static str])>,
    ) -> (Compositor, JoinHandle<()>) {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            exchange_versions(&mut theirs);
            for (expected, replies) in script {
                let sent: serde_json::Value =
                    serde_json::from_str(&read_frame(&mut theirs)).unwrap();
                let expected: serde_json::Value = serde_json::from_str(expected).unwrap();
                assert_eq!(sent, expected);
                for reply in replies {
                    theirs.write_all(&framed(reply)).unwrap();
                }
            }
        });
        (Compositor::new(ours, None).unwrap(), peer)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{VERSION, exchange_versions, framed, read_frame, scripted};
    use super::*;
    use crate::events::channel;
    use std::{fs, os::unix::net::UnixListener, thread::JoinHandle};

    #[test]
    fn clipboard_events_open_the_right_picker_without_carrying_contents() {
        use crate::clipboard_picker::Kind;
        assert!(matches!(
            shell_event(&serde_json::json!({"event":"clipboard"})),
            Some(Event::Picker(Kind::History))
        ));
        assert!(matches!(
            shell_event(&serde_json::json!({"event":"characters"})),
            Some(Event::Picker(Kind::Characters))
        ));
        assert!(matches!(
            shell_event(&serde_json::json!({"event":"clipboard_changed"})),
            Some(Event::ClipboardChanged)
        ));
    }

    #[test]
    fn the_shell_states_the_version_first_and_each_message_is_a_frame() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            let mut version = [0u8; 17];
            theirs.read_exact(&mut version).unwrap();
            assert_eq!(&version, b"\0\0\0\x0d{\"version\":1}");
            theirs.write_all(b"\0\0\0\x0d{\"version\":1}").unwrap();
            let mut request = [0u8; 22];
            theirs.read_exact(&mut request).unwrap();
            assert_eq!(&request, b"\0\0\0\x12{\"op\":\"launching\"}");
            theirs.write_all(b"\0\0\0\x0e{\"reply\":\"ok\"}").unwrap();
            theirs
        });
        let compositor = Compositor::new(ours, None).unwrap();
        compositor.launching().unwrap();
        let _peer_end = peer.join().unwrap();
    }

    #[test]
    fn a_compositor_that_states_another_version_or_none_is_given_up() {
        for first in [
            r#"{"version":2}"#,
            r#"{"reply":"ok"}"#,
            r#"{"event":"launcher"}"#,
            "version 1",
        ] {
            let (ours, mut theirs) = UnixStream::pair().unwrap();
            theirs.write_all(&framed(first)).unwrap();
            theirs.write_all(&framed(r#"{"reply":"ok"}"#)).unwrap();
            let compositor = Compositor::new(ours, None).unwrap();
            assert!(compositor.launching().is_err(), "{first}");
            assert!(compositor.closed(), "{first}");

            let (ours, mut theirs) = UnixStream::pair().unwrap();
            theirs.write_all(&framed(first)).unwrap();
            let compositor = Compositor::new(ours, None).unwrap();
            compositor.pump();
            assert!(compositor.closed(), "{first}, read by the pump");
        }
    }

    #[test]
    fn a_frame_longer_than_the_shell_reads_closes_the_channel() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        theirs.write_all(&framed(VERSION)).unwrap();
        theirs.write_all(&[0x00, 0x40, 0x00, 0x01]).unwrap();
        let compositor = Compositor::new(ours, None).unwrap();
        let start = Instant::now();
        assert!(compositor.launching().is_err());
        assert!(compositor.closed());
        assert!(
            start.elapsed() < Duration::from_millis(400),
            "{:?}",
            start.elapsed()
        );
    }

    #[test]
    fn the_shell_waits_for_a_display() {
        const NONE: &[&str] = &[r#"{"reply":"outputs","outputs":[]}"#];
        const ONE: &[&str] = &[r#"{"reply":"outputs","outputs":[{"name":"Virtual-1"}]}"#];
        const ASK: &str = r#"{"op":"outputs"}"#;
        let (compositor, peer) = scripted(vec![(ASK, NONE), (ASK, NONE), (ASK, ONE)]);
        let mut pauses = 0;
        wait_for_a_display_on(&compositor, || pauses += 1).unwrap();
        assert_eq!(pauses, 2);
        peer.join().unwrap();
    }

    #[test]
    fn the_wait_for_a_display_ends_without_a_compositor_to_ask() {
        let hyprland = Hyprland {
            socket: PathBuf::from("/nonexistent"),
        };
        wait_for_a_display_on(&hyprland, || panic!("waited")).unwrap();
        let (ours, theirs) = UnixStream::pair().unwrap();
        drop(theirs);
        let compositor = Compositor::new(ours, None).unwrap();
        assert!(wait_for_a_display_on(&compositor, || panic!("waited")).is_err());
    }

    #[test]
    fn reading_events_for_a_while_survives_a_wait_that_ends_late() {
        let mut waits = 0;
        for_a_while(
            Duration::from_millis(20),
            |timeout| {
                waits += 1;
                std::thread::sleep(timeout + Duration::from_millis(5));
            },
            || {},
        );
        assert_eq!(waits, 1);
    }

    #[test]
    fn super_tapped_alone_toggles_the_launcher_and_a_launch_is_announced() {
        let (tx, mut rx) = channel();
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let compositor = Compositor::new(ours, Some(tx)).unwrap();

        exchange_versions(&mut theirs);

        theirs
            .write_all(&framed(r#"{"event":"launcher"}"#))
            .unwrap();
        compositor.pump();
        assert!(matches!(rx.try_recv(), Ok(Event::Toggle)));

        let launcher = framed(r#"{"event":"launcher"}"#);
        theirs.write_all(&launcher[..14]).unwrap();
        compositor.pump();
        assert!(rx.try_recv().is_err());
        theirs.write_all(&launcher[14..]).unwrap();
        theirs.write_all(&launcher[..2]).unwrap();
        compositor.pump();
        assert!(matches!(rx.try_recv(), Ok(Event::Toggle)));
        assert!(rx.try_recv().is_err());
        theirs.write_all(&launcher[2..]).unwrap();
        compositor.pump();
        assert!(matches!(rx.try_recv(), Ok(Event::Toggle)));

        theirs
            .write_all(
                &[
                    framed(r#"{"event":"locked"}"#),
                    framed(r#"{"event":"unlocked"}"#),
                ]
                .concat(),
            )
            .unwrap();
        compositor.pump();
        assert!(matches!(rx.try_recv(), Ok(Event::Lock(true))));
        assert!(matches!(rx.try_recv(), Ok(Event::Lock(false))));

        let mut later = theirs.try_clone().unwrap();
        theirs.write_all(&launcher[..14]).unwrap();
        compositor.pump();
        let peer = std::thread::spawn(move || {
            assert_eq!(read_frame(&mut theirs), r#"{"op":"launching"}"#);
            theirs
                .write_all(&[&launcher[14..], &framed(r#"{"reply":"ok"}"#)].concat())
                .unwrap();
        });
        compositor.launching().unwrap();
        peer.join().unwrap();
        assert!(matches!(rx.try_recv(), Ok(Event::Toggle)));

        later
            .write_all(&framed(concat!(
                r#"{"event":"snapshot","workspace":2,"active":"0x2","locked":false,"#,
                r#""windows":[{"id":"0x2","app_id":"foot","title":"~","workspace":2,"attention":false}]}"#
            )))
            .unwrap();
        compositor.pump();
        match rx.try_recv() {
            Ok(Event::Snapshot(snapshot)) => {
                assert_eq!(snapshot.workspace, 2);
                assert_eq!(snapshot.active.address, "0x2");
                assert_eq!(snapshot.clients[0].workspace.id, 2);
                assert!(snapshot.connected);
            }
            _ => panic!("the snapshot event did not reach the shell"),
        }

        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            later
                .write_all(&framed(r#"{"event":"launcher"}"#))
                .unwrap();
            later
        });
        let start = Instant::now();
        compositor.wait(Duration::from_secs(10));
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        let mut later = sender.join().unwrap();
        compositor.pump();
        assert!(matches!(rx.try_recv(), Ok(Event::Toggle)));

        later
            .write_all(
                &[
                    framed(r#"{"event":"locked"}"#),
                    vec![0xff, 0xff, 0xff, 0xff],
                ]
                .concat(),
            )
            .unwrap();
        compositor.pump();
        assert!(matches!(rx.try_recv(), Ok(Event::Lock(true))));
        assert!(compositor.closed());
    }

    #[test]
    fn a_window_that_wants_attention_is_marked_in_the_snapshot() {
        let (compositor, peer) = scripted(vec![(
            r#"{"op":"snapshot"}"#,
            &[concat!(
                r#"{"reply":"snapshot","workspace":1,"active":null,"windows":["#,
                r#"{"id":"0x1","app_id":"org.example.Notes","title":"Notes","workspace":3,"attention":true}]}"#,
            )],
        )]);
        let snapshot = compositor.snapshot().unwrap();
        peer.join().unwrap();
        assert!(snapshot.clients[0].attention);
    }

    #[test]
    fn a_snapshot_reply_becomes_the_shells_snapshot() {
        let (compositor, peer) = scripted(vec![(
            r#"{"op":"snapshot"}"#,
            &[concat!(
                r#"{"reply":"snapshot","workspace":2,"active":"0x2","windows":["#,
                r#"{"id":"0x1","app_id":"org.example.Terminal","title":"Terminal","workspace":1},"#,
                r#"{"id":"0x2","app_id":"com.crowbaros.StudioSettings","title":"Studio \"Settings\"","workspace":2}]}"#,
            )],
        )]);

        let snapshot = compositor.snapshot().unwrap();

        let studio = Client {
            address: "0x2".into(),
            class: "com.crowbaros.StudioSettings".into(),
            title: "Studio \"Settings\"".into(),
            workspace: Workspace { id: 2 },
            attention: false,
            fullscreen: false,
            output: String::new(),
        };
        assert_eq!(
            snapshot,
            Snapshot {
                workspace: 2,
                clients: vec![
                    Client {
                        address: "0x1".into(),
                        class: "org.example.Terminal".into(),
                        title: "Terminal".into(),
                        workspace: Workspace { id: 1 },
                        attention: false,
                        fullscreen: false,
                        output: String::new(),
                    },
                    studio.clone(),
                ],
                active: studio,
                connected: true,
                locked: false,
            }
        );
        peer.join().unwrap();
    }

    #[test]
    fn focus_requests_use_the_compositors_wire_format() {
        let (compositor, peer) = scripted(vec![
            (r#"{"id":3,"op":"focus_workspace"}"#, &[r#"{"reply":"ok"}"#]),
            (
                r#"{"id":"0x1f","op":"focus_window"}"#,
                &[r#"{"reply":"ok"}"#],
            ),
        ]);

        compositor.focus_workspace(3).unwrap();
        compositor.focus_window("0x1f").unwrap();

        peer.join().unwrap();
    }

    #[test]
    fn the_summon_gesture_reaches_the_shell_with_its_display_and_the_keyboard() {
        let message: serde_json::Value = serde_json::from_str(
            r#"{"event":"summon","output":"HDMI-A-1","width":2560,"keyboard":false}"#,
        )
        .unwrap();
        match shell_event(&message) {
            Some(Event::Summon(summon)) => assert_eq!(
                summon,
                Summon {
                    output: "HDMI-A-1".into(),
                    width: 2560.,
                    keyboard: false,
                }
            ),
            other => panic!("{other:?}"),
        }
        for other in [r#"{"event":"summon"}"#, r#"{"event":"summon","output":1}"#] {
            let message: serde_json::Value = serde_json::from_str(other).unwrap();
            assert!(shell_event(&message).is_none(), "{other}");
        }
        let launcher = serde_json::json!({"event": "launcher"});
        assert!(matches!(shell_event(&launcher), Some(Event::Toggle)));
    }

    #[test]
    fn the_agents_surface_asks_for_the_keyboard_and_gives_it_back_in_the_compositors_wire_format() {
        let (compositor, peer) = scripted(vec![
            (r#"{"op":"focus_agent"}"#, &[r#"{"reply":"ok"}"#]),
            (r#"{"op":"unfocus_agent"}"#, &[r#"{"reply":"ok"}"#]),
        ]);

        compositor.agent_keyboard(true).unwrap();
        compositor.agent_keyboard(false).unwrap();

        peer.join().unwrap();
    }

    #[test]
    fn an_event_that_arrives_before_the_reply_is_skipped() {
        let (compositor, peer) = scripted(vec![(
            r#"{"id":1,"op":"focus_workspace"}"#,
            &[r#"{"event":"confirm"}"#, r#"{"reply":"ok"}"#],
        )]);

        compositor.focus_workspace(1).unwrap();

        peer.join().unwrap();
    }

    #[test]
    fn a_refusal_from_the_compositor_is_an_error() {
        let (compositor, peer) = scripted(vec![(
            r#"{"id":"0x99","op":"focus_window"}"#,
            &[r#"{"reply":"error","message":"no such window"}"#],
        )]);

        let error = compositor.focus_window("0x99").unwrap_err();

        assert!(error.to_string().contains("no such window"), "{error}");
        peer.join().unwrap();
    }

    #[test]
    fn a_control_channel_descriptor_is_adopted_and_used() {
        use std::os::fd::IntoRawFd;
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            exchange_versions(&mut theirs);
            let sent: serde_json::Value =
                serde_json::from_str(&read_frame(&mut theirs)).unwrap();
            assert_eq!(sent, serde_json::json!({"op": "focus_workspace", "id": 2}));
            theirs.write_all(&framed(r#"{"reply":"ok"}"#)).unwrap();
        });
        let descriptor = ours.into_raw_fd().to_string();

        let adapter = adapter_for(
            Some(descriptor),
            Err(anyhow::anyhow!("No Hyprland session signature")),
            None,
        )
        .unwrap();
        adapter.focus_workspace(2).unwrap();

        peer.join().unwrap();
    }

    fn scripted_hyprland(script: Vec<(&'static str, &'static str)>) -> (PathBuf, JoinHandle<()>) {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let base = std::env::temp_dir();
        let root = if base.as_os_str().len() > 48 {
            PathBuf::from("/tmp")
        } else {
            base
        };
        let dir = root.join(format!(
            "crowbaros-shell-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hyprland.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let peer = std::thread::spawn(move || {
            for (expected, reply) in script {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = [0u8; 256];
                let n = stream.read(&mut bytes).unwrap();
                assert_eq!(std::str::from_utf8(&bytes[..n]).unwrap(), expected);
                stream.write_all(reply.as_bytes()).unwrap();
            }
            let _ = fs::remove_dir_all(dir);
        });
        (path, peer)
    }

    #[test]
    fn without_a_control_channel_the_shell_reads_its_snapshot_from_hyprland() {
        let (socket, peer) = scripted_hyprland(vec![
            ("j/activeworkspace", r#"{"id":2,"name":"2"}"#),
            (
                "j/clients",
                r#"[{"address":"0x1","class":"org.example.Terminal","title":"Terminal","workspace":{"id":1,"name":"1"}}]"#,
            ),
            (
                "j/activewindow",
                r#"{"address":"0x1","class":"org.example.Terminal","title":"Terminal","workspace":{"id":1,"name":"1"}}"#,
            ),
        ]);

        let adapter = adapter_for(None, Ok(socket), None).unwrap();
        let snapshot = adapter.snapshot().unwrap();

        let terminal = Client {
            address: "0x1".into(),
            class: "org.example.Terminal".into(),
            title: "Terminal".into(),
            workspace: Workspace { id: 1 },
            attention: false,
            fullscreen: false,
            output: String::new(),
        };
        assert_eq!(
            snapshot,
            Snapshot {
                workspace: 2,
                clients: vec![terminal.clone()],
                active: terminal,
                connected: true,
                locked: false,
            }
        );
        peer.join().unwrap();
    }

    #[test]
    fn a_hyprland_window_whose_workspace_has_no_id_does_not_lose_the_snapshot() {
        let (socket, peer) = scripted_hyprland(vec![
            ("j/activeworkspace", r#"{"id":1,"name":"1"}"#),
            (
                "j/clients",
                concat!(
                    r#"[{"address":"0x1","class":"org.example.Terminal","title":"Terminal","workspace":{"id":1,"name":"1"}},"#,
                    r#"{"address":"0x2","class":"org.example.Notes","title":"Notes","workspace":{"address":"0x0","type":"special","name":"notes"}}]"#
                ),
            ),
            (
                "j/activewindow",
                r#"{"address":"0x1","class":"org.example.Terminal","title":"Terminal","workspace":{"id":1,"name":"1"}}"#,
            ),
        ]);

        let snapshot = adapter_for(None, Ok(socket), None)
            .unwrap()
            .snapshot()
            .unwrap();

        assert_eq!(snapshot.workspace, 1);
        assert_eq!(
            snapshot.clients,
            vec![
                Client {
                    address: "0x1".into(),
                    class: "org.example.Terminal".into(),
                    title: "Terminal".into(),
                    workspace: Workspace { id: 1 },
                    attention: false,
                    fullscreen: false,
                    output: String::new(),
                },
                Client {
                    address: "0x2".into(),
                    class: "org.example.Notes".into(),
                    title: "Notes".into(),
                    workspace: Workspace { id: 0 },
                    attention: false,
                    fullscreen: false,
                    output: String::new(),
                },
            ]
        );
        peer.join().unwrap();
    }

    #[test]
    fn without_a_control_channel_focus_requests_go_to_hyprland() {
        let (socket, peer) = scripted_hyprland(vec![
            (r#"/eval hl.dispatch(hl.dsp.focus({workspace="3"}))"#, "ok"),
            (
                r#"/eval hl.dispatch(hl.dsp.focus({window="address:0x1f"}))"#,
                "ok",
            ),
        ]);

        let adapter = adapter_for(None, Ok(socket), None).unwrap();
        adapter.focus_workspace(3).unwrap();
        adapter.focus_window("0x1f").unwrap();

        peer.join().unwrap();
    }

    #[test]
    fn a_broken_control_channel_does_not_fall_back_to_hyprland() {
        let (socket, _peer) = scripted_hyprland(vec![]);

        assert!(adapter_for(Some("not-a-descriptor".into()), Ok(socket), None).is_err());
    }

    #[test]
    fn with_neither_a_control_channel_nor_hyprland_there_is_no_adapter() {
        assert!(
            adapter_for(None, Err(anyhow::anyhow!("No Hyprland session signature")), None).is_err()
        );
    }

    #[test]
    fn a_late_reply_is_skipped_and_the_next_request_gets_its_own() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            exchange_versions(&mut theirs);
            read_frame(&mut theirs);
            std::thread::sleep(Duration::from_millis(800));
            theirs
                .write_all(&framed(r#"{"reply":"error","message":"the late one"}"#))
                .unwrap();
            let next = read_frame(&mut theirs);
            assert!(next.contains("focus_window"), "{next}");
            theirs.write_all(&framed(r#"{"reply":"ok"}"#)).unwrap();
            theirs
        });
        let compositor = Compositor::new(ours, None).unwrap();

        let late = compositor.focus_workspace(1).unwrap_err();
        assert!(late.to_string().contains("late"), "{late}");
        compositor.focus_window("0x1").unwrap();
        assert!(!compositor.closed());
        let _peer_end = peer.join().unwrap();
    }

    #[test]
    fn a_late_reply_that_comes_while_no_request_waits_is_skipped_too() {
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            exchange_versions(&mut theirs);
            read_frame(&mut theirs);
            std::thread::sleep(Duration::from_millis(800));
            theirs
                .write_all(&framed(r#"{"reply":"error","message":"the late one"}"#))
                .unwrap();
            read_frame(&mut theirs);
            theirs.write_all(&framed(r#"{"reply":"ok"}"#)).unwrap();
            theirs
        });
        let compositor = Compositor::new(ours, None).unwrap();

        assert!(compositor.focus_workspace(1).is_err());
        std::thread::sleep(Duration::from_millis(500));
        compositor.pump();
        compositor.focus_window("0x1").unwrap();
        let _peer_end = peer.join().unwrap();
    }

    #[test]
    fn a_channel_the_compositor_closed_is_closed_for_good() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        drop(theirs);
        let compositor = Compositor::new(ours, None).unwrap();

        assert!(compositor.focus_workspace(1).is_err());
        assert!(compositor.closed());
        assert!(compositor.focus_window("0x1").is_err());
    }

    #[test]
    fn two_independent_compositor_clients_operate_without_global_state() {
        let (ours_a, mut theirs_a) = UnixStream::pair().unwrap();
        let (ours_b, mut theirs_b) = UnixStream::pair().unwrap();

        let peer_a = std::thread::spawn(move || {
            exchange_versions(&mut theirs_a);
            theirs_a
                .write_all(&framed(r#"{"event":"launcher"}"#))
                .unwrap();
            let req = read_frame(&mut theirs_a);
            assert!(req.contains("focus_agent"), "{req}");
            theirs_a.write_all(&framed(r#"{"reply":"ok"}"#)).unwrap();
            theirs_a
        });
        let peer_b = std::thread::spawn(move || {
            exchange_versions(&mut theirs_b);
            theirs_b
                .write_all(&framed(r#"{"event":"locked"}"#))
                .unwrap();
            let req = read_frame(&mut theirs_b);
            assert!(req.contains("unfocus_agent"), "{req}");
            theirs_b.write_all(&framed(r#"{"reply":"ok"}"#)).unwrap();
            theirs_b
        });

        let (tx_a, mut rx_a) = crate::events::channel();
        let (tx_b, mut rx_b) = crate::events::channel();
        let client_a = CompositorClient::from_adapter(Arc::new(
            Compositor::new(ours_a, Some(tx_a)).unwrap(),
        ));
        let client_b = CompositorClient::from_adapter(Arc::new(
            Compositor::new(ours_b, Some(tx_b)).unwrap(),
        ));

        client_a.agent_keyboard(true);
        client_b.agent_keyboard(false);

        let _end_a = peer_a.join().unwrap();
        let _end_b = peer_b.join().unwrap();

        assert!(matches!(rx_a.try_recv(), Ok(Event::Toggle)));
        assert!(matches!(rx_b.try_recv(), Ok(Event::Lock(true))));
    }
}
