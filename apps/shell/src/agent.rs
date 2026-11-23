// SPDX-License-Identifier: MIT
//! The shell's client of the Harness's surface socket (ADR 0035).
//!
//! Speaks the shared framing (4-byte big-endian length + UTF-8 JSON,
//! `{"version":1}` first) over `CROWBAR_AGENT_SOCKET`, keeping the drawer's
//! conversation state and the mark (`idle`, `working`, `done`). Blocking I/O
//! on dedicated background threads wakes GPUI only when a frame arrives, so a
//! still desktop draws nothing.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use crate::session;

/// Protocol version spoken on the Harness's surface socket.
const SURFACE_VERSION: u32 = 1;
/// Longest frame accepted from the Harness (4 MiB).
const MAX_FRAME: usize = 4 * 1024 * 1024;
const INITIAL_BACKOFF: Duration = Duration::from_millis(200);
const MAX_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopWindow {
    pub id: String,
    pub app_id: String,
    pub title: String,
    pub workspace: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopDisplay {
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
    pub scale: i32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopContext {
    pub workspace: i32,
    pub active: Option<DesktopWindow>,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub windows: Vec<DesktopWindow>,
    #[serde(default)]
    pub displays: Vec<DesktopDisplay>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub provider: String,
    pub provider_name: String,
    pub model: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "lowercase")]
pub enum TurnState {
    Working,
    Done,
    Stopped,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub turn: u64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<DesktopContext>,
    pub answer: String,
    pub state: TurnState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mark {
    #[default]
    Idle,
    Working,
    Done,
}

impl Mark {
    pub fn label(self) -> &'static str {
        match self {
            Mark::Idle => "idle",
            Mark::Working => "working",
            Mark::Done => "done",
        }
    }
}

/// Events sent from the surface-socket client to the shell's UI loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEvent {
    Connected,
    Disconnected,
    Status {
        model: Option<ModelInfo>,
        missing: Option<String>,
    },
    Resumed {
        conversation: String,
        turns: Vec<Turn>,
    },
    Sent {
        conversation: String,
        turn: u64,
    },
    Message {
        conversation: String,
        turn: u64,
        text: String,
        context: Option<DesktopContext>,
    },
    Text {
        conversation: String,
        turn: u64,
        text: String,
    },
    StartOver {
        conversation: String,
        turn: u64,
    },
    Retry {
        conversation: String,
        turn: u64,
        message: String,
        seconds: u64,
    },
    Done {
        conversation: String,
        turn: u64,
    },
    Stopped {
        conversation: String,
        turn: u64,
    },
    Failed {
        conversation: String,
        turn: u64,
        message: String,
    },
    Error {
        code: String,
        message: String,
    },
}

/// Commands sent from the shell's UI to the surface-socket background thread.
#[derive(Clone, Debug)]
pub enum AgentCommand {
    Send {
        conversation: Option<String>,
        text: String,
    },
    Stop {
        conversation: String,
        turn: Option<u64>,
    },
    RefreshStatus,
}

#[derive(Clone)]
pub struct CommandSender {
    tx: mpsc::Sender<AgentCommand>,
}

impl CommandSender {
    pub fn send_message(&self, conversation: Option<String>, text: String) {
        let _ = self.tx.send(AgentCommand::Send { conversation, text });
    }

    pub fn stop_turn(&self, conversation: String, turn: Option<u64>) {
        let _ = self.tx.send(AgentCommand::Stop { conversation, turn });
    }

    pub fn refresh_status(&self) {
        let _ = self.tx.send(AgentCommand::RefreshStatus);
    }
}

/// The shell's held state for the Agent drawer and mark.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AgentState {
    pub configured: bool,
    pub connected: bool,
    pub sending: bool,
    pub model: Option<ModelInfo>,
    pub missing_model: Option<String>,
    pub conversation: Option<String>,
    pub turns: Vec<Turn>,
    pub error: Option<String>,
    pub mark: Mark,
}

/// Snapshot exposed on the shell's own `crowbar-shell`-only control socket for
/// desktop checks to inspect drawer and mark state without touching logs.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AgentViewSnapshot {
    pub drawer_up: bool,
    pub mark_overlay: bool,
    pub mark_overlay_output: Option<String>,
    pub state: AgentState,
}

