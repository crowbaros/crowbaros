// SPDX-License-Identifier: MIT
//! The shell's client of Lever's surface socket (ADR 0036).
//!
//! Speaks Lever's version-1 framing (4-byte big-endian length + UTF-8 JSON,
//! `{"version":1}` first) over `CROWBAR_LEVER_SOCKET`, holding the active
//! thread's turns, acts, notes, desktop context, and the mark (`idle`,
//! `working`, `done`). Blocking I/O on dedicated background threads wakes GPUI
//! only when a frame arrives, so a still desktop draws nothing.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use crate::session;

/// Protocol version spoken on Lever's surface socket.
const SURFACE_VERSION: u32 = 1;
/// Longest frame accepted from Lever (64 MiB, matching `crowbar-agent`, so
/// `resume` replies carrying long threads are not truncated).
const MAX_FRAME: usize = 64 * 1024 * 1024;
const INITIAL_BACKOFF: Duration = Duration::from_millis(200);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// Deadline for handshake + initial `status` / `list` / `resume` before
/// reporting `Connected` to the drawer.
const INIT_DEADLINE: Duration = Duration::from_secs(5);

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActState {
    Pending,
    Working,
    Done,
    Failed,
}

impl ActState {
    pub fn label(self) -> &'static str {
        match self {
            ActState::Pending => "Pending",
            ActState::Working => "Working…",
            ActState::Done => "Done",
            ActState::Failed => "Failed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Act {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub state: ActState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Act {
    pub fn kind_label(&self) -> &'static str {
        match self.kind.as_str() {
            "read" => "Read",
            "edit" => "Edit",
            "delete" => "Delete",
            "move" => "Move",
            "search" => "Search",
            "command" => "Command",
            "think" => "Think",
            "fetch" => "Fetch",
            "mode" => "Mode",
            _ => "Act",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "message", rename_all = "lowercase")]
pub enum TurnState {
    Working,
    Done,
    Stopped,
    Interrupted,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub turn: u64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<DesktopContext>,
    #[serde(default)]
    pub acts: Vec<Act>,
    #[serde(default)]
    pub notes: Vec<Note>,
    pub answer: String,
    pub state: TurnState,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusInfo {
    pub lever_version: String,
    pub harness_started: bool,
    pub harnesses: Vec<String>,
    pub free_hosted_models: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub thread: String,
    pub title: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub started: i64,
    pub updated: i64,
    pub turns: u64,
    pub state: String,
    pub working: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumedThread {
    pub thread: String,
    pub title: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub working: bool,
    pub turns: Vec<Turn>,
}

/// Every reply or event Lever sends on its surface socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeverMessage {
    Status(StatusInfo),
    List { threads: Vec<ThreadSummary> },
    Resumed(ResumedThread),
    Sent { thread: String, turn: u64 },
    Ok,
    Error { code: String, message: String },
    Message {
        thread: String,
        turn: u64,
        text: String,
        context: Option<DesktopContext>,
    },
    Text {
        thread: String,
        turn: u64,
        text: String,
    },
    Act {
        thread: String,
        turn: u64,
        act: Act,
    },
    Note {
        thread: String,
        turn: u64,
        note: Note,
    },
    ThreadUpdated {
        thread: String,
        title: String,
    },
    Done {
        thread: String,
        turn: u64,
    },
    Stopped {
        thread: String,
        turn: u64,
    },
    Failed {
        thread: String,
        turn: u64,
        message: String,
    },
    Interrupted {
        thread: String,
        turn: u64,
    },
}

/// Events sent from the surface-socket client to the shell's UI loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEvent {
    /// Emitted only after Lever's initial `status` (and `list`/`resume`, if
    /// applicable) have arrived within the bootstrap deadline.
    Connected {
        status: StatusInfo,
        resumed: Option<ResumedThread>,
    },
    Disconnected,
    Status(StatusInfo),
    Resumed(ResumedThread),
    Sent {
        thread: String,
        turn: u64,
    },
    Message {
        thread: String,
        turn: u64,
        text: String,
        context: Option<DesktopContext>,
    },
    Text {
        thread: String,
        turn: u64,
        text: String,
    },
    Act {
        thread: String,
        turn: u64,
        act: Act,
    },
    Note {
        thread: String,
        turn: u64,
        note: Note,
    },
    ThreadUpdated {
        thread: String,
        title: String,
    },
    Done {
        thread: String,
        turn: u64,
    },
    Stopped {
        thread: String,
        turn: u64,
    },
    Failed {
        thread: String,
        turn: u64,
        message: String,
    },
    Interrupted {
        thread: String,
        turn: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

impl LeverMessage {
    fn into_agent_event(self) -> Option<AgentEvent> {
        match self {
            LeverMessage::Status(s) => Some(AgentEvent::Status(s)),
            LeverMessage::Resumed(r) => Some(AgentEvent::Resumed(r)),
            LeverMessage::Sent { thread, turn } => Some(AgentEvent::Sent { thread, turn }),
            LeverMessage::Message {
                thread,
                turn,
                text,
                context,
            } => Some(AgentEvent::Message {
                thread,
                turn,
                text,
                context,
            }),
            LeverMessage::Text { thread, turn, text } => Some(AgentEvent::Text { thread, turn, text }),
            LeverMessage::Act { thread, turn, act } => Some(AgentEvent::Act { thread, turn, act }),
            LeverMessage::Note { thread, turn, note } => Some(AgentEvent::Note { thread, turn, note }),
            LeverMessage::ThreadUpdated { thread, title } => {
                Some(AgentEvent::ThreadUpdated { thread, title })
            }
            LeverMessage::Done { thread, turn } => Some(AgentEvent::Done { thread, turn }),
            LeverMessage::Stopped { thread, turn } => Some(AgentEvent::Stopped { thread, turn }),
            LeverMessage::Failed {
                thread,
                turn,
                message,
            } => Some(AgentEvent::Failed {
                thread,
                turn,
                message,
            }),
            LeverMessage::Interrupted { thread, turn } => {
                Some(AgentEvent::Interrupted { thread, turn })
            }
            LeverMessage::Error { code, message } => Some(AgentEvent::Error { code, message }),
            LeverMessage::List { .. } | LeverMessage::Ok => None,
        }
    }
}

/// Commands sent from the shell's UI to the surface-socket background thread.
#[derive(Clone, Debug)]
pub enum AgentCommand {
    Send {
        thread: Option<String>,
        text: String,
    },
    Stop {
        thread: String,
        turn: Option<u64>,
    },
    RefreshStatus,
}

#[derive(Clone)]
pub struct CommandSender {
    tx: mpsc::Sender<AgentCommand>,
}

impl CommandSender {
    pub fn send_message(&self, thread: Option<String>, text: String) {
        let _ = self.tx.send(AgentCommand::Send { thread, text });
    }

    pub fn stop_turn(&self, thread: String, turn: Option<u64>) {
        let _ = self.tx.send(AgentCommand::Stop { thread, turn });
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
    pub lever_version: Option<String>,
    pub harness_started: bool,
    pub harnesses: Vec<String>,
    pub free_hosted_models: bool,
    pub no_harness: bool,
    pub sending: bool,
    pub model: Option<String>,
    pub missing_model: Option<String>,
    pub thread: Option<String>,
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_turn: Option<u64>,
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
        self.connected && !self.working() && !self.sending && !self.no_harness
    }

    pub fn current_turn(&self) -> Option<u64> {
        self.active_turn.or_else(|| {
            self.turns
                .last()
                .filter(|t| matches!(t.state, TurnState::Working))
                .map(|t| t.turn)
        })
    }

    fn apply_status(&mut self, status: StatusInfo) -> bool {
        let no_harness = status.harnesses.is_empty();
        let lever_version = Some(status.lever_version);
        let changed = self.lever_version != lever_version
            || self.harness_started != status.harness_started
            || self.harnesses != status.harnesses
            || self.free_hosted_models != status.free_hosted_models
            || self.no_harness != no_harness;
        self.lever_version = lever_version;
        self.harness_started = status.harness_started;
        self.harnesses = status.harnesses;
        self.free_hosted_models = status.free_hosted_models;
        self.no_harness = no_harness;
        if self.no_harness {
            self.error = None;
        }
        changed
    }

    fn apply_resumed(&mut self, resumed: ResumedThread, drawer_up: bool) {
        self.sending = false;
        self.thread = Some(resumed.thread);
        self.title = if resumed.title.is_empty() {
            None
        } else {
            Some(resumed.title)
        };
        self.model = resumed.model;
        self.active_turn = resumed
            .turns
            .last()
            .filter(|t| matches!(t.state, TurnState::Working))
            .map(|t| t.turn);
        let next_mark = match resumed.turns.last().map(|t| &t.state) {
            Some(TurnState::Working) => Mark::Working,
            Some(TurnState::Done) if self.mark == Mark::Working => Mark::Done,
            Some(TurnState::Failed(_) | TurnState::Interrupted)
                if self.mark == Mark::Working && !drawer_up =>
            {
                Mark::Done
            }
            _ if self.mark == Mark::Working => Mark::Idle,
            _ => self.mark,
        };
        self.turns = resumed.turns;
        self.mark = next_mark;
    }

    /// Applies an event from Lever. Returns true if visible state changed.
    pub fn apply(&mut self, event: AgentEvent, drawer_up: bool) -> bool {
        match event {
            AgentEvent::Connected { status, resumed } => {
                let mut changed = !self.connected || self.error.is_some();
                self.connected = true;
                self.sending = false;
                self.error = None;
                changed |= self.apply_status(status);
                if let Some(resumed) = resumed {
                    self.apply_resumed(resumed, drawer_up);
                    changed = true;
                } else if self.thread.is_some() {
                    self.thread = None;
                    self.title = None;
                    self.model = None;
                    self.active_turn = None;
                    self.turns.clear();
                    self.mark = Mark::Idle;
                    changed = true;
                }
                changed
            }
            AgentEvent::Disconnected => {
                let changed = self.connected
                    || self.sending
                    || self.lever_version.is_some()
                    || self.harness_started
                    || !self.harnesses.is_empty()
                    || self.no_harness;
                self.connected = false;
                self.sending = false;
                self.active_turn = None;
                self.lever_version = None;
                self.harness_started = false;
                self.harnesses.clear();
                self.free_hosted_models = false;
                self.no_harness = false;
                changed
            }
            AgentEvent::Status(status) => self.apply_status(status),
            AgentEvent::Resumed(resumed) => {
                self.apply_resumed(resumed, drawer_up);
                true
            }
            AgentEvent::Sent { thread, turn } => {
                self.sending = false;
                if self.thread.as_deref() != Some(thread.as_str()) {
                    self.turns.clear();
                    self.title = None;
                    self.model = None;
                }
                self.thread = Some(thread);
                self.active_turn = Some(turn);
                self.error = None;
                self.missing_model = None;
                self.mark = Mark::Working;
                true
            }
            AgentEvent::Message {
                thread,
                turn,
                text,
                context,
            } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    if self.thread.is_none() {
                        self.thread = Some(thread);
                    } else {
                        return false;
                    }
                }
                self.sending = false;
                self.active_turn = Some(turn);
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.message = text;
                    existing.context = context;
                } else {
                    self.turns.push(Turn {
                        turn,
                        message: text,
                        context,
                        acts: Vec::new(),
                        notes: Vec::new(),
                        answer: String::new(),
                        state: TurnState::Working,
                    });
                }
                self.error = None;
                self.mark = Mark::Working;
                true
            }
            AgentEvent::Text { thread, turn, text } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.answer.push_str(&text);
                    return true;
                }
                false
            }
            AgentEvent::Act { thread, turn, act } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                if let Some(existing_turn) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    if let Some(existing_act) =
                        existing_turn.acts.iter_mut().find(|a| a.id == act.id)
                    {
                        *existing_act = act;
                    } else {
                        existing_turn.acts.push(act);
                    }
                    return true;
                }
                false
            }
            AgentEvent::Note { thread, turn, note } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                if let Some(existing_turn) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    if let Some(existing_note) =
                        existing_turn.notes.iter_mut().find(|n| n.id == note.id)
                    {
                        existing_note.text = note.text;
                    } else {
                        existing_turn.notes.push(note);
                    }
                    return true;
                }
                false
            }
            AgentEvent::ThreadUpdated { thread, title } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                let next = if title.is_empty() { None } else { Some(title) };
                if self.title != next {
                    self.title = next;
                    true
                } else {
                    false
                }
            }
            AgentEvent::Done { thread, turn } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.state = TurnState::Done;
                }
                if self.active_turn == Some(turn) {
                    self.active_turn = None;
                }
                self.mark = Mark::Done;
                true
            }
            AgentEvent::Stopped { thread, turn } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.state = TurnState::Stopped;
                }
                if self.active_turn == Some(turn) {
                    self.active_turn = None;
                }
                self.mark = Mark::Idle;
                true
            }
            AgentEvent::Failed {
                thread,
                turn,
                message,
            } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                self.sending = false;
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.state = TurnState::Failed(calm_turn_error(&message));
                }
                if self.active_turn == Some(turn) {
                    self.active_turn = None;
                }
                // Behind a put-away drawer, leave the mark lit as Done so the
                // person knows the turn ended and can open the drawer to read why.
                self.mark = if drawer_up { Mark::Idle } else { Mark::Done };
                true
            }
            AgentEvent::Interrupted { thread, turn } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                self.sending = false;
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.state = TurnState::Interrupted;
                }
                if self.active_turn == Some(turn) {
                    self.active_turn = None;
                }
                self.mark = if drawer_up { Mark::Idle } else { Mark::Done };
                true
            }
            AgentEvent::Error { code, message } => {
                self.sending = false;
                if code == "no-model" {
                    self.missing_model = Some(message);
                    self.error = None;
                } else if code == "no-thread" {
                    self.thread = None;
                    self.title = None;
                    self.model = None;
                    self.active_turn = None;
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
        || lower.contains("eai_again")
        || lower.contains("enetunreach")
        || lower.contains("ehostunreach")
        || lower.contains("ehostdown")
        || lower.contains("etimedout")
        || lower.contains("econnreset")
        || lower.contains("fetch failed")
        || lower.contains("connection error")
        || lower.contains("connect timeout")
        || lower.contains("socket hang up")
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
        || lower.contains("authentication_error")
        || lower.contains("api key not valid")
        || lower.contains("invalid api key")
        || lower.contains("authentication failed")
}

/// Computes the composer contents after `Sent` acknowledges `submitted`.
/// Clears an unchanged draft, strips the submitted prefix when the person
/// typed ahead before the reply arrived, and leaves an edited draft alone.
pub fn draft_after_send(current: &str, submitted: &str) -> Option<String> {
    if current == submitted {
        Some(String::new())
    } else if let Some(rest) = current.strip_prefix(submitted) {
        Some(rest.trim_start().to_string())
    } else {
        None
    }
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

/// Truncates long act detail output to a bounded number of lines and characters
/// for readable rendering in the drawer.
pub fn format_act_detail(raw: &str) -> String {
    const MAX_LINES: usize = 12;
    const MAX_CHARS: usize = 800;
    let trimmed = raw.trim();
    let mut out = String::new();
    let mut truncated = false;
    for (idx, line) in trimmed.lines().enumerate() {
        if idx >= MAX_LINES {
            truncated = true;
            break;
        }
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(line);
    }
    if out.chars().count() > MAX_CHARS {
        out = out.chars().take(MAX_CHARS).collect();
        truncated = true;
    }
    if truncated {
        out.push('…');
    }
    out
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
        bail!("Lever frame too long ({len} bytes)");
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    serde_json::from_slice(&body).context("Invalid JSON frame from Lever")
}

fn read_frame_before(stream: &mut UnixStream, deadline: Instant) -> Result<serde_json::Value> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        bail!("Timed out waiting for Lever");
    }
    stream.set_read_timeout(Some(remaining))?;
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME {
        bail!("Lever frame too long ({len} bytes)");
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        bail!("Timed out waiting for Lever frame body");
    }
    stream.set_read_timeout(Some(remaining))?;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    serde_json::from_slice(&body).context("Invalid JSON frame from Lever")
}