impl AgentState {
    pub fn new(configured: bool) -> Self {
        Self {
            configured,
            ..Default::default()
        }
    }

    pub fn working(&self) -> bool {
        self.mark == Mark::Working
            || self
                .turns
                .last()
                .is_some_and(|t| matches!(t.state, TurnState::Working))
    }

    pub fn can_send(&self) -> bool {
        self.connected && !self.working() && !self.sending
    }

    pub fn current_turn(&self) -> Option<u64> {
        self.turns.last().map(|t| t.turn)
    }

    /// Applies an event from the Harness. Returns true if visible state changed.
    pub fn apply(&mut self, event: AgentEvent, drawer_up: bool) -> bool {
        match event {
            AgentEvent::Connected => {
                let changed = !self.connected || self.error.is_some();
                self.connected = true;
                self.error = None;
                changed
            }
            AgentEvent::Disconnected => {
                let changed = self.connected || self.sending;
                self.connected = false;
                self.sending = false;
                changed
            }
            AgentEvent::Status { model, missing } => {
                let changed = self.model != model || self.missing_model != missing;
                self.model = model;
                self.missing_model = missing;
                if self.model.is_some() {
                    self.error = None;
                }
                changed
            }
            AgentEvent::Resumed {
                conversation,
                turns,
            } => {
                self.sending = false;
                self.conversation = Some(conversation);
                let next_mark = match turns.last().map(|t| &t.state) {
                    Some(TurnState::Working) => Mark::Working,
                    Some(TurnState::Done) if self.mark == Mark::Working => Mark::Done,
                    Some(TurnState::Failed(_)) if self.mark == Mark::Working && !drawer_up => {
                        Mark::Done
                    }
                    _ if self.mark == Mark::Working => Mark::Idle,
                    _ => self.mark,
                };
                self.turns = turns;
                self.mark = next_mark;
                true
            }
            AgentEvent::Sent {
                conversation,
                turn: _,
            } => {
                self.sending = false;
                if self.conversation.as_deref() != Some(conversation.as_str()) {
                    self.turns.clear();
                }
                self.conversation = Some(conversation);
                self.error = None;
                self.mark = Mark::Working;
                true
            }
            AgentEvent::Message {
                conversation,
                turn,
                text,
                context,
            } => {
                if self.conversation.as_deref() != Some(conversation.as_str()) {
                    if self.conversation.is_none() {
                        self.conversation = Some(conversation);
                    } else {
                        return false;
                    }
                }
                self.sending = false;
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.message = text;
                    existing.context = context;
                } else {
                    self.turns.push(Turn {
                        turn,
                        message: text,
                        context,
                        answer: String::new(),
                        state: TurnState::Working,
                        retry: None,
                    });
                }
                self.error = None;
                self.mark = Mark::Working;
                true
            }
            AgentEvent::Text {
                conversation,
                turn,
                text,
            } => {
                if self.conversation.as_deref() != Some(conversation.as_str()) {
                    return false;
                }
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.answer.push_str(&text);
                    existing.retry = None;
                    return true;
                }
                false
            }
            AgentEvent::StartOver { conversation, turn } => {
                if self.conversation.as_deref() != Some(conversation.as_str()) {
                    return false;
                }
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.answer.clear();
                    existing.retry = None;
                    return true;
                }
                false
            }
            AgentEvent::Retry {
                conversation,
                turn,
                message,
                seconds,
            } => {
                if self.conversation.as_deref() != Some(conversation.as_str()) {
                    return false;
                }
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    let reason = calm_retry_reason(&message);
                    existing.retry = Some(format!("{reason} Trying again in {seconds}s…"));
                    return true;
                }
                false
            }
            AgentEvent::Done { conversation, turn } => {
                if self.conversation.as_deref() != Some(conversation.as_str()) {
                    return false;
                }
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.state = TurnState::Done;
                    existing.retry = None;
                }
                self.mark = Mark::Done;
                true
            }
            AgentEvent::Stopped { conversation, turn } => {
                if self.conversation.as_deref() != Some(conversation.as_str()) {
                    return false;
                }
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.state = TurnState::Stopped;
                    existing.retry = None;
                }
                self.mark = Mark::Idle;
                true
            }
            AgentEvent::Failed {
                conversation,
                turn,
                message,
            } => {
                if self.conversation.as_deref() != Some(conversation.as_str()) {
                    return false;
                }
                self.sending = false;
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.state = TurnState::Failed(calm_turn_error(&message));
                    existing.retry = None;
                }
                // Behind a put-away drawer, leave the mark lit as Done so the
                // person knows the turn ended and can open the drawer to read why.
                self.mark = if drawer_up { Mark::Idle } else { Mark::Done };
                true
            }
            AgentEvent::Error { code, message } => {
                self.sending = false;
                if code == "no-model" {
                    self.model = None;
                    self.missing_model = Some(message);
                    self.error = None;
                } else if code == "no-conversation" {
                    self.conversation = None;
                    self.turns.clear();
                    self.mark = Mark::Idle;
                    self.error = None;
                } else {
                    self.error = Some(calm_turn_error(&message));
                }
                true
            }
        }
    }

    /// Summoning the drawer acknowledges a finished turn's `Done` mark.
    pub fn on_drawer_opened(&mut self) -> bool {
        if self.mark == Mark::Done {
            self.mark = Mark::Idle;
            return true;
        }
        false
    }

    /// Putting the drawer away clears `Done` (already seen in the drawer),
    /// while leaving `Working` active so the mark tracks the turn behind it.
    pub fn on_drawer_closed(&mut self) -> bool {
        if self.mark == Mark::Done {
            self.mark = Mark::Idle;
            return true;
        }
        false
    }
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    let bytes = haystack.as_bytes();
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let abs = start + pos;
        let end = abs + needle.len();
        let before_ok = abs == 0 || !bytes[abs - 1].is_ascii_alphanumeric();
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        start = abs + needle.len();
    }
    false
}

fn is_network_error(lower: &str) -> bool {
    lower.contains("econnrefused")
        || lower.contains("enotfound")
        || lower.contains("etimedout")
        || lower.contains("econnreset")
        || lower.contains("fetch failed")
        || lower.contains("network error")
        || lower.contains("network unreachable")
        || lower.contains("host unreachable")
        || lower.contains("connection refused")
        || lower.contains("could not connect")
}

fn is_auth_error(lower: &str) -> bool {
    contains_word(lower, "401")
        || contains_word(lower, "403")
        || contains_word(lower, "unauthorized")
        || lower.contains("invalid api key")
        || lower.contains("authentication failed")
}

/// Maps raw network or provider failure diagnostics into calm, person-facing copy.
pub fn calm_turn_error(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if is_network_error(&lower) {
        return "Could not reach the Model right now. Check your connection and try again.".into();
    }
    if is_auth_error(&lower) {
        return "The Model refused the current sign-in. Run `crowbar-agent login` in a terminal to sign in again.".into();
    }
    let clean = raw.trim();
    if clean.is_empty() {
        "The Model could not answer right now.".into()
    } else {
        clean.to_string()
    }
}

fn calm_retry_reason(raw: &str) -> String {
    let lower = raw.to_ascii_lowercase();
    if is_network_error(&lower) {
        "Could not reach the Model.".into()
    } else if is_auth_error(&lower) {
        "The Model refused the current sign-in.".into()
    } else {
        let clean = raw.trim();
        if clean.is_empty() {
            "The Model did not answer.".into()
        } else {
            clean.to_string()
        }
    }
}

/// Returns the display name where the floating fullscreen mark overlay should
/// appear, or `None` when it should be hidden. Prefers the active window if it
/// is fullscreen on the shown workspace, else the first fullscreen window there.
pub fn fullscreen_mark_overlay_output(
    mark: Mark,
    snapshot: &session::Snapshot,
    drawer_up: bool,
) -> Option<String> {
    if mark == Mark::Idle || drawer_up || snapshot.locked || !snapshot.connected {
        return None;
    }
    if snapshot.active.fullscreen && snapshot.active.workspace.id == snapshot.workspace {
        return Some(snapshot.active.output.clone());
    }
    snapshot
        .clients
        .iter()
        .find(|c| c.fullscreen && c.workspace.id == snapshot.workspace)
        .map(|c| c.output.clone())
}