fn write_frame_before(
    stream: &mut UnixStream,
    message: &serde_json::Value,
    deadline: Instant,
) -> Result<()> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        bail!("Timed out writing to Lever");
    }
    stream.set_write_timeout(Some(remaining))?;
    stream.write_all(&encode_frame(message))?;
    Ok(())
}

/// Translates one message from Lever's surface socket into a [`LeverMessage`].
pub fn parse_lever_message(msg: &serde_json::Value) -> Option<LeverMessage> {
    if let Some(reply) = msg.get("reply").and_then(|v| v.as_str()) {
        return match reply {
            "status" => {
                let lever_version = msg.get("version")?.as_str()?.to_string();
                let harness_started = msg
                    .get("harness_started")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let harnesses = msg
                    .get("harnesses")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| {
                                item.as_str()
                                    .or_else(|| item.get("name").and_then(|n| n.as_str()))
                                    .map(str::to_string)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let free_hosted_models = msg
                    .get("free_hosted_models")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                Some(LeverMessage::Status(StatusInfo {
                    lever_version,
                    harness_started,
                    harnesses,
                    free_hosted_models,
                }))
            }
            "list" => {
                let threads = msg
                    .get("threads")?
                    .as_array()?
                    .iter()
                    .filter_map(parse_thread_summary)
                    .collect();
                Some(LeverMessage::List { threads })
            }
            "resumed" => {
                let thread = msg.get("thread")?.as_str()?.to_string();
                let title = msg
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let cwd = msg
                    .get("cwd")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let model = msg
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let working = msg
                    .get("working")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let turns = msg
                    .get("turns")?
                    .as_array()?
                    .iter()
                    .filter_map(parse_turn)
                    .collect();
                Some(LeverMessage::Resumed(ResumedThread {
                    thread,
                    title,
                    cwd,
                    model,
                    working,
                    turns,
                }))
            }
            "sent" => Some(LeverMessage::Sent {
                thread: msg.get("thread")?.as_str()?.to_string(),
                turn: msg.get("turn")?.as_u64()?,
            }),
            "ok" => Some(LeverMessage::Ok),
            "error" => Some(LeverMessage::Error {
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
    let thread = msg.get("thread")?.as_str()?.to_string();
    if event == "thread-updated" {
        let title = msg.get("title")?.as_str()?.to_string();
        return Some(LeverMessage::ThreadUpdated { thread, title });
    }
    let turn = msg.get("turn")?.as_u64()?;
    match event {
        "message" => {
            let text = msg.get("text")?.as_str()?.to_string();
            let context = msg
                .get("context")
                .filter(|v| !v.is_null())
                .and_then(|v| serde_json::from_value::<DesktopContext>(v.clone()).ok());
            Some(LeverMessage::Message {
                thread,
                turn,
                text,
                context,
            })
        }
        "text" => Some(LeverMessage::Text {
            thread,
            turn,
            text: msg.get("text")?.as_str()?.to_string(),
        }),
        "act" => Some(LeverMessage::Act {
            thread,
            turn,
            act: parse_act(msg)?,
        }),
        "note" => Some(LeverMessage::Note {
            thread,
            turn,
            note: parse_note(msg)?,
        }),
        "done" => Some(LeverMessage::Done { thread, turn }),
        "stopped" => Some(LeverMessage::Stopped { thread, turn }),
        "failed" => Some(LeverMessage::Failed {
            thread,
            turn,
            message: msg
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("The Agent failed to answer.")
                .to_string(),
        }),
        "interrupted" => Some(LeverMessage::Interrupted { thread, turn }),
        _ => None,
    }
}

fn parse_thread_summary(value: &serde_json::Value) -> Option<ThreadSummary> {
    Some(ThreadSummary {
        thread: value.get("thread")?.as_str()?.to_string(),
        title: value
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        cwd: value
            .get("cwd")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        model: value
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        started: value.get("started").and_then(|v| v.as_i64()).unwrap_or(0),
        updated: value.get("updated").and_then(|v| v.as_i64()).unwrap_or(0),
        turns: value.get("turns").and_then(|v| v.as_u64()).unwrap_or(0),
        state: value
            .get("state")
            .and_then(|v| v.as_str())
            .unwrap_or("idle")
            .to_string(),
        working: value
            .get("working")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    })
}

fn parse_act(value: &serde_json::Value) -> Option<Act> {
    let id = value.get("act")?.as_str()?.to_string();
    let kind = value
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("other")
        .to_string();
    let title = value
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Act")
        .to_string();
    let state = match value.get("state").and_then(|v| v.as_str()) {
        Some("working") => ActState::Working,
        Some("done") => ActState::Done,
        Some("failed") => ActState::Failed,
        _ => ActState::Pending,
    };
    let detail = value
        .get("detail")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some(Act {
        id,
        kind,
        title,
        state,
        detail,
    })
}

fn parse_note(value: &serde_json::Value) -> Option<Note> {
    Some(Note {
        id: value.get("note")?.as_str()?.to_string(),
        text: value.get("text")?.as_str()?.to_string(),
    })
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
    let acts = value
        .get("acts")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_act).collect())
        .unwrap_or_default();
    let notes = value
        .get("notes")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_note).collect())
        .unwrap_or_default();
    let state = match value.get("state").and_then(|v| v.as_str()) {
        Some("working") => TurnState::Working,
        Some("stopped") => TurnState::Stopped,
        Some("interrupted") => TurnState::Interrupted,
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
        acts,
        notes,
        answer,
        state,
    })
}