/// Concise, readable lines summarizing the desktop context carried by a message.
pub fn format_context_summary(ctx: &DesktopContext) -> Vec<String> {
    let mut lines = Vec::new();
    let active = match &ctx.active {
        Some(win) => {
            let title = if win.title.trim().is_empty() {
                "(untitled)"
            } else {
                win.title.trim()
            };
            format!("{title} ({})", win.app_id)
        }
        None => "none".to_string(),
    };
    lines.push(format!("Workspace {} · Active: {active}", ctx.workspace));

    if !ctx.windows.is_empty() {
        let mut by_ws: BTreeMap<i32, Vec<String>> = BTreeMap::new();
        for win in &ctx.windows {
            let label = if win.title.trim().is_empty() {
                win.app_id.clone()
            } else {
                format!("{} ({})", win.title.trim(), win.app_id)
            };
            by_ws.entry(win.workspace).or_default().push(label);
        }
        let groups: Vec<String> = by_ws
            .into_iter()
            .map(|(ws, wins)| format!("W{ws}: {}", wins.join(", ")))
            .collect();
        lines.push(format!("Windows: {}", groups.join(" · ")));
    }

    if !ctx.displays.is_empty() {
        let displays: Vec<String> = ctx
            .displays
            .iter()
            .map(|d| {
                let pos = format!("({},{})", d.x, d.y);
                if d.scale == 120 {
                    format!("{} {}x{} at {pos}", d.name, d.width, d.height)
                } else {
                    let scale = d.scale as f32 / 120.0;
                    format!("{} {}x{} at {pos} @{:.2}x", d.name, d.width, d.height, scale)
                }
            })
            .collect();
        lines.push(format!("Displays: {}", displays.join(", ")));
    }
    lines
}

/// Encodes one framed JSON message (4-byte big-endian length + UTF-8 JSON).
fn encode_frame(message: &serde_json::Value) -> Vec<u8> {
    let bytes = message.to_string().into_bytes();
    let mut out = Vec::with_capacity(4 + bytes.len());
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(&bytes);
    out
}

/// Reads one framed JSON message from `stream`.
fn read_frame(stream: &mut UnixStream) -> Result<serde_json::Value> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME {
        bail!("Harness frame too long ({len} bytes)");
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    serde_json::from_slice(&body).context("Invalid JSON frame from Harness")
}

/// Translates one message from the Harness into an [`AgentEvent`].
pub fn parse_harness_message(msg: &serde_json::Value) -> Option<AgentEvent> {
    if let Some(reply) = msg.get("reply").and_then(|v| v.as_str()) {
        return match reply {
            "status" => {
                let model = msg
                    .get("model")
                    .filter(|v| !v.is_null())
                    .and_then(|v| serde_json::from_value::<ModelInfo>(v.clone()).ok());
                let missing = msg
                    .get("missing")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                Some(AgentEvent::Status { model, missing })
            }
            "sent" => Some(AgentEvent::Sent {
                conversation: msg.get("conversation")?.as_str()?.to_string(),
                turn: msg.get("turn")?.as_u64()?,
            }),
            "resumed" => {
                let conversation = msg.get("conversation")?.as_str()?.to_string();
                let turns = msg
                    .get("turns")?
                    .as_array()?
                    .iter()
                    .filter_map(parse_turn)
                    .collect();
                Some(AgentEvent::Resumed {
                    conversation,
                    turns,
                })
            }
            "error" => Some(AgentEvent::Error {
                code: msg
                    .get("code")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error")
                    .to_string(),
                message: msg
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("The Agent could not carry out that request.")
                    .to_string(),
            }),
            _ => None,
        };
    }

    let event = msg.get("event")?.as_str()?;
    let conversation = msg.get("conversation")?.as_str()?.to_string();
    let turn = msg.get("turn")?.as_u64()?;
    match event {
        "message" => {
            let text = msg.get("text")?.as_str()?.to_string();
            let context = msg
                .get("context")
                .filter(|v| !v.is_null())
                .and_then(|v| serde_json::from_value::<DesktopContext>(v.clone()).ok());
            Some(AgentEvent::Message {
                conversation,
                turn,
                text,
                context,
            })
        }
        "text" => Some(AgentEvent::Text {
            conversation,
            turn,
            text: msg.get("text")?.as_str()?.to_string(),
        }),
        "start-over" => Some(AgentEvent::StartOver { conversation, turn }),
        "retry" => Some(AgentEvent::Retry {
            conversation,
            turn,
            message: msg
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("The Model did not answer")
                .to_string(),
            seconds: msg.get("seconds").and_then(|v| v.as_u64()).unwrap_or(0),
        }),
        "done" => Some(AgentEvent::Done { conversation, turn }),
        "stopped" => Some(AgentEvent::Stopped { conversation, turn }),
        "failed" => Some(AgentEvent::Failed {
            conversation,
            turn,
            message: msg
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("The Model failed to answer.")
                .to_string(),
        }),
        _ => None,
    }
}