/// Starts the background client for `CROWBAR_LEVER_SOCKET`, if configured.
pub fn start_from_env(events: session::Sender) -> Option<CommandSender> {
    let path = std::env::var_os("CROWBAR_LEVER_SOCKET").map(PathBuf::from)?;
    Some(start(path, events))
}

/// Starts the background client for a specific surface socket path.
pub fn start(path: PathBuf, events: session::Sender) -> CommandSender {
    start_with_deadline(path, events, INIT_DEADLINE)
}

fn start_with_deadline(
    path: PathBuf,
    events: session::Sender,
    init_deadline: Duration,
) -> CommandSender {
    let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>();
    let writer: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
    let active_thread: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Writer thread: forwards UI commands to the currently connected stream.
    let writer_for_cmds = writer.clone();
    let events_for_cmds = events.clone();
    std::thread::spawn(move || {
        for cmd in cmd_rx {
            let request = match cmd {
                AgentCommand::Send { thread, text } => match thread {
                    Some(thread) => {
                        serde_json::json!({ "op": "send", "thread": thread, "text": text })
                    }
                    None => serde_json::json!({ "op": "send", "thread": null, "text": text }),
                },
                AgentCommand::Stop { thread, turn } => match turn {
                    Some(turn) => {
                        serde_json::json!({ "op": "stop", "thread": thread, "turn": turn })
                    }
                    None => serde_json::json!({ "op": "stop", "thread": thread }),
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

    // Reader / reconnect thread: connects with exponential backoff, holds
    // `Connected` until initial `status` (and thread restore) arrive within
    // `init_deadline`, and then blocks on live reads with zero idle wakeups.
    std::thread::spawn(move || {
        let mut backoff = INITIAL_BACKOFF;
        loop {
            let existing_thread = active_thread.lock().ok().and_then(|g| g.clone());
            match connect_and_bootstrap(&path, existing_thread, init_deadline) {
                Ok((mut stream, status, resumed)) => {
                    backoff = INITIAL_BACKOFF;
                    let Ok(write_half) = stream.try_clone() else {
                        std::thread::sleep(backoff);
                        continue;
                    };
                    let _ = write_half.set_write_timeout(Some(Duration::from_secs(5)));
                    if let Ok(mut guard) = active_thread.lock() {
                        *guard = resumed.as_ref().map(|r| r.thread.clone());
                    }
                    if let Ok(mut guard) = writer.lock() {
                        *guard = Some(write_half);
                    }
                    log::info!("SHELL agent-socket connected=true");
                    if events
                        .send(session::Event::Agent(AgentEvent::Connected {
                            status,
                            resumed,
                        }))
                        .is_err()
                    {
                        break;
                    }
                    loop {
                        match read_frame(&mut stream) {
                            Ok(message) => {
                                if let Some(parsed) = parse_lever_message(&message) {
                                    if let Some(event) = parsed.into_agent_event() {
                                        match &event {
                                            AgentEvent::Sent { thread, .. }
                                            | AgentEvent::Message { thread, .. } => {
                                                if let Ok(mut guard) = active_thread.lock() {
                                                    *guard = Some(thread.clone());
                                                }
                                            }
                                            AgentEvent::Resumed(r) => {
                                                if let Ok(mut guard) = active_thread.lock() {
                                                    *guard = Some(r.thread.clone());
                                                }
                                            }
                                            AgentEvent::Error { code, .. }
                                                if code == "no-thread" =>
                                            {
                                                if let Ok(mut guard) = active_thread.lock() {
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

fn resume_thread_before(
    stream: &mut UnixStream,
    thread_id: &str,
    deadline: Instant,
) -> Result<Option<ResumedThread>> {
    write_frame_before(
        stream,
        &serde_json::json!({ "op": "resume", "thread": thread_id }),
        deadline,
    )?;
    match parse_lever_message(&read_frame_before(stream, deadline)?) {
        Some(LeverMessage::Resumed(r)) => Ok(Some(r)),
        Some(LeverMessage::Error { code, .. }) if code == "no-thread" => Ok(None),
        other => bail!("Unexpected resume reply from Lever: {other:?}"),
    }
}

/// Connects to Lever's surface socket, completes the version-1 handshake,
/// fetches initial `status`, and restores the active thread (or, on a fresh
/// shell start, the working thread if any, else the newest thread) before
/// clearing the read timeout. Fails if Lever does not answer within
/// `init_deadline`.
fn connect_and_bootstrap(
    path: &PathBuf,
    existing_thread: Option<String>,
    init_deadline: Duration,
) -> Result<(UnixStream, StatusInfo, Option<ResumedThread>)> {
    let deadline = Instant::now() + init_deadline;
    let mut stream = UnixStream::connect(path)?;

    // Handshake.
    write_frame_before(
        &mut stream,
        &serde_json::json!({ "version": SURFACE_VERSION }),
        deadline,
    )?;
    let reply = read_frame_before(&mut stream, deadline)?;
    if reply != serde_json::json!({ "version": SURFACE_VERSION }) {
        bail!("Lever did not speak surface protocol version {SURFACE_VERSION}");
    }

    // Initial status (must arrive within `deadline` before reporting Connected).
    write_frame_before(&mut stream, &serde_json::json!({ "op": "status" }), deadline)?;
    let status = match parse_lever_message(&read_frame_before(&mut stream, deadline)?) {
        Some(LeverMessage::Status(status)) => status,
        other => bail!("Unexpected initial status reply from Lever: {other:?}"),
    };

    // Restore thread: resume the held thread if reconnecting, else ask `list`
    // and resume a working thread (if any) or the newest thread so a restarted
    // shell redraws from Lever.
    let mut resumed = None;
    if let Some(tid) = existing_thread {
        resumed = resume_thread_before(&mut stream, &tid, deadline)?;
    }
    if resumed.is_none() {
        write_frame_before(&mut stream, &serde_json::json!({ "op": "list" }), deadline)?;
        match parse_lever_message(&read_frame_before(&mut stream, deadline)?) {
            Some(LeverMessage::List { threads }) => {
                if let Some(chosen) = threads.iter().find(|t| t.working).or_else(|| threads.first()) {
                    resumed = resume_thread_before(&mut stream, &chosen.thread, deadline)?;
                }
            }
            Some(LeverMessage::Error { .. }) => {}
            other => bail!("Unexpected list reply from Lever: {other:?}"),
        }
    }

    // Clear read timeout so the idle reader thread blocks with zero wakeups.
    stream.set_read_timeout(None)?;
    Ok((stream, status, resumed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn short_temp_dir(prefix: &str) -> PathBuf {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir();
        let root = if base.as_os_str().len() > 48 {
            PathBuf::from("/tmp")
        } else {
            base
        };
        let dir = root.join(format!("{prefix}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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

    fn sample_status(harnesses: Vec<&str>) -> StatusInfo {
        StatusInfo {
            lever_version: "0.1.0".into(),
            harness_started: false,
            harnesses: harnesses.into_iter().map(str::to_string).collect(),
            free_hosted_models: false,
        }
    }

    #[test]
    fn parses_every_lever_surface_message() {
        // status
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": true,
                "harnesses": [{ "id": "harness-1", "name": "OpenCode" }],
                "free_hosted_models": true
            })),
            Some(LeverMessage::Status(StatusInfo {
                lever_version: "0.1.0".into(),
                harness_started: true,
                harnesses: vec!["OpenCode".into()],
                free_hosted_models: true,
            }))
        );

        // list
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "reply": "list",
                "threads": [{
                    "thread": "t-123",
                    "title": "Check disk",
                    "cwd": "/home/crowbar",
                    "model": "stub/stub-1",
                    "started": 100,
                    "updated": 105,
                    "turns": 2,
                    "state": "done",
                    "working": false
                }]
            })),
            Some(LeverMessage::List {
                threads: vec![ThreadSummary {
                    thread: "t-123".into(),
                    title: "Check disk".into(),
                    cwd: "/home/crowbar".into(),
                    model: Some("stub/stub-1".into()),
                    started: 100,
                    updated: 105,
                    turns: 2,
                    state: "done".into(),
                    working: false,
                }]
            })
        );

        // resumed (with context, acts, notes, and failed/interrupted/stopped/working states)
        let resumed = parse_lever_message(&serde_json::json!({
            "reply": "resumed",
            "thread": "t-123",
            "title": "Check disk",
            "cwd": "/home/crowbar",
            "model": "stub/stub-1",
            "started": 100,
            "updated": 105,
            "working": false,
            "turns": [
                {
                    "turn": 1,
                    "message": "ls",
                    "context": {
                        "workspace": 1,
                        "active": { "id": "0x1", "app_id": "foot", "title": "foot", "workspace": 1 },
                        "locked": false,
                        "windows": [{ "id": "0x1", "app_id": "foot", "title": "foot", "workspace": 1 }],
                        "displays": [{ "name": "Virtual-1", "width": 1280, "height": 960, "x": 0, "y": 0, "scale": 120 }]
                    },
                    "answer": "Cargo.toml",
                    "state": "done",
                    "acts": [{ "act": "act-1", "kind": "command", "title": "Run ls", "state": "done", "detail": "Cargo.toml" }],
                    "notes": [{ "note": "note-1", "text": "Session restarted" }]
                },
                { "turn": 2, "message": "stop me", "answer": "", "state": "stopped", "acts": [], "notes": [] },
                { "turn": 3, "message": "crash", "answer": "", "state": "interrupted", "acts": [], "notes": [] },
                { "turn": 4, "message": "fail", "answer": "", "state": "failed", "error": "boom", "acts": [], "notes": [] }
            ]
        }));
        match resumed {
            Some(LeverMessage::Resumed(r)) => {
                assert_eq!(r.thread, "t-123");
                assert_eq!(r.model.as_deref(), Some("stub/stub-1"));
                assert_eq!(r.turns.len(), 4);
                assert_eq!(r.turns[0].acts.len(), 1);
                assert_eq!(r.turns[0].acts[0].detail.as_deref(), Some("Cargo.toml"));
                assert_eq!(r.turns[0].notes.len(), 1);
                assert!(r.turns[0].context.is_some());
                assert_eq!(r.turns[1].state, TurnState::Stopped);
                assert_eq!(r.turns[2].state, TurnState::Interrupted);
                assert_eq!(r.turns[3].state, TurnState::Failed("boom".into()));
            }
            other => panic!("expected Resumed, got {other:?}"),
        }

        // sent, ok, error
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "sent", "thread": "t-1", "turn": 1 })),
            Some(LeverMessage::Sent { thread: "t-1".into(), turn: 1 })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "ok" })),
            Some(LeverMessage::Ok)
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "reply": "error", "code": "no-model", "message": "No Model is available."
            })),
            Some(LeverMessage::Error {
                code: "no-model".into(),
                message: "No Model is available.".into()
            })
        );

        // live events: message (without and with desktop context), text, act, note, thread-updated, done, stopped, failed, interrupted
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "message", "thread": "t-1", "turn": 1, "text": "hello"
            })),
            Some(LeverMessage::Message {
                thread: "t-1".into(),
                turn: 1,
                text: "hello".into(),
                context: None,
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "message",
                "thread": "t-1",
                "turn": 2,
                "text": "what is focused?",
                "context": {
                    "workspace": 2,
                    "active": { "id": "0x2", "app_id": "foot", "title": "work", "workspace": 2 },
                    "locked": false,
                    "windows": [
                        { "id": "0x1", "app_id": "com.crowbaros.StudioSettings", "title": "Settings", "workspace": 1 },
                        { "id": "0x2", "app_id": "foot", "title": "work", "workspace": 2 }
                    ],
                    "displays": [{ "name": "Virtual-1", "width": 1280, "height": 960, "x": 0, "y": 0, "scale": 120 }]
                }
            })),
            Some(LeverMessage::Message {
                thread: "t-1".into(),
                turn: 2,
                text: "what is focused?".into(),
                context: Some(sample_context()),
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "text", "thread": "t-1", "turn": 1, "text": "hi"
            })),
            Some(LeverMessage::Text { thread: "t-1".into(), turn: 1, text: "hi".into() })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "act", "thread": "t-1", "turn": 1,
                "act": "act-a", "kind": "edit", "title": "Edit main.rs", "state": "working", "detail": "src/main.rs"
            })),
            Some(LeverMessage::Act {
                thread: "t-1".into(),
                turn: 1,
                act: Act {
                    id: "act-a".into(),
                    kind: "edit".into(),
                    title: "Edit main.rs".into(),
                    state: ActState::Working,
                    detail: Some("src/main.rs".into()),
                }
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "note", "thread": "t-1", "turn": 1, "note": "note-a", "text": "New session"
            })),
            Some(LeverMessage::Note {
                thread: "t-1".into(),
                turn: 1,
                note: Note { id: "note-a".into(), text: "New session".into() }
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "thread-updated", "thread": "t-1", "title": "Inspect workspace"
            })),
            Some(LeverMessage::ThreadUpdated { thread: "t-1".into(), title: "Inspect workspace".into() })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "done", "thread": "t-1", "turn": 1 })),
            Some(LeverMessage::Done { thread: "t-1".into(), turn: 1 })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "stopped", "thread": "t-1", "turn": 1 })),
            Some(LeverMessage::Stopped { thread: "t-1".into(), turn: 1 })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "failed", "thread": "t-1", "turn": 1, "message": "provider failed"
            })),
            Some(LeverMessage::Failed { thread: "t-1".into(), turn: 1, message: "provider failed".into() })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "interrupted", "thread": "t-1", "turn": 1 })),
            Some(LeverMessage::Interrupted { thread: "t-1".into(), turn: 1 })
        );
    }

    #[test]
    fn turn_streaming_acts_notes_and_mark_transitions_behind_drawer() {
        let mut state = AgentState::new(true);
        assert_eq!(state.mark, Mark::Idle);

        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
            },
            true,
        );
        assert!(state.connected);
        assert!(!state.no_harness);
        assert!(state.can_send());

        state.apply(AgentEvent::Sent { thread: "t-abc".into(), turn: 1 }, true);
        assert_eq!(state.mark, Mark::Working);
        state.apply(
            AgentEvent::Message {
                thread: "t-abc".into(),
                turn: 1,
                text: "What is focused?".into(),
                context: Some(sample_context()),
            },
            true,
        );
        // Act transitions pending -> working -> done in place by act ID.
        state.apply(
            AgentEvent::Act {
                thread: "t-abc".into(),
                turn: 1,
                act: Act {
                    id: "act-1".into(),
                    kind: "command".into(),
                    title: "shell".into(),
                    state: ActState::Pending,
                    detail: None,
                },
            },
            true,
        );
        state.apply(
            AgentEvent::Act {
                thread: "t-abc".into(),
                turn: 1,
                act: Act {
                    id: "act-1".into(),
                    kind: "command".into(),
                    title: "id -un".into(),
                    state: ActState::Done,
                    detail: Some("crowbar".into()),
                },
            },
            true,
        );
        state.apply(
            AgentEvent::Note {
                thread: "t-abc".into(),
                turn: 1,
                note: Note { id: "note-1".into(), text: "Warm cache".into() },
            },
            true,
        );
        state.apply(
            AgentEvent::ThreadUpdated { thread: "t-abc".into(), title: "Focus check".into() },
            true,
        );
        state.apply(
            AgentEvent::Text { thread: "t-abc".into(), turn: 1, text: "The active ".into() },
            true,
        );

        // Putting the drawer away stops nothing and keeps Working.
        assert!(!state.on_drawer_closed());
        assert_eq!(state.mark, Mark::Working);

        // Answer finishes behind the drawer -> mark shows Done.
        state.apply(
            AgentEvent::Text { thread: "t-abc".into(), turn: 1, text: "window is foot.".into() },
            false,
        );
        state.apply(AgentEvent::Done { thread: "t-abc".into(), turn: 1 }, false);
        assert_eq!(state.mark, Mark::Done);
        assert_eq!(state.title.as_deref(), Some("Focus check"));
        assert_eq!(state.turns.len(), 1);
        assert_eq!(state.turns[0].acts.len(), 1);
        assert_eq!(state.turns[0].acts[0].title, "id -un");
        assert_eq!(state.turns[0].acts[0].state, ActState::Done);
        assert_eq!(state.turns[0].acts[0].detail.as_deref(), Some("crowbar"));
        assert_eq!(state.turns[0].notes.len(), 1);
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
        assert_eq!(fullscreen_mark_overlay_output(Mark::Working, &snap, true), None);
        snap.locked = true;
        assert_eq!(fullscreen_mark_overlay_output(Mark::Working, &snap, false), None);
        snap.locked = false;
        snap.workspace = 2;
        assert_eq!(fullscreen_mark_overlay_output(Mark::Working, &snap, false), None);
    }

    #[test]
    fn rapid_send_guard_stopped_interrupted_failed_and_no_thread_recovery() {
        let mut state = AgentState::new(true);
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
            },
            true,
        );
        assert!(state.can_send());
        state.sending = true;
        assert!(!state.can_send());
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        assert!(!state.sending);
        assert!(!state.can_send());

        // Failure while drawer is put away leaves Mark::Done so the person notices.
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "hi".into(), context: None },
            false,
        );
        state.apply(
            AgentEvent::Failed {
                thread: "t-1".into(),
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

        // Stopped turn transitions mark to Idle; Stop right after Sent (before Message)
        // targets the new turn rather than the previous finished turn.
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 2 }, true);
        assert_eq!(state.current_turn(), Some(2));
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 2, text: "wait".into(), context: None },
            true,
        );
        state.apply(AgentEvent::Stopped { thread: "t-1".into(), turn: 2 }, true);
        assert_eq!(state.turns[1].state, TurnState::Stopped);
        assert_eq!(state.current_turn(), None);
        assert_eq!(state.mark, Mark::Idle);

        // Resuming a thread with `model: None` clears any previously held model.
        state.model = Some("stub/stub-1".into());
        state.apply(
            AgentEvent::Resumed(ResumedThread {
                thread: "t-2".into(),
                title: "Fresh".into(),
                cwd: "/tmp".into(),
                model: None,
                working: false,
                turns: Vec::new(),
            }),
            true,
        );
        assert_eq!(state.model, None);
        state.thread = Some("t-1".into());
        state.turns.push(Turn {
            turn: 1,
            message: "prev".into(),
            context: None,
            acts: Vec::new(),
            notes: Vec::new(),
            answer: "ok".into(),
            state: TurnState::Done,
        });
        state.turns.push(Turn {
            turn: 2,
            message: "stopped".into(),
            context: None,
            acts: Vec::new(),
            notes: Vec::new(),
            answer: String::new(),
            state: TurnState::Stopped,
        });

        // Interrupted turn behind drawer shows Done.
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 3 }, false);
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 3, text: "mid".into(), context: None },
            false,
        );
        state.apply(AgentEvent::Interrupted { thread: "t-1".into(), turn: 3 }, false);
        assert_eq!(state.turns[2].state, TurnState::Interrupted);
        assert_eq!(state.mark, Mark::Done);

        // Lost thread clears stale state and unlocks composer.
        state.mark = Mark::Working;
        state.apply(
            AgentEvent::Error {
                code: "no-thread".into(),
                message: "There is no thread t-1.".into(),
            },
            true,
        );
        assert_eq!(state.thread, None);
        assert!(state.turns.is_empty());
        assert_eq!(state.mark, Mark::Idle);
        assert!(state.can_send());

        // Numbers containing 401/403 must not be misclassified as sign-in errors.
        let token_err = "prompt is too long: 204031 tokens > 200000 maximum (140123 input)";
        assert_eq!(calm_turn_error(token_err), token_err);
        for auth_err in [
            "HTTP 401 Unauthorized",
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
            "API key not valid. Please pass a valid API key.",
        ] {
            assert!(
                calm_turn_error(auth_err).contains("crowbar-agent login"),
                "expected auth copy for {auth_err}"
            );
        }
    }

    #[test]
    fn draft_after_send_clears_exact_strips_typed_ahead_and_keeps_edits() {
        assert_eq!(draft_after_send("hello", "hello"), Some(String::new()));
        assert_eq!(
            draft_after_send("hello next question", "hello"),
            Some("next question".into())
        );
        assert_eq!(draft_after_send("completely new draft", "hello"), None);
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
    fn surface_client_holds_connected_until_initial_status_and_reconnects_on_timeout() {
        let dir = short_temp_dir("cs-lever-deadline");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            // Attempt 1: handshake succeeds, status sends a 4-byte length header
            // and stalls the payload past the deadline -> client must NOT emit Connected.
            let (mut slow, _) = listener.accept().unwrap();
            let v = read_frame(&mut slow).unwrap();
            assert_eq!(v, serde_json::json!({ "version": 1 }));
            slow.write_all(&encode_frame(&serde_json::json!({ "version": 1 })))
                .unwrap();
            let req = read_frame(&mut slow).unwrap();
            assert_eq!(req["op"], "status");
            // Send 4-byte header advertising 32 bytes, then stall the body past 180ms.
            slow.write_all(&32u32.to_be_bytes()).unwrap();
            std::thread::sleep(Duration::from_millis(320));
            drop(slow);

            // Attempt 2: answers status + empty list -> client reports Connected with status already populated.
            let (mut ok, _) = listener.accept().unwrap();
            let v = read_frame(&mut ok).unwrap();
            assert_eq!(v, serde_json::json!({ "version": 1 }));
            ok.write_all(&encode_frame(&serde_json::json!({ "version": 1 })))
                .unwrap();
            let req = read_frame(&mut ok).unwrap();
            assert_eq!(req["op"], "status");
            ok.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": false,
                "harnesses": [{ "id": "harness-1", "name": "OpenCode" }],
                "free_hosted_models": false
            })))
            .unwrap();
            let list_req = read_frame(&mut ok).unwrap();
            assert_eq!(list_req["op"], "list");
            ok.write_all(&encode_frame(&serde_json::json!({
                "reply": "list",
                "threads": []
            })))
            .unwrap();
        });

        let (tx, mut rx) = session::channel();
        let _sender = start_with_deadline(sock.clone(), tx, Duration::from_millis(180));

        let mut events_seen = Vec::new();
        let mut state = AgentState::new(true);
        futures::executor::block_on(async {
            while let Some(ev) = rx.next().await {
                if let session::Event::Agent(agent_ev) = ev {
                    events_seen.push(agent_ev.clone());
                    state.apply(agent_ev, true);
                    if state.connected {
                        break;
                    }
                }
            }
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(events_seen.len(), 1, "expected only Connected after retry: {events_seen:?}");
        assert!(state.connected);
        assert_eq!(state.lever_version.as_deref(), Some("0.1.0"));
        assert_eq!(state.harnesses, vec!["OpenCode".to_string()]);
        assert!(!state.no_harness);
    }

    #[test]
    fn surface_client_resumes_newest_thread_after_restart_and_streams_acts_and_stop() {
        let dir = short_temp_dir("cs-lever-resume");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            let v = read_frame(&mut peer).unwrap();
            assert_eq!(v, serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 })))
                .unwrap();

            assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": true,
                "harnesses": [{ "id": "harness-1", "name": "OpenCode" }],
                "free_hosted_models": false
            })))
            .unwrap();

            // Fresh shell asks `list` and prefers a working thread over a newer idle one.
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "list");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "list",
                "threads": [
                    {
                        "thread": "t-idle-newer",
                        "title": "Idle newer",
                        "cwd": "/home/crowbar",
                        "started": 110,
                        "updated": 120,
                        "turns": 1,
                        "state": "done",
                        "working": false
                    },
                    {
                        "thread": "t-existing",
                        "title": "Existing work",
                        "cwd": "/home/crowbar",
                        "model": "stub/stub-1",
                        "started": 100,
                        "updated": 105,
                        "turns": 1,
                        "state": "working",
                        "working": true
                    }
                ]
            })))
            .unwrap();

            let resume_req = read_frame(&mut peer).unwrap();
            assert_eq!(resume_req, serde_json::json!({ "op": "resume", "thread": "t-existing" }));
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "resumed",
                "thread": "t-existing",
                "title": "Existing work",
                "cwd": "/home/crowbar",
                "model": "stub/stub-1",
                "started": 100,
                "updated": 105,
                "working": false,
                "turns": [{
                    "turn": 1,
                    "message": "check whoami",
                    "context": {
                        "workspace": 1,
                        "active": { "id": "0x1", "app_id": "foot", "title": "foot", "workspace": 1 },
                        "locked": false,
                        "windows": [{ "id": "0x1", "app_id": "foot", "title": "foot", "workspace": 1 }],
                        "displays": [{ "name": "Virtual-1", "width": 1280, "height": 960, "x": 0, "y": 0, "scale": 120 }]
                    },
                    "answer": "crowbar",
                    "state": "done",
                    "acts": [{ "act": "act-0", "kind": "command", "title": "id -un", "state": "done", "detail": "crowbar" }],
                    "notes": []
                }]
            })))
            .unwrap();

            // Client sends turn 2 and then stops it.
            let send_req = read_frame(&mut peer).unwrap();
            assert_eq!(send_req["op"], "send");
            assert_eq!(send_req["thread"], "t-existing");
            assert_eq!(send_req["text"], "long command");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "sent", "thread": "t-existing", "turn": 2
            })))
            .unwrap();
            peer.write_all(&encode_frame(&serde_json::json!({
                "event": "message", "thread": "t-existing", "turn": 2, "text": "long command"
            })))
            .unwrap();
            peer.write_all(&encode_frame(&serde_json::json!({
                "event": "act", "thread": "t-existing", "turn": 2,
                "act": "act-2", "kind": "command", "title": "sleep 30", "state": "working"
            })))
            .unwrap();

            let stop_req = read_frame(&mut peer).unwrap();
            assert_eq!(stop_req, serde_json::json!({ "op": "stop", "thread": "t-existing", "turn": 2 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "reply": "ok" })))
                .unwrap();
            peer.write_all(&encode_frame(&serde_json::json!({
                "event": "stopped", "thread": "t-existing", "turn": 2
            })))
            .unwrap();
        });

        let (tx, mut rx) = session::channel();
        let sender = start(sock.clone(), tx);

        let mut state = AgentState::new(true);
        let mut sent_turn2 = false;
        let mut stopped_turn2 = false;
        futures::executor::block_on(async {
            while let Some(ev) = rx.next().await {
                if let session::Event::Agent(agent_ev) = ev {
                    state.apply(agent_ev, true);
                    if state.connected && !sent_turn2 {
                        assert_eq!(state.thread.as_deref(), Some("t-existing"));
                        assert_eq!(state.turns.len(), 1);
                        assert_eq!(state.turns[0].acts[0].detail.as_deref(), Some("crowbar"));
                        sent_turn2 = true;
                        sender.send_message(state.thread.clone(), "long command".into());
                    }
                    if state.turns.len() == 2 && !state.turns[1].acts.is_empty() && !stopped_turn2 {
                        stopped_turn2 = true;
                        sender.stop_turn("t-existing".into(), Some(2));
                    }
                    if state.turns.len() == 2 && state.turns[1].state == TurnState::Stopped {
                        break;
                    }
                }
            }
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(state.turns.len(), 2);
        assert_eq!(state.turns[1].state, TurnState::Stopped);
        assert_eq!(state.mark, Mark::Idle);
    }

    #[test]
    fn surface_client_connects_to_lever_and_reports_no_harness() {
        let dir = short_temp_dir("cs-lever-noharness");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let deadline = Duration::from_secs(10);
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(deadline)).unwrap();
            peer.set_write_timeout(Some(deadline)).unwrap();
            let v = read_frame(&mut peer).unwrap();
            assert_eq!(v, serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 })))
                .unwrap();

            let status_req = read_frame(&mut peer).unwrap();
            assert_eq!(status_req["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": false,
                "harnesses": [],
                "free_hosted_models": false
            })))
            .unwrap();

            let list_req = read_frame(&mut peer).unwrap();
            assert_eq!(list_req["op"], "list");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "list",
                "threads": []
            })))
            .unwrap();
        });

        let (tx, mut rx) = session::channel();
        let late = tx.clone();
        let _sender = start(sock.clone(), tx);
        std::thread::spawn(move || {
            std::thread::sleep(deadline);
            let _ = late.send(session::Event::Agent(AgentEvent::Error {
                code: "test-deadline".into(),
                message: String::new(),
            }));
        });

        let mut state = AgentState::new(true);
        futures::executor::block_on(async {
            while let Some(ev) = rx.next().await {
                if let session::Event::Agent(agent_ev) = ev {
                    if matches!(&agent_ev, AgentEvent::Error { code, .. } if code == "test-deadline")
                    {
                        panic!("the shell did not report Lever's status in {deadline:?}");
                    }
                    state.apply(agent_ev, true);
                    if state.connected && state.lever_version.is_some() {
                        break;
                    }
                }
            }
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(state.connected);
        assert_eq!(state.lever_version.as_deref(), Some("0.1.0"));
        assert!(!state.harness_started);
        assert!(state.no_harness);
        assert!(!state.can_send());
        assert_eq!(state.mark, Mark::Idle);
    }
}