fn parse_turn(value: &serde_json::Value) -> Option<Turn> {
    let turn = value.get("turn")?.as_u64()?;
    let message = value.get("message")?.as_str()?.to_string();
    let answer = value
        .get("answer")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let context = value
        .get("context")
        .filter(|v| !v.is_null())
        .and_then(|v| serde_json::from_value::<DesktopContext>(v.clone()).ok());
    let state = match value.get("state").and_then(|v| v.as_str()) {
        Some("working") => TurnState::Working,
        Some("stopped") => TurnState::Stopped,
        Some("failed") => TurnState::Failed(calm_turn_error(
            value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("The turn failed."),
        )),
        _ => TurnState::Done,
    };
    Some(Turn {
        turn,
        message,
        context,
        answer,
        state,
        retry: None,
    })
}

/// Starts the background client for `CROWBAR_AGENT_SOCKET`, if configured.
pub fn start_from_env(events: session::Sender) -> Option<CommandSender> {
    let path = std::env::var_os("CROWBAR_AGENT_SOCKET").map(PathBuf::from)?;
    Some(start(path, events))
}

/// Starts the background client for a specific surface socket path.
pub fn start(path: PathBuf, events: session::Sender) -> CommandSender {
    let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>();
    let writer: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
    let active_conversation: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Writer thread: forwards UI commands to the currently connected stream.
    let writer_for_cmds = writer.clone();
    let events_for_cmds = events.clone();
    std::thread::spawn(move || {
        for cmd in cmd_rx {
            let request = match cmd {
                AgentCommand::Send { conversation, text } => match conversation {
                    Some(conversation) => {
                        serde_json::json!({ "op": "send", "conversation": conversation, "text": text })
                    }
                    None => serde_json::json!({ "op": "send", "text": text }),
                },
                AgentCommand::Stop { conversation, turn } => match turn {
                    Some(turn) => {
                        serde_json::json!({ "op": "stop", "conversation": conversation, "turn": turn })
                    }
                    None => serde_json::json!({ "op": "stop", "conversation": conversation }),
                },
                AgentCommand::RefreshStatus => serde_json::json!({ "op": "status" }),
            };
            let frame = encode_frame(&request);
            let write_result = {
                let mut guard = match writer_for_cmds.lock() {
                    Ok(g) => g,
                    Err(_) => break,
                };
                match guard.as_mut() {
                    Some(stream) => Some(stream.write_all(&frame)),
                    None => None,
                }
            };
            match write_result {
                Some(Ok(())) => {}
                Some(Err(e)) => {
                    log::warn!("Agent surface socket write failed: {e}");
                    if let Ok(mut guard) = writer_for_cmds.lock() {
                        if let Some(stream) = guard.take() {
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                        }
                    }
                    let _ = events_for_cmds.send(session::Event::Agent(AgentEvent::Disconnected));
                }
                None => {
                    if request.get("op").and_then(|v| v.as_str()) == Some("send") {
                        let _ = events_for_cmds.send(session::Event::Agent(AgentEvent::Error {
                            code: "disconnected".into(),
                            message: "The Agent is not reachable right now.".into(),
                        }));
                    }
                }
            }
        }
    });

    // Reader / reconnect thread: connects with exponential backoff and blocks on reads.
    std::thread::spawn(move || {
        let mut backoff = INITIAL_BACKOFF;
        loop {
            match connect_and_handshake(&path) {
                Ok(mut stream) => {
                    backoff = INITIAL_BACKOFF;
                    let Ok(mut write_half) = stream.try_clone() else {
                        std::thread::sleep(backoff);
                        continue;
                    };
                    let _ = write_half.set_write_timeout(Some(Duration::from_secs(5)));
                    // Ask status immediately, and resume our active conversation if we had one.
                    let resume_id = active_conversation.lock().ok().and_then(|g| g.clone());
                    let mut init_ok = write_half
                        .write_all(&encode_frame(&serde_json::json!({ "op": "status" })))
                        .is_ok();
                    if init_ok {
                        if let Some(id) = resume_id {
                            init_ok = write_half
                                .write_all(&encode_frame(
                                    &serde_json::json!({ "op": "resume", "conversation": id }),
                                ))
                                .is_ok();
                        }
                    }
                    if !init_ok {
                        std::thread::sleep(backoff);
                        continue;
                    }
                    if let Ok(mut guard) = writer.lock() {
                        *guard = Some(write_half);
                    }
                    log::info!("SHELL agent-socket connected=true");
                    if events
                        .send(session::Event::Agent(AgentEvent::Connected))
                        .is_err()
                    {
                        break;
                    }
                    loop {
                        match read_frame(&mut stream) {
                            Ok(message) => {
                                if let Some(event) = parse_harness_message(&message) {
                                    match &event {
                                        AgentEvent::Sent { conversation, .. }
                                        | AgentEvent::Resumed { conversation, .. }
                                        | AgentEvent::Message { conversation, .. } => {
                                            if let Ok(mut guard) = active_conversation.lock() {
                                                *guard = Some(conversation.clone());
                                            }
                                        }
                                        AgentEvent::Error { code, .. }
                                            if code == "no-conversation" =>
                                        {
                                            if let Ok(mut guard) = active_conversation.lock() {
                                                *guard = None;
                                            }
                                        }
                                        _ => {}
                                    }
                                    if events.send(session::Event::Agent(event)).is_err() {
                                        return;
                                    }
                                }
                            }
                            Err(e) => {
                                log::info!("SHELL agent-socket connected=false reason={e}");
                                break;
                            }
                        }
                    }
                    if let Ok(mut guard) = writer.lock() {
                        *guard = None;
                    }
                    if events
                        .send(session::Event::Agent(AgentEvent::Disconnected))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => {}
            }
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    });

    CommandSender { tx: cmd_tx }
}

fn connect_and_handshake(path: &PathBuf) -> Result<UnixStream> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(&encode_frame(
        &serde_json::json!({ "version": SURFACE_VERSION }),
    ))?;
    let reply = read_frame(&mut stream)?;
    if reply != serde_json::json!({ "version": SURFACE_VERSION }) {
        bail!("Harness did not speak surface protocol version {SURFACE_VERSION}");
    }
    // Clear read timeout so the idle reader thread blocks with zero wakeups.
    stream.set_read_timeout(None)?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::os::unix::net::UnixListener;

    fn sample_context() -> DesktopContext {
        DesktopContext {
            workspace: 2,
            active: Some(DesktopWindow {
                id: "0x2".into(),
                app_id: "foot".into(),
                title: "work".into(),
                workspace: 2,
            }),
            locked: false,
            windows: vec![
                DesktopWindow {
                    id: "0x1".into(),
                    app_id: "com.crowbaros.StudioSettings".into(),
                    title: "Settings".into(),
                    workspace: 1,
                },
                DesktopWindow {
                    id: "0x2".into(),
                    app_id: "foot".into(),
                    title: "work".into(),
                    workspace: 2,
                },
            ],
            displays: vec![DesktopDisplay {
                name: "Virtual-1".into(),
                width: 1280,
                height: 960,
                x: 0,
                y: 0,
                scale: 120,
            }],
        }
    }

    #[test]
    fn turn_streaming_and_mark_transitions_behind_drawer() {
        let mut state = AgentState::new(true);
        assert_eq!(state.mark, Mark::Idle);

        state.apply(AgentEvent::Connected, true);
        state.apply(
            AgentEvent::Status {
                model: Some(ModelInfo {
                    provider: "endpoint".into(),
                    provider_name: "Endpoint".into(),
                    model: "stub-1".into(),
                    name: "stub-1".into(),
                }),
                missing: None,
            },
            true,
        );
        assert!(state.connected);
        assert!(state.model.is_some());

        // Person sends a message while the drawer is up.
        state.apply(
            AgentEvent::Sent {
                conversation: "abc12345".into(),
                turn: 1,
            },
            true,
        );
        assert_eq!(state.mark, Mark::Working);
        state.apply(
            AgentEvent::Message {
                conversation: "abc12345".into(),
                turn: 1,
                text: "What is focused?".into(),
                context: Some(sample_context()),
            },
            true,
        );
        state.apply(
            AgentEvent::Text {
                conversation: "abc12345".into(),
                turn: 1,
                text: "The active ".into(),
            },
            true,
        );

        // Putting the drawer away stops nothing and keeps Working.
        assert!(!state.on_drawer_closed());
        assert_eq!(state.mark, Mark::Working);

        // Answer finishes behind the drawer -> mark shows Done.
        state.apply(
            AgentEvent::Text {
                conversation: "abc12345".into(),
                turn: 1,
                text: "window is foot.".into(),
            },
            false,
        );
        state.apply(
            AgentEvent::Done {
                conversation: "abc12345".into(),
                turn: 1,
            },
            false,
        );
        assert_eq!(state.mark, Mark::Done);
        assert_eq!(state.turns.len(), 1);
        assert_eq!(state.turns[0].answer, "The active window is foot.");
        assert_eq!(
            state.turns[0]
                .context
                .as_ref()
                .and_then(|c| c.active.as_ref())
                .map(|w| w.app_id.as_str()),
            Some("foot")
        );

        // Summoning the drawer acknowledges Done -> Idle.
        assert!(state.on_drawer_opened());
        assert_eq!(state.mark, Mark::Idle);
    }

    #[test]
    fn fullscreen_mark_overlay_visibility_and_output_rules() {
        let mut snap = session::Snapshot {
            workspace: 1,
            connected: true,
            locked: false,
            clients: vec![session::Client {
                address: "0x1".into(),
                class: "foot".into(),
                title: "foot".into(),
                workspace: session::Workspace { id: 1 },
                attention: false,
                fullscreen: true,
                output: "Virtual-2".into(),
            }],
            ..Default::default()
        };
        assert_eq!(fullscreen_mark_overlay_output(Mark::Idle, &snap, false), None);
        assert_eq!(
            fullscreen_mark_overlay_output(Mark::Working, &snap, false).as_deref(),
            Some("Virtual-2")
        );
        assert_eq!(
            fullscreen_mark_overlay_output(Mark::Done, &snap, false).as_deref(),
            Some("Virtual-2")
        );
        // Hidden while drawer is up or desktop is locked.
        assert_eq!(fullscreen_mark_overlay_output(Mark::Working, &snap, true), None);
        snap.locked = true;
        assert_eq!(fullscreen_mark_overlay_output(Mark::Working, &snap, false), None);
        // Hidden when fullscreen window is on another workspace.
        snap.locked = false;
        snap.workspace = 2;
        assert_eq!(fullscreen_mark_overlay_output(Mark::Working, &snap, false), None);
    }

    #[test]
    fn rapid_send_guard_no_conversation_recovery_and_behind_drawer_failure() {
        let mut state = AgentState::new(true);
        state.apply(AgentEvent::Connected, true);
        assert!(state.can_send());
        // In-flight submission blocks duplicate sends until Sent or Error.
        state.sending = true;
        assert!(!state.can_send());
        state.apply(
            AgentEvent::Sent {
                conversation: "conv1".into(),
                turn: 1,
            },
            true,
        );
        assert!(!state.sending);
        assert!(!state.can_send()); // Working now.

        // A failure while the drawer is put away leaves Mark::Done so the person notices.
        state.apply(
            AgentEvent::Message {
                conversation: "conv1".into(),
                turn: 1,
                text: "hi".into(),
                context: None,
            },
            false,
        );
        state.apply(
            AgentEvent::Failed {
                conversation: "conv1".into(),
                turn: 1,
                message: "TypeError: fetch failed (ECONNREFUSED)".into(),
            },
            false,
        );
        assert_eq!(state.mark, Mark::Done);
        match &state.turns[0].state {
            TurnState::Failed(msg) => assert!(msg.contains("Could not reach the Model"), "{msg}"),
            other => panic!("expected Failed, got {other:?}"),
        }

        // Lost conversation clears stale state and unlocks composer.
        state.mark = Mark::Working;
        state.apply(
            AgentEvent::Error {
                code: "no-conversation".into(),
                message: "There is no conversation conv1.".into(),
            },
            true,
        );
        assert_eq!(state.conversation, None);
        assert!(state.turns.is_empty());
        assert_eq!(state.mark, Mark::Idle);
        assert!(state.can_send());

        // Numbers containing 401/403 must not be misclassified as sign-in errors.
        let token_err = "prompt is too long: 204031 tokens > 200000 maximum (140123 input)";
        assert_eq!(calm_turn_error(token_err), token_err);
        // Word-bounded 401/403 status codes map calmly to sign-in guidance.
        assert!(calm_turn_error("HTTP 401 Unauthorized").contains("crowbar-agent login"));
        // Resumed failed turns apply the same calm mapping.
        let resumed = parse_turn(&serde_json::json!({
            "turn": 1,
            "message": "hi",
            "answer": "",
            "state": "failed",
            "error": "TypeError: fetch failed (ECONNREFUSED)"
        }))
        .unwrap();
        match resumed.state {
            TurnState::Failed(msg) => assert!(msg.contains("Could not reach the Model"), "{msg}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn context_summary_lists_active_windows_and_displays() {
        let lines = format_context_summary(&sample_context());
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("Workspace 2"));
        assert!(lines[0].contains("work (foot)"));
        assert!(lines[1].contains("W1: Settings (com.crowbaros.StudioSettings)"));
        assert!(lines[1].contains("W2: work (foot)"));
        assert!(lines[2].contains("Virtual-1 1280x960"));
    }

    #[test]
    fn surface_client_speaks_framed_protocol_and_streams_events() {
        let dir = std::env::temp_dir().join(format!("crowbar-shell-agent-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("surface");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            let v = read_frame(&mut peer).unwrap();
            assert_eq!(v, serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 })))
                .unwrap();

            // Status request.
            let status_req = read_frame(&mut peer).unwrap();
            assert_eq!(status_req["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "model": { "provider": "endpoint", "provider_name": "Endpoint", "model": "stub-1", "name": "stub-1" },
                "signed_in": []
            })))
            .unwrap();

            // Send request.
            let send_req = read_frame(&mut peer).unwrap();
            assert_eq!(send_req["op"], "send");
            assert_eq!(send_req["text"], "Which app is focused?");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "sent",
                "conversation": "conv1234",
                "turn": 1
            })))
            .unwrap();
            peer.write_all(&encode_frame(&serde_json::json!({
                "event": "message",
                "conversation": "conv1234",
                "turn": 1,
                "text": "Which app is focused?",
                "context": {
                    "workspace": 1,
                    "active": { "id": "0x1", "app_id": "foot", "title": "foot", "workspace": 1 },
                    "locked": false,
                    "windows": [{ "id": "0x1", "app_id": "foot", "title": "foot", "workspace": 1 }],
                    "displays": [{ "name": "Virtual-1", "width": 1280, "height": 960, "x": 0, "y": 0, "scale": 120 }]
                }
            })))
            .unwrap();
            peer.write_all(&encode_frame(&serde_json::json!({
                "event": "text",
                "conversation": "conv1234",
                "turn": 1,
                "text": "foot"
            })))
            .unwrap();
            peer.write_all(&encode_frame(&serde_json::json!({
                "event": "done",
                "conversation": "conv1234",
                "turn": 1
            })))
            .unwrap();
        });

        let (tx, mut rx) = session::channel();
        let sender = start(sock.clone(), tx);

        let mut state = AgentState::new(true);
        futures::executor::block_on(async {
            let mut sent_msg = false;
            while let Some(ev) = rx.next().await {
                if let session::Event::Agent(agent_ev) = ev {
                    state.apply(agent_ev, true);
                    if state.connected && state.model.is_some() && !sent_msg {
                        sent_msg = true;
                        sender.send_message(None, "Which app is focused?".into());
                    }
                    if state.mark == Mark::Done {
                        break;
                    }
                }
            }
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(state.conversation.as_deref(), Some("conv1234"));
        assert_eq!(state.turns.len(), 1);
        assert_eq!(state.turns[0].answer, "foot");
        assert_eq!(
            state.turns[0]
                .context
                .as_ref()
                .and_then(|c| c.active.as_ref())
                .map(|w| w.app_id.as_str()),
            Some("foot")
        );
    }
}
