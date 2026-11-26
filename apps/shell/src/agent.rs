// SPDX-License-Identifier: MIT
//! The shell's client of Lever's surface socket (ADR 0036).
//!
//! Speaks Lever's version-1 framing (4-byte big-endian length + UTF-8 JSON,
//! `{"version":1}` first) over `CROWBAR_LEVER_SOCKET`, holding the active
//! thread's turns, acts, notes, requests, desktop context, and mode, and the
//! mark (`idle`, `working`, `waiting`, `done`) across every thread, which
//! Lever's `activity` drives. Blocking I/O on dedicated background threads
//! wakes GPUI only when a frame arrives, so a still desktop draws nothing.
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{ErrorKind, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
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

/// Where a harness's request stands, as Lever says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestState {
    Waiting,
    Answered,
    Withdrawn,
}

/// One answer a request offers: `once`, `always`, `no`, or `never`, with the
/// harness's own name for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestAnswer {
    pub answer: String,
    pub name: String,
}

impl RequestAnswer {
    /// The harness's name for the answer, or a plain one when it gave none.
    pub fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        answer_label(&self.answer).to_string()
    }
}

/// A plain name for an answer the harness did not name.
pub fn answer_label(answer: &str) -> &'static str {
    match answer {
        "once" => "Allow once",
        "always" => "Always allow",
        "no" => "Refuse",
        "never" => "Never allow",
        _ => "Answer",
    }
}

/// A harness's request to the person before an act: the exact command, or
/// the file and the change, and the answers it offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    #[serde(rename = "request")]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act: Option<String>,
    pub kind: String,
    pub title: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub answers: Vec<RequestAnswer>,
    pub state: RequestState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// The answer the person picked in the drawer, sent and not yet taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sending: Option<String>,
}

impl Request {
    /// The name of the answer given, from the answers the request offered.
    pub fn answer_name(&self, answer: &str) -> String {
        self.answers
            .iter()
            .find(|a| a.answer == answer)
            .map(RequestAnswer::label)
            .unwrap_or_else(|| answer_label(answer).to_string())
    }
}

/// How the drawer shows a request in its turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestView {
    /// It waits for the person, with one control per answer.
    Waiting,
    /// The person picked this answer; Lever has yet to take it.
    Sending(String),
    /// Answered, with the answer's name.
    Answered(String),
    /// Withdrawn, or no longer waiting because its turn was interrupted.
    Withdrawn,
}

/// A request waits as long as Lever says it does, after its turn ended too:
/// a harness may ask after a turn, and the request waits on it all the same.
/// Only one in a turn the drawer saw interrupted, as when Lever went away,
/// is shown as withdrawn, with no answer controls: Lever withdraws what a
/// harness asked when it ends.
pub fn request_view(turn_state: &TurnState, request: &Request) -> RequestView {
    match request.state {
        RequestState::Answered => RequestView::Answered(
            request
                .answer
                .as_deref()
                .map(|a| request.answer_name(a))
                .unwrap_or_else(|| "Answered".into()),
        ),
        RequestState::Withdrawn => RequestView::Withdrawn,
        RequestState::Waiting if matches!(turn_state, TurnState::Interrupted) => {
            RequestView::Withdrawn
        }
        RequestState::Waiting => match &request.sending {
            Some(answer) => RequestView::Sending(request.answer_name(answer)),
            None => RequestView::Waiting,
        },
    }
}

/// One line of a request's detail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DetailLine {
    Plain,
    Removed,
    Added,
}

/// Splits a request's detail into lines for the drawer. A command is shown
/// as it is; any other detail's lines that begin `-` or `+` are a diff's
/// removed and added lines. Lever cuts a long change itself, so both what
/// it takes out and what it puts in show, in at most 402 lines: the drawer
/// shows all of that, and cuts only longer detail, saying how much.
pub fn request_detail_lines(kind: &str, detail: &str) -> Vec<(DetailLine, String)> {
    const MAX_LINES: usize = 402;
    let diff = kind != "command";
    let lines: Vec<&str> = detail.trim_end_matches('\n').lines().collect();
    let mut out: Vec<(DetailLine, String)> = lines
        .iter()
        .take(MAX_LINES)
        .map(|line| {
            let tone = if !diff {
                DetailLine::Plain
            } else if line.starts_with('-') {
                DetailLine::Removed
            } else if line.starts_with('+') {
                DetailLine::Added
            } else {
                DetailLine::Plain
            };
            (tone, line.to_string())
        })
        .collect();
    if lines.len() > MAX_LINES {
        out.push((
            DetailLine::Plain,
            format!("… {} more lines", lines.len() - MAX_LINES),
        ));
    }
    out
}

/// A mode Lever offers for the harness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeInfo {
    pub id: String,
    pub name: String,
}

/// The name of a mode: the one Lever gave, or ours for the modes we know.
pub fn mode_name(id: &str, modes: &[ModeInfo]) -> String {
    if let Some(mode) = modes.iter().find(|m| m.id == id && !m.name.trim().is_empty()) {
        return mode.name.clone();
    }
    match id {
        "ask-first" => "Ask first".into(),
        "full-access" => "Full access".into(),
        "plan" => "Plan".into(),
        other => other.to_string(),
    }
}

/// The modes new threads may start in.
pub const DEFAULT_MODES: [&str; 2] = ["ask-first", "full-access"];

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
    #[serde(default)]
    pub requests: Vec<Request>,
    /// The mode the turn began in, when Lever recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub answer: String,
    pub state: TurnState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mark {
    #[default]
    Idle,
    Working,
    /// A request of some thread waits for the person.
    Waiting,
    Done,
}

impl Mark {
    pub fn label(self) -> &'static str {
        match self {
            Mark::Idle => "idle",
            Mark::Working => "working",
            Mark::Waiting => "waiting",
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
    /// The mode new threads start in, when Lever says.
    pub default_mode: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSummary {
    #[serde(default)]
    pub archived: bool,
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
    #[serde(default)]
    pub waiting: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumedThread {
    pub thread: String,
    pub title: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub working: bool,
    pub turns: Vec<Turn>,
}

/// Every reply or event Lever sends on its surface socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeverMessage {
    Threads { threads: Vec<ThreadSummary>, finished: bool },
    Lifecycle { thread: String, deleted: bool, warning: Option<String> },
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
    Request {
        thread: String,
        turn: u64,
        request: Request,
    },
    ThreadUpdated {
        thread: String,
        title: Option<String>,
        mode: Option<String>,
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
    /// The `models` reply; the drawer reads only its modes.
    Models { modes: Vec<ModeInfo> },
    ModeSet { thread: String, mode: String },
    DefaultMode { mode: String },
    /// The `watch` reply and each `activity` event.
    Activity {
        working: Vec<String>,
        waiting: Vec<String>,
    },
}

/// Events sent from the surface-socket client to the shell's UI loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEvent {
    Threads { threads: Vec<ThreadSummary>, finished: bool },
    Lifecycle { thread: String, deleted: bool, warning: Option<String> },
    /// Emitted only after Lever's initial `status` (and `list`/`resume`, if
    /// applicable) have arrived within the bootstrap deadline.
    Connected {
        status: StatusInfo,
        resumed: Option<ResumedThread>,
        preserved_thread: Option<String>,
        resume_error: Option<String>,
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
    Request {
        thread: String,
        turn: u64,
        request: Request,
    },
    ThreadUpdated {
        thread: String,
        title: Option<String>,
        mode: Option<String>,
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
    Modes(Vec<ModeInfo>),
    ModeSet {
        thread: String,
        mode: String,
    },
    DefaultMode(String),
    Activity {
        working: Vec<String>,
        waiting: Vec<String>,
    },
    Error {
        code: String,
        message: String,
    },
}

impl LeverMessage {
    fn into_agent_event(self) -> Option<AgentEvent> {
        match self {
            LeverMessage::Threads { threads, finished } => Some(AgentEvent::Threads { threads, finished }),
            LeverMessage::List { threads } => Some(AgentEvent::Threads { threads, finished: false }),
            LeverMessage::Lifecycle { thread, deleted, warning } => Some(AgentEvent::Lifecycle { thread, deleted, warning }),
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
            LeverMessage::Request {
                thread,
                turn,
                request,
            } => Some(AgentEvent::Request {
                thread,
                turn,
                request,
            }),
            LeverMessage::ThreadUpdated {
                thread,
                title,
                mode,
            } => Some(AgentEvent::ThreadUpdated {
                thread,
                title,
                mode,
            }),
            LeverMessage::Models { modes } => Some(AgentEvent::Modes(modes)),
            LeverMessage::ModeSet { thread, mode } => Some(AgentEvent::ModeSet { thread, mode }),
            LeverMessage::DefaultMode { mode } => Some(AgentEvent::DefaultMode(mode)),
            LeverMessage::Activity { working, waiting } => {
                Some(AgentEvent::Activity { working, waiting })
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
            LeverMessage::Ok => None,
        }
    }
}

/// Commands sent from the shell's UI to the surface-socket background thread.
#[derive(Clone, Debug)]
pub enum AgentCommand {
    Select { thread: Option<String> },
    Lifecycle { thread: String, delete: bool },
    Send {
        thread: Option<String>,
        text: String,
        mode: Option<String>,
    },
    Stop {
        thread: String,
        turn: Option<u64>,
    },
    Answer {
        thread: String,
        request: String,
        answer: String,
    },
    SetMode {
        thread: String,
        mode: String,
    },
    SetDefaultMode {
        mode: String,
    },
    /// `models`, for the modes Lever offers. It starts the harness, so the
    /// drawer asks only when the person opens the mode menu.
    FetchModes,
    RefreshStatus,
}

#[derive(Clone)]
pub struct CommandSender {
    tx: mpsc::Sender<AgentCommand>,
}

impl CommandSender {
    pub fn select(&self, thread: Option<String>) { let _ = self.tx.send(AgentCommand::Select { thread }); }
    pub fn lifecycle(&self, thread: String, delete: bool) { let _ = self.tx.send(AgentCommand::Lifecycle { thread, delete }); }

    /// Sends a message. `mode` is the mode a new thread starts in.
    pub fn send_message(&self, thread: Option<String>, text: String, mode: Option<String>) {
        let _ = self.tx.send(AgentCommand::Send { thread, text, mode });
    }

    pub fn answer(&self, thread: String, request: String, answer: String) {
        let _ = self.tx.send(AgentCommand::Answer {
            thread,
            request,
            answer,
        });
    }

    pub fn set_mode(&self, thread: String, mode: String) {
        let _ = self.tx.send(AgentCommand::SetMode { thread, mode });
    }

    pub fn set_default_mode(&self, mode: String) {
        let _ = self.tx.send(AgentCommand::SetDefaultMode { mode });
    }

    pub fn fetch_modes(&self) {
        let _ = self.tx.send(AgentCommand::FetchModes);
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
    pub list_view: bool,
    pub loading: bool,
    pub threads: Vec<ThreadSummary>,
    #[serde(skip)]
    list_received: bool,

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
    /// The mark across every thread: `waiting` while any request waits,
    /// else `working` while any thread works, else `done` until the drawer
    /// is summoned or put away after a turn ended, else `idle`.
    pub mark: Mark,
    /// The thread's mode, as Lever has it.
    pub mode: Option<String>,
    /// The mode picked for the drawer's new thread, sent with its first message.
    pub pending_mode: Option<String>,
    /// The mode new threads start in, from Lever's `status`.
    pub default_mode: Option<String>,
    /// The modes Lever offers, fetched when the person opens the mode menu.
    pub modes: Vec<ModeInfo>,
    pub modes_loading: bool,
    pub modes_error: Option<String>,
    /// The threads working and waiting, from Lever's `activity`.
    pub working_threads: Vec<String>,
    pub waiting_threads: Vec<String>,
    /// Lever has said which threads work and wait on this connection.
    #[serde(skip)]
    watching: bool,
    /// The mark from the drawer's own thread alone: what the mark is before
    /// Lever's `activity` arrives, and where `done` is held after it does.
    #[serde(skip)]
    thread_mark: Mark,
    #[serde(skip)]
    working_before_disconnect: Option<String>,
}

/// Where a control the drawer drew is on its display, in the display's
/// logical pixels, so a check can click it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ControlBounds {
    pub id: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Snapshot exposed on the shell's own `crowbar-shell`-only control socket for
/// desktop checks to inspect drawer and mark state without touching logs.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AgentViewSnapshot {
    pub drawer_up: bool,
    /// The display the drawer is on.
    pub drawer_output: Option<String>,
    pub mark_overlay: bool,
    pub mark_overlay_output: Option<String>,
    pub mode_menu_open: bool,
    /// The drawer's controls in view as it last drew: `mode`, `mode:<id>`,
    /// `default-mode:<id>`, `answer:<request>:<answer>`, `send`, `stop`,
    /// `close`.
    pub controls: Vec<ControlBounds>,
    pub state: AgentState,
}

/// What picking a mode in the drawer does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModePick {
    /// Ask Lever to set the thread's mode.
    SetThread { thread: String, mode: String },
    /// Held for the drawer's new thread, and sent with its first message.
    Pending,
    /// The thread works, or the drawer cannot reach Lever; nothing changes.
    Refused,
}

impl AgentState {
    pub fn new(configured: bool) -> Self {
        Self {
            configured,
            list_view: true,
            ..Default::default()
        }
    }

    pub fn working(&self) -> bool {
        self.thread_mark == Mark::Working
            || self
                .turns
                .last()
                .is_some_and(|t| matches!(t.state, TurnState::Working))
    }

    /// Stop is offered while the thread works, or while a request of its
    /// waits: Stop withdraws it.
    pub fn can_stop(&self) -> bool {
        self.connected && (self.working() || self.waiting())
    }

    pub fn can_send(&self) -> bool {
        self.connected && !self.working() && !self.sending && !self.loading && !self.no_harness
    }

    pub fn current_turn(&self) -> Option<u64> {
        self.active_turn.or_else(|| {
            self.turns
                .last()
                .filter(|t| matches!(t.state, TurnState::Working))
                .map(|t| t.turn)
        })
    }

    /// A request of the drawer's thread waits for the person.
    pub fn waiting(&self) -> bool {
        self.turns.iter().any(|t| {
            t.requests.iter().any(|r| {
                matches!(
                    request_view(&t.state, r),
                    RequestView::Waiting | RequestView::Sending(_)
                )
            })
        })
    }

    /// The mode the drawer shows: the thread's, or for the drawer's new
    /// thread, the one picked for it or the starting mode.
    pub fn shown_mode(&self) -> Option<String> {
        if self.thread.is_some() {
            self.mode.clone()
        } else {
            self.pending_mode.clone().or_else(|| self.default_mode.clone())
        }
    }

    /// The thread's mode cannot change now: it works, or Lever is out of reach.
    pub fn mode_locked(&self) -> bool {
        !self.connected || self.no_harness || self.sending || self.working()
    }

    /// The person opened the mode menu: ask Lever for its modes. Returns
    /// whether to send `models`; the modes already known stay shown meanwhile.
    pub fn open_mode_menu(&mut self) -> bool {
        if !self.connected || self.no_harness {
            return false;
        }
        self.modes_loading = true;
        self.modes_error = None;
        true
    }

    /// The person picked a mode for the thread.
    pub fn pick_mode(&mut self, mode: &str) -> ModePick {
        if self.mode_locked() {
            return ModePick::Refused;
        }
        match &self.thread {
            Some(thread) => ModePick::SetThread {
                thread: thread.clone(),
                mode: mode.to_string(),
            },
            None => {
                self.pending_mode = Some(mode.to_string());
                ModePick::Pending
            }
        }
    }

    /// The person answered a request. Returns the thread to send `answer`
    /// on, or `None` when the request no longer waits or is already answered.
    pub fn begin_answer(&mut self, request: &str, answer: &str) -> Option<String> {
        if !self.connected {
            return None;
        }
        let thread = self.thread.clone()?;
        let turn = self
            .turns
            .iter_mut()
            .find(|t| t.requests.iter().any(|r| r.id == request))?;
        let state = turn.state.clone();
        let req = turn.requests.iter_mut().find(|r| r.id == request)?;
        if request_view(&state, req) != RequestView::Waiting
            || !req.answers.iter().any(|a| a.answer == answer)
        {
            return None;
        }
        req.sending = Some(answer.to_string());
        Some(thread)
    }

    /// Works the mark out from what Lever's `activity` says of every thread
    /// (waiting over working over done), or, before it has, from the
    /// drawer's own thread.
    fn refresh_mark(&mut self) {
        self.mark = if self.watching {
            if !self.waiting_threads.is_empty() {
                Mark::Waiting
            } else if !self.working_threads.is_empty() {
                Mark::Working
            } else if self.thread_mark == Mark::Done {
                Mark::Done
            } else {
                Mark::Idle
            }
        } else if self.connected && self.waiting() {
            Mark::Waiting
        } else {
            self.thread_mark
        };
    }

    fn apply_status(&mut self, status: StatusInfo) -> bool {
        let no_harness = status.harnesses.is_empty();
        let lever_version = Some(status.lever_version);
        let changed = self.lever_version != lever_version
            || self.harness_started != status.harness_started
            || self.harnesses != status.harnesses
            || self.free_hosted_models != status.free_hosted_models
            || self.no_harness != no_harness
            || (status.default_mode.is_some() && self.default_mode != status.default_mode);
        if status.default_mode.is_some() {
            self.default_mode = status.default_mode;
        }
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
        self.loading = false;
        self.thread = Some(resumed.thread);
        self.title = if resumed.title.is_empty() {
            None
        } else {
            Some(resumed.title)
        };
        self.model = resumed.model;
        self.mode = resumed.mode;
        self.pending_mode = None;
        self.active_turn = resumed
            .turns
            .last()
            .filter(|t| matches!(t.state, TurnState::Working))
            .map(|t| t.turn);
        let was_working = self.thread_mark == Mark::Working
            || self
                .working_before_disconnect
                .take()
                .is_some_and(|t| self.thread.as_deref() == Some(t.as_str()));
        let next_mark = match resumed.turns.last().map(|t| &t.state) {
            Some(TurnState::Working) => Mark::Working,
            Some(TurnState::Done) if was_working => Mark::Done,
            Some(TurnState::Stopped | TurnState::Failed(_) | TurnState::Interrupted)
                if was_working && !drawer_up =>
            {
                Mark::Done
            }
            _ if was_working => Mark::Idle,
            _ => self.thread_mark,
        };
        self.turns = resumed.turns;
        self.thread_mark = next_mark;
    }

    /// Forgets the drawer's thread, as for a thread Lever no longer has.
    fn clear_thread(&mut self) {
        self.thread = None;
        self.title = None;
        self.model = None;
        self.mode = None;
        self.active_turn = None;
        self.turns.clear();
        self.thread_mark = Mark::Idle;
    }

    pub fn select_thread(&mut self, thread: Option<String>) -> bool {
        if self.sending || self.loading { return false; }
        self.thread = thread;
        self.title = None;
        self.model = None;
        self.turns.clear();
        self.active_turn = None;
        self.error = None;
        self.missing_model = None;
        self.loading = self.thread.is_some();
        self.list_view = false;
        true
    }

    /// Folder order follows newest activity, as does each folder's thread order.
    pub fn groups(&self) -> Vec<(String, Vec<ThreadSummary>)> {
        let mut groups: Vec<(String, Vec<ThreadSummary>)> = Vec::new();
        for thread in self.threads.iter().filter(|t| !t.archived) {
            if let Some((_, threads)) = groups.iter_mut().find(|(cwd, _)| cwd == &thread.cwd) {
                threads.push(thread.clone());
            } else { groups.push((thread.cwd.clone(), vec![thread.clone()])); }
        }
        groups
    }

    /// Applies an event from Lever. Returns true if visible state changed.
    pub fn apply(&mut self, event: AgentEvent, drawer_up: bool) -> bool {
        let before = self.mark;
        let changed = self.apply_event(event, drawer_up);
        self.refresh_mark();
        changed || self.mark != before
    }

    fn apply_event(&mut self, event: AgentEvent, drawer_up: bool) -> bool {
        match event {
            AgentEvent::Threads { threads, finished } => {
                let ended = self.threads.iter().any(|old| old.working && threads.iter().any(|t| t.thread == old.thread && !t.working));
                let working = threads.iter().any(|t| t.working);
                if let Some(selected) = threads.iter().find(|t| Some(t.thread.as_str()) == self.thread.as_deref()) {
                    self.title = (!selected.title.is_empty()).then(|| selected.title.clone());
                    self.model = selected.model.clone();
                }
                self.threads = threads;
                self.list_received = true;
                self.mark = if working { Mark::Working }
                    else if finished || ended { if drawer_up { Mark::Idle } else { Mark::Done } }
                    else if self.mark == Mark::Working { Mark::Idle } else { self.mark };
                true
            }
            AgentEvent::Lifecycle { thread, deleted, warning } => {
                if self.thread.as_deref() == Some(thread.as_str()) { self.loading = false; }
                if warning.is_some() { self.error = warning; }
                if deleted { self.threads.retain(|t| t.thread != thread); }
                else if let Some(t) = self.threads.iter_mut().find(|t| t.thread == thread) { t.archived = true; }
                if self.thread.as_deref() == Some(&thread) {
                    self.thread = None;
                    self.title = None;
                    self.model = None;
                    self.active_turn = None;
                    self.turns.clear();
                    self.list_view = true;
                }
                true
            }
            AgentEvent::Connected {
                status,
                resumed,
                preserved_thread,
                resume_error,
            } => {
                let mut changed = !self.connected
                    || self.error != resume_error
                    || self.missing_model.is_some();
                self.connected = true;
                self.sending = false;
                self.error = resume_error;
                self.missing_model = None;
                changed |= self.apply_status(status);
                if let Some(resumed) = resumed {
                    if self.thread.is_none() { self.list_view = false; }
                    self.apply_resumed(resumed, drawer_up);
                    changed = true;
                } else if let Some(tid) = preserved_thread {
                    // Keep the saved thread ID across a transient resume error
                    // or timeout so follow-ups stay on the person's thread.
                    // Because this socket did not subscribe via `resume`, never
                    // leave a cached `Working` turn wedging `can_send()`, and
                    // keep `working_before_disconnect` for a later successful
                    // resume of the same thread.
                    if self.working_before_disconnect.as_deref() != Some(tid.as_str()) {
                        self.working_before_disconnect = None;
                    }
                    if self.thread.as_deref() != Some(tid.as_str()) {
                        self.clear_thread();
                        self.thread = Some(tid);
                        changed = true;
                    } else {
                        if self.active_turn.take().is_some() || self.thread_mark != Mark::Idle {
                            self.thread_mark = Mark::Idle;
                            changed = true;
                        }
                        for turn in &mut self.turns {
                            if matches!(turn.state, TurnState::Working) {
                                turn.state = TurnState::Interrupted;
                                changed = true;
                            }
                        }
                    }
                } else if self.thread.is_some() {
                    self.clear_thread();
                    self.working_before_disconnect = None;
                    changed = true;
                } else {
                    self.working_before_disconnect = None;
                }
                changed
            }
            AgentEvent::Disconnected => {
                self.loading = false;
                let changed = self.connected
                    || self.sending
                    || self.lever_version.is_some()
                    || self.harness_started
                    || !self.harnesses.is_empty()
                    || self.no_harness
                    || self.missing_model.is_some()
                    || self.modes_loading
                    || self.thread_mark != Mark::Idle;
                if self.working() && let Some(thread) = self.thread.clone() {
                    self.working_before_disconnect = Some(thread);
                }
                // Lever says again which threads work and wait once the
                // shell connects again and sends `watch`.
                self.watching = false;
                self.working_threads.clear();
                self.waiting_threads.clear();
                self.modes_loading = false;
                self.connected = false;
                self.sending = false;
                self.active_turn = None;
                self.lever_version = None;
                self.harness_started = false;
                self.harnesses.clear();
                self.free_hosted_models = false;
                self.no_harness = false;
                self.missing_model = None;
                self.thread_mark = Mark::Idle;
                changed
            }
            AgentEvent::Status(status) => self.apply_status(status),
            AgentEvent::Resumed(resumed) => {
                self.apply_resumed(resumed, drawer_up);
                true
            }
            AgentEvent::Sent { thread, turn } => {
                self.sending = false;
                self.working_before_disconnect = None;
                if self.thread.as_deref() != Some(thread.as_str()) {
                    self.turns.clear();
                    self.title = None;
                    self.model = None;
                    // A new thread starts in the mode picked for it, or in
                    // the starting mode; Lever's `thread-updated` corrects it.
                    self.mode = self
                        .pending_mode
                        .take()
                        .or_else(|| self.default_mode.clone());
                }
                self.thread = Some(thread);
                self.active_turn = Some(turn);
                self.error = None;
                self.missing_model = None;
                self.thread_mark = Mark::Working;
                true
            }
            AgentEvent::Message {
                thread,
                turn,
                text,
                context,
            } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
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
                        requests: Vec::new(),
                        mode: self.mode.clone(),
                        answer: String::new(),
                        state: TurnState::Working,
                    });
                }
                self.error = None;
                self.thread_mark = Mark::Working;
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
            AgentEvent::Request {
                thread,
                turn,
                request,
            } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                let Some(existing_turn) = self.turns.iter_mut().find(|t| t.turn == turn) else {
                    return false;
                };
                // Each change of state comes with the same request ID.
                if let Some(existing) =
                    existing_turn.requests.iter_mut().find(|r| r.id == request.id)
                {
                    *existing = request;
                } else {
                    existing_turn.requests.push(request);
                }
                true
            }
            AgentEvent::ThreadUpdated {
                thread,
                title,
                mode,
            } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                let mut changed = false;
                if let Some(title) = title {
                    let next = if title.is_empty() { None } else { Some(title) };
                    if self.title != next {
                        self.title = next;
                        changed = true;
                    }
                }
                if let Some(mode) = mode
                    && self.mode.as_deref() != Some(mode.as_str())
                {
                    self.mode = Some(mode);
                    changed = true;
                }
                changed
            }
            AgentEvent::ModeSet { thread, mode } => {
                if self.thread.as_deref() != Some(thread.as_str())
                    || self.mode.as_deref() == Some(mode.as_str())
                {
                    return false;
                }
                self.mode = Some(mode);
                true
            }
            AgentEvent::DefaultMode(mode) => {
                if self.default_mode.as_deref() == Some(mode.as_str()) {
                    return false;
                }
                self.default_mode = Some(mode);
                true
            }
            AgentEvent::Modes(modes) => {
                self.modes_loading = false;
                self.modes_error = None;
                self.modes = modes;
                true
            }
            AgentEvent::Activity { working, waiting } => {
                let was_busy = self.watching
                    && (!self.working_threads.is_empty() || !self.waiting_threads.is_empty());
                let busy = !working.is_empty() || !waiting.is_empty();
                if was_busy && !busy && !drawer_up {
                    // The last working thread ended behind a put-away drawer.
                    self.thread_mark = Mark::Done;
                } else if busy && self.thread_mark != Mark::Working {
                    // New work clears a `done` no one has seen yet.
                    self.thread_mark = Mark::Idle;
                }
                let changed = !self.watching
                    || self.working_threads != working
                    || self.waiting_threads != waiting;
                self.watching = true;
                self.working_threads = working;
                self.waiting_threads = waiting;
                changed
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
                self.thread_mark = Mark::Done;
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
                self.thread_mark = if drawer_up { Mark::Idle } else { Mark::Done };
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
                self.thread_mark = if drawer_up { Mark::Idle } else { Mark::Done };
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
                self.thread_mark = if drawer_up { Mark::Idle } else { Mark::Done };
                true
            }
            AgentEvent::Error { code, message } => {
                self.loading = false;
                self.sending = false;
                // Lever's replies name no request, so an answer that was not
                // taken lets the person answer again.
                for turn in &mut self.turns {
                    for request in &mut turn.requests {
                        request.sending = None;
                    }
                }
                if self.modes_loading
                    && !matches!(code.as_str(), "no-model" | "no-thread" | "disconnected")
                {
                    // Asked while the mode menu opened: `models` failed.
                    self.modes_loading = false;
                    self.modes_error = Some("Modes are not available right now.".into());
                    return true;
                }
                self.modes_loading = false;
                if code == "no-model" {
                    self.missing_model = Some(message);
                    self.error = None;
                } else if code == "no-thread" {
                    self.clear_thread();
                    self.working_before_disconnect = None;
                    self.error = None;
                } else if code == "no-request" {
                    self.error = Some("That request no longer waits for an answer.".into());
                } else if code == "invalid-mode" {
                    self.error = Some("The Agent does not offer that mode.".into());
                } else {
                    self.error = Some(calm_turn_error(&message));
                }
                true
            }
        }
    }

    /// Summoning the drawer acknowledges a finished turn's `Done` mark.
    pub fn on_drawer_opened(&mut self) -> bool {
        self.acknowledge_done()
    }

    /// Putting the drawer away clears `Done` (already seen in the drawer),
    /// while leaving `Working` and `Waiting` so the mark tracks what goes on
    /// behind it.
    pub fn on_drawer_closed(&mut self) -> bool {
        self.acknowledge_done()
    }

    fn acknowledge_done(&mut self) -> bool {
        let before = self.mark;
        if self.thread_mark == Mark::Done {
            self.thread_mark = Mark::Idle;
        }
        self.refresh_mark();
        self.mark != before
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
    let trimmed = raw.trim();
    let stripped = if trimmed
        .get(.."internal error:".len())
        .is_some_and(|p| p.eq_ignore_ascii_case("internal error:"))
    {
        trimmed["internal error:".len()..].trim()
    } else {
        trimmed
    };
    let lower = stripped.to_ascii_lowercase();
    if is_network_error(&lower) {
        return "Could not reach the Model right now. Check your connection and try again.".into();
    }
    if is_auth_error(&lower) {
        return "The Model refused the current sign-in. Run `crowbar-agent login` in a terminal to sign in again.".into();
    }
    if lower.contains("lever is restarting") {
        return "The Agent is restarting after a settings change; try again once active turns finish.".into();
    }
    if lower.contains("lever is still saving") {
        return "The Agent is still finishing the previous turn; try again in a moment.".into();
    }
    if stripped.is_empty() {
        "The Model could not answer right now.".into()
    } else {
        stripped.to_string()
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActTone {
    Active,
    Failed,
    Muted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActDisplay {
    pub label: &'static str,
    pub tone: ActTone,
}

/// Returns display label and tone for rendering an act inside a turn.
/// Terminal turns never leave an unfinished act highlighted as `Working` or `Pending`.
pub fn act_display_status(turn_state: &TurnState, act: &Act) -> ActDisplay {
    let tone_for = |state: ActState| match state {
        ActState::Working => ActTone::Active,
        ActState::Failed => ActTone::Failed,
        ActState::Pending | ActState::Done => ActTone::Muted,
    };
    match turn_state {
        TurnState::Working => ActDisplay {
            label: act.state.label(),
            tone: tone_for(act.state),
        },
        TurnState::Stopped => {
            let interrupted_by_stop = matches!(act.state, ActState::Pending | ActState::Working)
                || (act.state == ActState::Failed
                    && act.detail.as_deref().is_some_and(|d| {
                        d.to_ascii_lowercase().contains("interrupted by the user")
                    }));
            if interrupted_by_stop {
                ActDisplay {
                    label: "Stopped",
                    tone: ActTone::Muted,
                }
            } else {
                ActDisplay {
                    label: act.state.label(),
                    tone: tone_for(act.state),
                }
            }
        }
        TurnState::Done => {
            if matches!(act.state, ActState::Pending | ActState::Working) {
                ActDisplay {
                    label: "Done",
                    tone: ActTone::Muted,
                }
            } else {
                ActDisplay {
                    label: act.state.label(),
                    tone: tone_for(act.state),
                }
            }
        }
        TurnState::Interrupted | TurnState::Failed(_) => {
            if matches!(act.state, ActState::Pending | ActState::Working) {
                ActDisplay {
                    label: "Interrupted",
                    tone: ActTone::Muted,
                }
            } else {
                ActDisplay {
                    label: act.state.label(),
                    tone: tone_for(act.state),
                }
            }
        }
    }
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

enum DeadlineIoError {
    TimedOut,
    Other(anyhow::Error),
}

enum FrameReadError {
    TooLong(usize),
    TimedOut,
    Other(anyhow::Error),
}

impl From<DeadlineIoError> for anyhow::Error {
    fn from(err: DeadlineIoError) -> Self {
        match err {
            DeadlineIoError::TimedOut => anyhow::anyhow!("Timed out waiting for Lever"),
            DeadlineIoError::Other(e) => e,
        }
    }
}

fn read_exact_before(
    stream: &mut UnixStream,
    buf: &mut [u8],
    deadline: Instant,
) -> std::result::Result<(), DeadlineIoError> {
    let mut read = 0;
    while read < buf.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(DeadlineIoError::TimedOut);
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|e| DeadlineIoError::Other(e.into()))?;
        match stream.read(&mut buf[read..]) {
            Ok(0) => {
                return Err(DeadlineIoError::Other(anyhow::anyhow!(
                    "Lever closed the surface socket before finishing the frame"
                )));
            }
            Ok(n) => read += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
                return Err(DeadlineIoError::TimedOut);
            }
            Err(e) => return Err(DeadlineIoError::Other(e.into())),
        }
    }
    Ok(())
}

fn write_all_before(
    stream: &mut UnixStream,
    buf: &[u8],
    deadline: Instant,
) -> std::result::Result<(), DeadlineIoError> {
    let mut written = 0;
    while written < buf.len() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(DeadlineIoError::TimedOut);
        }
        stream
            .set_write_timeout(Some(remaining))
            .map_err(|e| DeadlineIoError::Other(e.into()))?;
        match stream.write(&buf[written..]) {
            Ok(0) => {
                return Err(DeadlineIoError::Other(anyhow::anyhow!(
                    "Lever closed the surface socket while writing"
                )));
            }
            Ok(n) => written += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
                return Err(DeadlineIoError::TimedOut);
            }
            Err(e) => return Err(DeadlineIoError::Other(e.into())),
        }
    }
    Ok(())
}

fn read_frame_before(
    stream: &mut UnixStream,
    deadline: Instant,
) -> std::result::Result<serde_json::Value, FrameReadError> {
    let mut header = [0u8; 4];
    read_exact_before(stream, &mut header, deadline).map_err(|e| match e {
        DeadlineIoError::TimedOut => FrameReadError::TimedOut,
        DeadlineIoError::Other(err) => FrameReadError::Other(err),
    })?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME {
        return Err(FrameReadError::TooLong(len));
    }
    let mut body = vec![0u8; len];
    read_exact_before(stream, &mut body, deadline).map_err(|e| match e {
        DeadlineIoError::TimedOut => FrameReadError::TimedOut,
        DeadlineIoError::Other(err) => FrameReadError::Other(err),
    })?;
    serde_json::from_slice(&body)
        .context("Invalid JSON frame from Lever")
        .map_err(FrameReadError::Other)
}

fn write_frame_before(
    stream: &mut UnixStream,
    message: &serde_json::Value,
    deadline: Instant,
) -> std::result::Result<(), DeadlineIoError> {
    write_all_before(stream, &encode_frame(message), deadline)
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
                    default_mode: opt_str(msg, "default_mode"),
                }))
            }
            "models" => Some(LeverMessage::Models {
                modes: msg
                    .get("modes")
                    .and_then(|v| v.as_array())
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| {
                                let id = item.get("id")?.as_str()?.to_string();
                                let name = item
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                Some(ModeInfo { id, name })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            }),
            "mode-set" => Some(LeverMessage::ModeSet {
                thread: msg.get("thread")?.as_str()?.to_string(),
                mode: msg.get("mode")?.as_str()?.to_string(),
            }),
            "default-mode" => Some(LeverMessage::DefaultMode {
                mode: msg.get("mode")?.as_str()?.to_string(),
            }),
            "activity" => parse_activity(msg),
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
                let mode = opt_str(msg, "mode");
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
                    mode,
                    working,
                    turns,
                }))
            }
            "sent" => Some(LeverMessage::Sent {
                thread: msg.get("thread")?.as_str()?.to_string(),
                turn: msg.get("turn")?.as_u64()?,
            }),
            "archived" | "deleted" => Some(LeverMessage::Lifecycle {
                thread: msg.get("thread")?.as_str()?.into(), deleted: reply == "deleted",
                warning: msg.get("warning").and_then(|v| v.as_str()).map(str::to_string),
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
    if event == "activity" {
        return parse_activity(msg);
    }
    if event == "threads" {
        return Some(LeverMessage::Threads { threads: serde_json::from_value(msg.get("threads")?.clone()).ok()?,
            finished: msg.get("finished").and_then(|v| v.as_bool()).unwrap_or(false) });
    }
    if event == "deleted" || (event == "archived" && msg["archived"] == true) {
        return Some(LeverMessage::Lifecycle { thread: msg.get("thread")?.as_str()?.into(), deleted: event == "deleted", warning: None });
    }
    let thread = msg.get("thread")?.as_str()?.to_string();
    if event == "thread-updated" {
        return Some(LeverMessage::ThreadUpdated {
            thread,
            title: opt_str(msg, "title"),
            mode: opt_str(msg, "mode"),
        });
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
        "request" => Some(LeverMessage::Request {
            thread,
            turn,
            request: parse_request(msg)?,
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
        archived: value.get("archived").and_then(|v| v.as_bool()).unwrap_or(false),
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
        waiting: value
            .get("waiting")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        mode: opt_str(value, "mode"),
    })
}

fn opt_str(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn str_list(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn parse_activity(msg: &serde_json::Value) -> Option<LeverMessage> {
    Some(LeverMessage::Activity {
        working: str_list(msg, "working"),
        waiting: str_list(msg, "waiting"),
    })
}

fn parse_request(value: &serde_json::Value) -> Option<Request> {
    let id = value.get("request")?.as_str()?.to_string();
    let answers = value
        .get("answers")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(RequestAnswer {
                        answer: item.get("answer")?.as_str()?.to_string(),
                        name: item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let state = match value.get("state").and_then(|v| v.as_str()) {
        Some("answered") => RequestState::Answered,
        Some("withdrawn") => RequestState::Withdrawn,
        _ => RequestState::Waiting,
    };
    Some(Request {
        id,
        act: opt_str(value, "act"),
        kind: value
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("other")
            .to_string(),
        title: opt_str(value, "title").unwrap_or_default(),
        detail: opt_str(value, "detail").unwrap_or_default(),
        path: opt_str(value, "path"),
        answers,
        state,
        answer: opt_str(value, "answer"),
        sending: None,
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
    let requests = value
        .get("requests")
        .and_then(|v| v.as_array())
        .map(|items| items.iter().filter_map(parse_request).collect())
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
        requests,
        mode: opt_str(value, "mode"),
        answer,
        state,
    })
}

fn default_thread_state_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(|dir| PathBuf::from(dir).join("crowbaros-shell").join("active-thread"))
}

fn load_saved_thread(path: Option<&Path>) -> Option<String> {
    let path = path?;
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn save_active_thread(path: Option<&Path>, thread: Option<&str>) {
    let Some(path) = path else {
        return;
    };
    match thread {
        Some(tid) if !tid.trim().is_empty() => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
            if std::fs::write(&tmp, format!("{tid}\n")).is_ok()
                && std::fs::rename(&tmp, path).is_err()
            {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        _ => {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn is_routine_connect_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
            matches!(io.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused)
        })
    })
}

/// Starts the background client for `CROWBAR_LEVER_SOCKET`, if configured.
pub fn start_from_env(events: session::Sender) -> Option<CommandSender> {
    let path = std::env::var_os("CROWBAR_LEVER_SOCKET").map(PathBuf::from)?;
    Some(start_with_options(
        path,
        events,
        INIT_DEADLINE,
        default_thread_state_path(),
    ))
}

/// Starts the background client for a specific surface socket path.
#[cfg(test)]
pub fn start(path: PathBuf, events: session::Sender) -> CommandSender {
    start_with_options(path, events, INIT_DEADLINE, None)
}

fn start_with_options(
    path: PathBuf,
    events: session::Sender,
    init_deadline: Duration,
    state_path: Option<PathBuf>,
) -> CommandSender {
    let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>();
    let writer: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
    let initial_thread = load_saved_thread(state_path.as_deref());
    let active_thread: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(initial_thread));

    // Writer thread: forwards UI commands to the currently connected stream.
    let writer_for_cmds = writer.clone();
    let events_for_cmds = events.clone();
    let active_for_cmds = active_thread.clone();
    let path_for_cmds = state_path.clone();
    std::thread::spawn(move || {
        for cmd in cmd_rx {
            if let AgentCommand::Select { thread } = &cmd {
                if let Ok(mut active) = active_for_cmds.lock() { *active = thread.clone(); }
                save_active_thread(path_for_cmds.as_deref(), thread.as_deref());
                if thread.is_none() { continue; }
            }
            let op = match &cmd {
                AgentCommand::Select { .. } => "resume",
                AgentCommand::Lifecycle { .. } => "lifecycle",
                AgentCommand::Send { .. } => "send",
                AgentCommand::Stop { .. } => "stop",
                AgentCommand::Answer { .. } => "answer",
                AgentCommand::SetMode { .. } => "set-mode",
                AgentCommand::SetDefaultMode { .. } => "set-default-mode",
                AgentCommand::FetchModes => "models",
                AgentCommand::RefreshStatus => "status",
            };
            let request = match cmd {
                AgentCommand::Select { thread } => serde_json::json!({"op":"resume","thread":thread}),
                AgentCommand::Lifecycle { thread, delete } => serde_json::json!({"op":if delete {"delete"} else {"archive"},"thread":thread}),
                AgentCommand::Send { thread, text, mode } => {
                    let mut request = match thread {
                        Some(thread) => {
                            serde_json::json!({ "op": "send", "thread": thread, "text": text })
                        }
                        None => serde_json::json!({ "op": "send", "thread": null, "text": text }),
                    };
                    if let Some(mode) = mode {
                        request["mode"] = serde_json::Value::String(mode);
                    }
                    request
                }
                AgentCommand::Answer {
                    thread,
                    request,
                    answer,
                } => serde_json::json!({
                    "op": "answer", "thread": thread, "request": request, "answer": answer
                }),
                AgentCommand::SetMode { thread, mode } => {
                    serde_json::json!({ "op": "set-mode", "thread": thread, "mode": mode })
                }
                AgentCommand::SetDefaultMode { mode } => {
                    serde_json::json!({ "op": "set-default-mode", "mode": mode })
                }
                AgentCommand::FetchModes => serde_json::json!({ "op": "models" }),
                AgentCommand::Stop { thread, turn } => match turn {
                    Some(turn) => {
                        serde_json::json!({ "op": "stop", "thread": thread, "turn": turn })
                    }
                    None => serde_json::json!({ "op": "stop", "thread": thread }),
                },
                AgentCommand::RefreshStatus => serde_json::json!({ "op": "status" }),
            };
            let frame = encode_frame(&request);
            // Hold the mutex across write and failure teardown so we never
            // shut down a freshly reconnected stream installed by the reader.
            // Let the reader thread alone emit `Disconnected` when its read ends.
            let write_outcome = {
                let mut guard = match writer_for_cmds.lock() {
                    Ok(g) => g,
                    Err(_) => break,
                };
                match guard.as_mut() {
                    Some(stream) => match stream.write_all(&frame) {
                        Ok(()) => Some(Ok(())),
                        Err(e) => {
                            log::warn!("Agent surface socket write failed: {e}");
                            if let Some(dead) = guard.take() {
                                let _ = dead.shutdown(std::net::Shutdown::Both);
                            }
                            Some(Err(e))
                        }
                    },
                    None => None,
                }
            };
            if !matches!(write_outcome, Some(Ok(()))) {
                match op {
                    "send" | "resume" | "lifecycle" => {
                        let _ = events_for_cmds.send(session::Event::Agent(AgentEvent::Error {
                            code: "disconnected".into(),
                            message: "The Agent is not reachable right now.".into(),
                        }));
                    }
                    "stop" => {
                        let _ = events_for_cmds.send(session::Event::Agent(AgentEvent::Error {
                            code: "disconnected".into(),
                            message: "Could not send Stop — the Agent is not reachable right now."
                                .into(),
                        }));
                    }
                    "answer" | "set-mode" | "set-default-mode" | "models" => {
                        let _ = events_for_cmds.send(session::Event::Agent(AgentEvent::Error {
                            code: "disconnected".into(),
                            message: "The Agent is not reachable right now.".into(),
                        }));
                    }
                    _ => {}
                }
            }
        }
    });

    // Reader / reconnect thread: connects with exponential backoff, holds
    // `Connected` until initial `status` (and thread restore) arrive within
    // `init_deadline`, and then blocks on live reads with zero idle wakeups.
    std::thread::spawn(move || {
        let mut backoff = INITIAL_BACKOFF;
        let mut skipped_resume: Option<(String, SkipResumeReason)> = None;
        let mut last_bootstrap_warn: Option<String> = None;
        // Only fall back to `list` on a truly fresh shell start with no saved
        // thread ID; once we have connected or a saved thread was deleted/skipped,
        // never silently adopt an unrelated thread on a subsequent reconnect.
        let mut allow_list_fallback = active_thread
            .lock()
            .ok()
            .is_none_or(|g| g.is_none());
        loop {
            let existing_thread = active_thread.lock().ok().and_then(|g| g.clone());
            match connect_and_bootstrap(
                &path,
                existing_thread,
                allow_list_fallback,
                skipped_resume.as_ref().map(|(t, r)| (t.as_str(), *r)),
                init_deadline,
            ) {
                Ok(BootstrapResult {
                    mut stream,
                    status,
                    resumed,
                    preserved_thread,
                    resume_error,
                }) => {
                    backoff = INITIAL_BACKOFF;
                    last_bootstrap_warn = None;
                    allow_list_fallback = false;
                    // Subscribe before exposing the writer, preserving request/reply order.
                    // A failed write is followed by EOF/error in the read loop below.
                    let _ = stream.write_all(&encode_frame(&serde_json::json!({"op":"list","watch":true})));
                    // Only keep a permanent skip for oversized history frames; a
                    // timed-out resume is skipped for this reconnect so the drawer
                    // comes online, then retried on a future reconnect.
                    if matches!(skipped_resume, Some((_, SkipResumeReason::TimedOut))) {
                        skipped_resume = None;
                    }
                    let Ok(write_half) = stream.try_clone() else {
                        std::thread::sleep(backoff);
                        continue;
                    };
                    let _ = write_half.set_write_timeout(Some(Duration::from_secs(5)));
                    let current_tid = resumed
                        .as_ref()
                        .map(|r| r.thread.clone())
                        .or_else(|| preserved_thread.clone());
                    if let Ok(mut guard) = active_thread.lock() {
                        *guard = current_tid.clone();
                    }
                    save_active_thread(state_path.as_deref(), current_tid.as_deref());
                    if let Ok(mut guard) = writer.lock() {
                        *guard = Some(write_half);
                    }
                    log::info!("SHELL agent-socket connected=true");
                    if events
                        .send(session::Event::Agent(AgentEvent::Connected {
                            status,
                            resumed,
                            preserved_thread,
                            resume_error,
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
                                            AgentEvent::Sent { thread, .. } => {
                                                skipped_resume = None;
                                                if let Ok(mut guard) = active_thread.lock() {
                                                    *guard = Some(thread.clone());
                                                }
                                                save_active_thread(
                                                    state_path.as_deref(),
                                                    Some(thread.as_str()),
                                                );
                                            }
                                            AgentEvent::Lifecycle { thread, .. } => {
                                                if let Ok(mut active) = active_thread.lock() && active.as_deref() == Some(thread) {
                                                    *active = None;
                                                    save_active_thread(state_path.as_deref(), None);
                                                }
                                            }
                                            AgentEvent::Resumed(r) => {
                                                skipped_resume = None;
                                                if let Ok(mut guard) = active_thread.lock() {
                                                    *guard = Some(r.thread.clone());
                                                }
                                                save_active_thread(
                                                    state_path.as_deref(),
                                                    Some(r.thread.as_str()),
                                                );
                                            }
                                            AgentEvent::Error { code, .. }
                                                if code == "no-thread" =>
                                            {
                                                if let Ok(mut guard) = active_thread.lock() {
                                                    *guard = None;
                                                }
                                                save_active_thread(state_path.as_deref(), None);
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
                Err(BootstrapError::ResumeFrameTooLong { thread, bytes }) => {
                    log::warn!(
                        "Agent thread {thread} resume reply exceeded {MAX_FRAME} bytes ({bytes} bytes); reconnecting without restoring its history"
                    );
                    skipped_resume = Some((thread, SkipResumeReason::TooLong));
                    if let Ok(mut guard) = active_thread.lock() {
                        *guard = None;
                    }
                    save_active_thread(state_path.as_deref(), None);
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(BootstrapError::ResumeTimedOut { thread }) => {
                    log::warn!(
                        "Timed out restoring Agent thread {thread}; reconnecting while keeping thread selection"
                    );
                    skipped_resume = Some((thread, SkipResumeReason::TimedOut));
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(BootstrapError::Fatal(err)) => {
                    if !is_routine_connect_error(&err) {
                        let msg = err.to_string();
                        if last_bootstrap_warn.as_deref() != Some(msg.as_str()) {
                            log::warn!("Agent surface bootstrap failed: {msg}");
                            last_bootstrap_warn = Some(msg);
                        }
                    }
                }
            }
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    });

    CommandSender { tx: cmd_tx }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SkipResumeReason {
    TooLong,
    TimedOut,
}

enum ResumeOutcome {
    Resumed(ResumedThread),
    NotFound,
    RecoverableError(String),
}

enum ResumeError {
    TooLong(usize),
    TimedOut,
    Fatal(anyhow::Error),
}

enum BootstrapError {
    ResumeFrameTooLong { thread: String, bytes: usize },
    ResumeTimedOut { thread: String },
    Fatal(anyhow::Error),
}

struct BootstrapResult {
    stream: UnixStream,
    status: StatusInfo,
    resumed: Option<ResumedThread>,
    preserved_thread: Option<String>,
    resume_error: Option<String>,
}

fn bootstrap_error_copy(code: &str, message: &str) -> String {
    if code == "storage-error" {
        "Could not load the previous thread right now. Try again in a moment.".into()
    } else {
        calm_turn_error(message)
    }
}

fn resume_thread_before(
    stream: &mut UnixStream,
    thread_id: &str,
    deadline: Instant,
) -> std::result::Result<ResumeOutcome, ResumeError> {
    if let Err(e) = write_frame_before(
        stream,
        &serde_json::json!({ "op": "resume", "thread": thread_id }),
        deadline,
    ) {
        return Err(match e {
            DeadlineIoError::TimedOut => ResumeError::TimedOut,
            DeadlineIoError::Other(err) => ResumeError::Fatal(err),
        });
    }
    let frame = match read_frame_before(stream, deadline) {
        Ok(f) => f,
        Err(FrameReadError::TooLong(bytes)) => return Err(ResumeError::TooLong(bytes)),
        Err(FrameReadError::TimedOut) => return Err(ResumeError::TimedOut),
        Err(FrameReadError::Other(e)) => return Err(ResumeError::Fatal(e)),
    };
    match parse_lever_message(&frame) {
        Some(LeverMessage::Resumed(r)) => Ok(ResumeOutcome::Resumed(r)),
        Some(LeverMessage::Error { code, .. }) if code == "no-thread" => {
            Ok(ResumeOutcome::NotFound)
        }
        Some(LeverMessage::Error { code, message }) => {
            log::warn!("Lever could not resume thread {thread_id} ({code}): {message}");
            Ok(ResumeOutcome::RecoverableError(bootstrap_error_copy(
                &code, &message,
            )))
        }
        other => Err(ResumeError::Fatal(anyhow::anyhow!(
            "Unexpected resume reply from Lever: {other:?}"
        ))),
    }
}

fn read_bootstrap_frame(stream: &mut UnixStream, deadline: Instant) -> Result<serde_json::Value> {
    match read_frame_before(stream, deadline) {
        Ok(frame) => Ok(frame),
        Err(FrameReadError::TooLong(bytes)) => bail!("Lever frame too long ({bytes} bytes)"),
        Err(FrameReadError::TimedOut) => bail!("Timed out waiting for Lever"),
        Err(FrameReadError::Other(err)) => Err(err),
    }
}

/// Connects to Lever's surface socket, completes the version-1 handshake,
/// fetches initial `status`, restores the active thread (or, on a fresh shell
/// start with no saved thread, a thread that waits for the person, else the
/// working thread if any, else the newest thread), and asks to `watch` every
/// thread's activity, before clearing the read timeout. Fails if Lever does
/// not answer within `init_deadline`.
fn connect_and_bootstrap(
    path: &PathBuf,
    existing_thread: Option<String>,
    allow_list_fallback: bool,
    skip_resume: Option<(&str, SkipResumeReason)>,
    init_deadline: Duration,
) -> std::result::Result<BootstrapResult, BootstrapError> {
    let deadline = Instant::now() + init_deadline;
    let mut stream = UnixStream::connect(path).map_err(|e| BootstrapError::Fatal(e.into()))?;

    // Handshake.
    write_frame_before(
        &mut stream,
        &serde_json::json!({ "version": SURFACE_VERSION }),
        deadline,
    )
    .map_err(|e| BootstrapError::Fatal(e.into()))?;
    let reply = read_bootstrap_frame(&mut stream, deadline).map_err(BootstrapError::Fatal)?;
    if reply != serde_json::json!({ "version": SURFACE_VERSION }) {
        return Err(BootstrapError::Fatal(anyhow::anyhow!(
            "Lever did not speak surface protocol version {SURFACE_VERSION}"
        )));
    }

    // Initial status (must arrive within `deadline` before reporting Connected).
    write_frame_before(&mut stream, &serde_json::json!({ "op": "status" }), deadline)
        .map_err(|e| BootstrapError::Fatal(e.into()))?;
    let status_frame =
        read_bootstrap_frame(&mut stream, deadline).map_err(BootstrapError::Fatal)?;
    let status = match parse_lever_message(&status_frame) {
        Some(LeverMessage::Status(status)) => status,
        other => {
            return Err(BootstrapError::Fatal(anyhow::anyhow!(
                "Unexpected initial status reply from Lever: {other:?}"
            )));
        }
    };

    // Restore thread: if the previous thread was skipped because its history
    // exceeded MAX_FRAME (which also cleared `active_thread`; the skip itself
    // clears as soon as Sent/Message/Resumed adopts a thread), stay at a fresh
    // drawer without adopting another thread. Otherwise resume the held/saved
    // thread if any, or ask `list` only on a fresh start with no saved thread.
    let mut resumed = None;
    let mut preserved_thread = None;
    let mut resume_error = None;
    if matches!(skip_resume, Some((_, SkipResumeReason::TooLong))) {
        resume_error = Some(
            "Could not load the previous thread's history because it is too large. Start a new thread or inspect it from a terminal."
                .into(),
        );
    } else if let Some(tid) = existing_thread {
        match skip_resume {
            Some((skipped, SkipResumeReason::TimedOut)) if skipped == tid => {
                preserved_thread = Some(tid);
                resume_error = Some(
                    "Timed out loading the previous thread's history. Your thread is kept — send a message to continue."
                        .into(),
                );
            }
            _ => match resume_thread_before(&mut stream, &tid, deadline) {
                Ok(ResumeOutcome::Resumed(r)) => resumed = Some(r),
                Ok(ResumeOutcome::NotFound) => {}
                Ok(ResumeOutcome::RecoverableError(msg)) => {
                    preserved_thread = Some(tid);
                    resume_error = Some(msg);
                }
                Err(ResumeError::TooLong(bytes)) => {
                    return Err(BootstrapError::ResumeFrameTooLong { thread: tid, bytes });
                }
                Err(ResumeError::TimedOut) => {
                    return Err(BootstrapError::ResumeTimedOut { thread: tid });
                }
                Err(ResumeError::Fatal(e)) => return Err(BootstrapError::Fatal(e)),
            },
        }
    } else if allow_list_fallback {
        write_frame_before(&mut stream, &serde_json::json!({ "op": "list" }), deadline)
            .map_err(|e| BootstrapError::Fatal(e.into()))?;
        let list_frame =
            read_bootstrap_frame(&mut stream, deadline).map_err(BootstrapError::Fatal)?;
        match parse_lever_message(&list_frame) {
            Some(LeverMessage::List { threads }) => {
                if let Some(chosen) = threads
                    .iter()
                    .find(|t| t.waiting)
                    .or_else(|| threads.iter().find(|t| t.working))
                    .or_else(|| threads.first())
                {
                    match skip_resume {
                        Some((skipped, SkipResumeReason::TimedOut)) if skipped == chosen.thread => {
                            resume_error = Some(
                                "Timed out loading the previous thread's history. Send a message to start a new thread."
                                    .into(),
                            );
                        }
                        _ => match resume_thread_before(&mut stream, &chosen.thread, deadline) {
                            Ok(ResumeOutcome::Resumed(r)) => resumed = Some(r),
                            Ok(ResumeOutcome::NotFound) => {}
                            Ok(ResumeOutcome::RecoverableError(msg)) => resume_error = Some(msg),
                            Err(ResumeError::TooLong(bytes)) => {
                                return Err(BootstrapError::ResumeFrameTooLong {
                                    thread: chosen.thread.clone(),
                                    bytes,
                                });
                            }
                            Err(ResumeError::TimedOut) => {
                                return Err(BootstrapError::ResumeTimedOut {
                                    thread: chosen.thread.clone(),
                                });
                            }
                            Err(ResumeError::Fatal(e)) => return Err(BootstrapError::Fatal(e)),
                        },
                    }
                }
            }
            Some(LeverMessage::Error { code, message }) => {
                resume_error = Some(bootstrap_error_copy(&code, &message));
            }
            other => {
                return Err(BootstrapError::Fatal(anyhow::anyhow!(
                    "Unexpected list reply from Lever: {other:?}"
                )));
            }
        }
    }

    // Ask which threads work and wait, for the mark. The reply and each
    // `activity` after it arrive on the reader loop. A write that fails
    // here finds the connection closed, which the reader loop sees too.
    if let Err(e) = write_frame_before(&mut stream, &serde_json::json!({ "op": "watch" }), deadline)
    {
        let e: anyhow::Error = e.into();
        log::warn!("Asking Lever to watch threads: {e}");
    }

    // Clear read timeout so the idle reader thread blocks with zero wakeups.
    stream
        .set_read_timeout(None)
        .map_err(|e| BootstrapError::Fatal(e.into()))?;
    Ok(BootstrapResult {
        stream,
        status,
        resumed,
        preserved_thread,
        resume_error,
    })
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
            default_mode: Some("ask-first".into()),
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
                "free_hosted_models": true,
                "default_mode": "full-access"
            })),
            Some(LeverMessage::Status(StatusInfo {
                lever_version: "0.1.0".into(),
                harness_started: true,
                harnesses: vec!["OpenCode".into()],
                free_hosted_models: true,
                default_mode: Some("full-access".into()),
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
                    "working": false,
                    "waiting": true,
                    "mode": "plan"
                }]
            })),
            Some(LeverMessage::List {
                threads: vec![ThreadSummary {
                    archived: false,
                    thread: "t-123".into(),
                    title: "Check disk".into(),
                    cwd: "/home/crowbar".into(),
                    model: Some("stub/stub-1".into()),
                    started: 100,
                    updated: 105,
                    turns: 2,
                    state: "done".into(),
                    working: false,
                    waiting: true,
                    mode: Some("plan".into()),
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
            Some(LeverMessage::ThreadUpdated {
                thread: "t-1".into(),
                title: Some("Inspect workspace".into()),
                mode: None,
            })
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
                preserved_thread: None,
                resume_error: None,
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
            AgentEvent::ThreadUpdated {
                thread: "t-abc".into(),
                title: Some("Focus check".into()),
                mode: None,
            },
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
                preserved_thread: None,
                resume_error: None,
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
                mode: None,
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
            requests: Vec::new(),
            mode: None,
            answer: "ok".into(),
            state: TurnState::Done,
        });
        state.turns.push(Turn {
            turn: 2,
            message: "stopped".into(),
            context: None,
            acts: Vec::new(),
            notes: Vec::new(),
            requests: Vec::new(),
            mode: None,
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
        state.thread_mark = Mark::Working;
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
        assert_eq!(
            calm_turn_error("Internal error: Stub model declined this turn"),
            "Stub model declined this turn"
        );
        assert!(
            !calm_turn_error("Lever is restarting the harness after settings changed")
                .contains("Lever")
        );
        assert!(
            !calm_turn_error("Lever is still saving how this thread's last turn ended.")
                .contains("Lever")
        );
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

        // Stopped behind a put-away drawer shows Done, and Disconnected resets
        // both Mark and stale missing_model.
        state.apply(AgentEvent::Sent { thread: "t-9".into(), turn: 1 }, false);
        state.apply(
            AgentEvent::Message { thread: "t-9".into(), turn: 1, text: "bg".into(), context: None },
            false,
        );
        state.apply(AgentEvent::Stopped { thread: "t-9".into(), turn: 1 }, false);
        assert_eq!(state.mark, Mark::Done);
        state.missing_model = Some("old".into());
        state.apply(AgentEvent::Disconnected, false);
        assert_eq!(state.mark, Mark::Idle);
        assert_eq!(state.missing_model, None);
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
        let late = tx.clone();
        let test_deadline = Duration::from_secs(10);
        let _sender = start_with_options(sock.clone(), tx, Duration::from_millis(180), None);
        std::thread::spawn(move || {
            std::thread::sleep(test_deadline);
            let _ = late.send(session::Event::Agent(AgentEvent::Error {
                code: "test-deadline".into(),
                message: String::new(),
            }));
        });

        let mut events_seen = Vec::new();
        let mut state = AgentState::new(true);
        futures::executor::block_on(async {
            while let Some(ev) = rx.next().await {
                if let session::Event::Agent(agent_ev) = ev {
                    if matches!(&agent_ev, AgentEvent::Error { code, .. } if code == "test-deadline")
                    {
                        panic!("timed out waiting for Connected after retry");
                    }
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

            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({"op":"list","watch":true}));
            peer.write_all(&encode_frame(&serde_json::json!({"reply":"list","threads":[]}))).unwrap();
            // After restoring the thread, the client asks to watch every
            // thread's activity for the mark.
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "op": "watch" }));
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "activity", "working": [], "waiting": []
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
        let late = tx.clone();
        let test_deadline = Duration::from_secs(10);
        let sender = start(sock.clone(), tx);
        std::thread::spawn(move || {
            std::thread::sleep(test_deadline);
            let _ = late.send(session::Event::Agent(AgentEvent::Error {
                code: "test-deadline".into(),
                message: String::new(),
            }));
        });

        let mut state = AgentState::new(true);
        let mut sent_turn2 = false;
        let mut stopped_turn2 = false;
        futures::executor::block_on(async {
            while let Some(ev) = rx.next().await {
                if let session::Event::Agent(agent_ev) = ev {
                    if matches!(&agent_ev, AgentEvent::Error { code, .. } if code == "test-deadline")
                    {
                        panic!("timed out waiting for resume/send/stop sequence");
                    }
                    state.apply(agent_ev, true);
                    if state.connected && !sent_turn2 {
                        assert_eq!(state.thread.as_deref(), Some("t-existing"));
                        assert_eq!(state.turns.len(), 1);
                        assert_eq!(state.turns[0].acts[0].detail.as_deref(), Some("crowbar"));
                        sent_turn2 = true;
                        sender.send_message(state.thread.clone(), "long command".into(), None);
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

    fn run_with_deadline<F>(
        rx: &mut futures::channel::mpsc::UnboundedReceiver<session::Event>,
        tx: &session::Sender,
        deadline: Duration,
        mut on_event: F,
    ) where
        F: FnMut(AgentEvent) -> bool,
    {
        let late = tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(deadline);
            let _ = late.send(session::Event::Agent(AgentEvent::Error {
                code: "test-deadline".into(),
                message: String::new(),
            }));
        });
        futures::executor::block_on(async {
            while let Some(ev) = rx.next().await {
                if let session::Event::Agent(agent_ev) = ev {
                    if matches!(&agent_ev, AgentEvent::Error { code, .. } if code == "test-deadline")
                    {
                        panic!("test timed out after {deadline:?}");
                    }
                    if on_event(agent_ev) {
                        break;
                    }
                }
            }
        });
    }

    #[test]
    fn surface_client_stays_connected_when_resume_returns_storage_error() {
        let dir = short_temp_dir("cs-lever-resume-err");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": false,
                "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                "free_hosted_models": false
            })))
            .unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "list");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "list",
                "threads": [{
                    "thread": "t-corrupt",
                    "title": "Corrupt",
                    "cwd": "/tmp",
                    "started": 1,
                    "updated": 2,
                    "turns": 1,
                    "state": "done",
                    "working": false
                }]
            })))
            .unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "resume");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "error",
                "code": "storage-error",
                "message": "database is locked"
            })))
            .unwrap();
        });

        let (tx, mut rx) = session::channel();
        let _sender = start(sock.clone(), tx.clone());
        let mut state = AgentState::new(true);
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            state.apply(ev, true);
            state.connected
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(state.connected);
        assert!(state.can_send());
        assert_eq!(state.thread, None);
        assert!(
            state
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Could not load the previous thread"),
            "expected calm storage-error copy, got {:?}",
            state.error
        );
    }

    #[test]
    fn surface_client_recovers_when_resume_frame_exceeds_max_frame() {
        let dir = short_temp_dir("cs-lever-oversize");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            for attempt in 0..2 {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
                peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })))
                .unwrap();
                if attempt == 0 {
                    assert_eq!(read_frame(&mut peer).unwrap()["op"], "list");
                    peer.write_all(&encode_frame(&serde_json::json!({
                        "reply": "list",
                        "threads": [{
                            "thread": "t-huge",
                            "title": "Huge",
                            "cwd": "/tmp",
                            "started": 1,
                            "updated": 2,
                            "turns": 500,
                            "state": "done",
                            "working": false
                        }]
                    })))
                    .unwrap();
                    assert_eq!(read_frame(&mut peer).unwrap()["op"], "resume");
                    // Advertise a frame larger than MAX_FRAME (64 MiB + 1).
                    peer.write_all(&((MAX_FRAME as u32) + 1).to_be_bytes()).unwrap();
                }
            }
        });

        let (tx, mut rx) = session::channel();
        let _sender = start(sock.clone(), tx.clone());
        let mut state = AgentState::new(true);
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            state.apply(ev, true);
            state.connected
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(state.connected);
        assert!(state.can_send());
        assert_eq!(state.thread, None);
        assert!(
            state.error.as_deref().unwrap_or("").contains("too large"),
            "expected oversized history warning, got {:?}",
            state.error
        );
    }

    #[test]
    fn surface_client_restores_persisted_thread_over_newer_working_thread() {
        let dir = short_temp_dir("cs-lever-persist");
        let sock = dir.join("surface");
        let state_path = dir.join("active-thread");
        save_active_thread(Some(&state_path), Some("t-drawer"));
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": true,
                "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                "free_hosted_models": false
            })))
            .unwrap();
            // Client must resume its persisted `t-drawer` directly without falling back to `list`.
            let req = read_frame(&mut peer).unwrap();
            assert_eq!(req, serde_json::json!({ "op": "resume", "thread": "t-drawer" }));
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "resumed",
                "thread": "t-drawer",
                "title": "Drawer thread",
                "cwd": "/tmp",
                "started": 10,
                "updated": 11,
                "working": false,
                "turns": [{
                    "turn": 1,
                    "message": "from drawer",
                    "answer": "drawer answer",
                    "state": "done",
                    "acts": [],
                    "notes": []
                }]
            })))
            .unwrap();
        });

        let (tx, mut rx) = session::channel();
        let _sender = start_with_options(sock.clone(), tx.clone(), INIT_DEADLINE, Some(state_path));
        let mut state = AgentState::new(true);
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            state.apply(ev, true);
            state.connected
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(state.thread.as_deref(), Some("t-drawer"));
        assert_eq!(state.turns.len(), 1);
        assert_eq!(state.turns[0].answer, "drawer answer");
    }

    #[test]
    fn surface_client_reports_error_when_stop_sent_while_disconnected() {
        let dir = short_temp_dir("cs-lever-stop-offline");
        let missing = dir.join("missing");
        let (tx, mut rx) = session::channel();
        let sender = start(missing, tx.clone());
        sender.stop_turn("t-1".into(), Some(1));

        let mut got = None;
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            if let AgentEvent::Error { code, message } = ev {
                got = Some((code, message));
                return true;
            }
            false
        });
        let _ = std::fs::remove_dir_all(&dir);
        let (code, msg) = got.expect("expected disconnected error for Stop");
        assert_eq!(code, "disconnected");
        assert!(msg.contains("Stop"), "{msg}");
    }

    #[test]
    fn surface_client_enforces_monotonic_deadline_on_fragmented_status_reply() {
        let dir = short_temp_dir("cs-lever-drip");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            // Attempt 1: drip 1 byte every 25ms past the 120ms deadline.
            let (mut slow, _) = listener.accept().unwrap();
            let _ = read_frame(&mut slow);
            let _ = slow.write_all(&encode_frame(&serde_json::json!({ "version": 1 })));
            let _ = read_frame(&mut slow);
            let frame = encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": false,
                "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                "free_hosted_models": false
            }));
            for byte in &frame {
                if slow.write_all(&[*byte]).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            drop(slow);

            // Attempt 2: reply immediately.
            let (mut ok, _) = listener.accept().unwrap();
            let _ = read_frame(&mut ok);
            ok.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
            let _ = read_frame(&mut ok);
            ok.write_all(&encode_frame(&serde_json::json!({
                "reply": "status",
                "version": "0.1.0",
                "harness_started": false,
                "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                "free_hosted_models": false
            })))
            .unwrap();
            let _ = read_frame(&mut ok);
            ok.write_all(&encode_frame(&serde_json::json!({ "reply": "list", "threads": [] })))
                .unwrap();
        });

        let (tx, mut rx) = session::channel();
        let _sender = start_with_options(sock.clone(), tx.clone(), Duration::from_millis(120), None);
        let mut connected_count = 0usize;
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            if matches!(ev, AgentEvent::Connected { .. }) {
                connected_count += 1;
                return true;
            }
            false
        });
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(connected_count, 1);
    }

    #[test]
    fn surface_client_preserves_saved_thread_across_storage_error_and_retries_it() {
        let dir = short_temp_dir("cs-lever-keep-tid");
        let sock = dir.join("surface");
        let state_path = dir.join("active-thread");
        save_active_thread(Some(&state_path), Some("t-drawer"));
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            // Attempt 1: saved thread returns transient storage-error, then disconnects.
            {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
                peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })))
                .unwrap();
                assert_eq!(
                    read_frame(&mut peer).unwrap(),
                    serde_json::json!({ "op": "resume", "thread": "t-drawer" })
                );
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "error",
                    "code": "storage-error",
                    "message": "database is locked"
                })))
                .unwrap();
                // Wait briefly so client emits Connected before disconnect.
                std::thread::sleep(Duration::from_millis(80));
            }
            // Attempt 2: client must retry `t-drawer` (never falling back to `list`).
            {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
                peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })))
                .unwrap();
                assert_eq!(
                    read_frame(&mut peer).unwrap(),
                    serde_json::json!({ "op": "resume", "thread": "t-drawer" })
                );
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "resumed",
                    "thread": "t-drawer",
                    "title": "Drawer",
                    "cwd": "/tmp",
                    "started": 1,
                    "updated": 2,
                    "working": false,
                    "turns": [{
                        "turn": 1,
                        "message": "hi",
                        "answer": "preserved",
                        "state": "done",
                        "acts": [],
                        "notes": []
                    }]
                })))
                .unwrap();
            }
        });

        let (tx, mut rx) = session::channel();
        let _sender =
            start_with_options(sock.clone(), tx.clone(), INIT_DEADLINE, Some(state_path.clone()));
        let mut state = AgentState::new(true);
        let mut first_connect_checked = false;
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            let is_conn = matches!(ev, AgentEvent::Connected { .. });
            state.apply(ev, true);
            if is_conn && !first_connect_checked {
                first_connect_checked = true;
                assert_eq!(state.thread.as_deref(), Some("t-drawer"));
                assert!(state.error.is_some());
                assert_eq!(load_saved_thread(Some(&state_path)).as_deref(), Some("t-drawer"));
                return false;
            }
            is_conn && first_connect_checked && state.turns.len() == 1
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(state.thread.as_deref(), Some("t-drawer"));
        assert_eq!(state.turns[0].answer, "preserved");
        assert_eq!(state.error, None);
    }

    #[test]
    fn surface_client_recovers_when_resume_times_out_preserving_saved_thread() {
        let dir = short_temp_dir("cs-lever-resume-timeout");
        let sock = dir.join("surface");
        let state_path = dir.join("active-thread");
        save_active_thread(Some(&state_path), Some("t-drawer"));
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            // Attempt 1: status succeeds, resume stalls past deadline.
            {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let _ = read_frame(&mut peer);
                let _ = peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 })));
                let _ = read_frame(&mut peer);
                let _ = peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })));
                assert_eq!(
                    read_frame(&mut peer).unwrap(),
                    serde_json::json!({ "op": "resume", "thread": "t-drawer" })
                );
                std::thread::sleep(Duration::from_millis(260));
            }
            // Attempt 2: client skips resume for this reconnect and connects online with `t-drawer` preserved.
            {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let _ = read_frame(&mut peer);
                peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
                let _ = read_frame(&mut peer);
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })))
                .unwrap();
            }
        });

        let (tx, mut rx) = session::channel();
        let _sender = start_with_options(
            sock.clone(),
            tx.clone(),
            Duration::from_millis(140),
            Some(state_path.clone()),
        );
        let mut state = AgentState::new(true);
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            state.apply(ev, true);
            state.connected
        });

        server.join().unwrap();
        let saved = load_saved_thread(Some(&state_path));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(state.connected);
        assert_eq!(state.thread.as_deref(), Some("t-drawer"));
        assert_eq!(saved.as_deref(), Some("t-drawer"));
        assert!(
            state.error.as_deref().unwrap_or("").contains("Timed out"),
            "expected timeout warning, got {:?}",
            state.error
        );
    }

    #[test]
    fn turn_finishing_during_disconnect_lights_done_on_reconnect() {
        let mut state = AgentState::new(true);
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
                preserved_thread: None,
                resume_error: None,
            },
            false,
        );
        state.apply(AgentEvent::Sent { thread: "t-bg".into(), turn: 1 }, false);
        state.apply(
            AgentEvent::Message {
                thread: "t-bg".into(),
                turn: 1,
                text: "run".into(),
                context: None,
            },
            false,
        );
        assert_eq!(state.mark, Mark::Working);

        // Lever disconnects while turn is working -> mark clears to Idle while offline.
        state.apply(AgentEvent::Disconnected, false);
        assert_eq!(state.mark, Mark::Idle);

        // Lever reconnects and resumes the thread now finished -> mark lights Done.
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: Some(ResumedThread {
                    thread: "t-bg".into(),
                    title: "bg".into(),
                    cwd: "/tmp".into(),
                    model: None,
                    mode: None,
                    working: false,
                    turns: vec![Turn {
                        turn: 1,
                        message: "run".into(),
                        context: None,
                        acts: Vec::new(),
                        notes: Vec::new(),
                        requests: Vec::new(),
                        mode: None,
                        answer: "finished".into(),
                        state: TurnState::Done,
                    }],
                }),
                preserved_thread: None,
                resume_error: None,
            },
            false,
        );
        assert_eq!(state.mark, Mark::Done);
    }

    #[test]
    fn terminal_turn_never_displays_unfinished_act_as_working() {
        let working_act = Act {
            id: "a1".into(),
            kind: "command".into(),
            title: "sleep 10".into(),
            state: ActState::Working,
            detail: None,
        };
        let pending_act = Act {
            id: "a2".into(),
            kind: "command".into(),
            title: "ls".into(),
            state: ActState::Pending,
            detail: None,
        };
        let stopped_err_act = Act {
            id: "a3".into(),
            kind: "command".into(),
            title: "sleep 45".into(),
            state: ActState::Failed,
            detail: Some("Command interrupted by the user.".into()),
        };
        assert_eq!(
            act_display_status(&TurnState::Working, &working_act),
            ActDisplay { label: "Working…", tone: ActTone::Active }
        );
        assert_eq!(
            act_display_status(&TurnState::Stopped, &working_act),
            ActDisplay { label: "Stopped", tone: ActTone::Muted }
        );
        assert_eq!(
            act_display_status(&TurnState::Stopped, &pending_act),
            ActDisplay { label: "Stopped", tone: ActTone::Muted }
        );
        assert_eq!(
            act_display_status(&TurnState::Stopped, &stopped_err_act),
            ActDisplay { label: "Stopped", tone: ActTone::Muted }
        );
        assert_eq!(
            act_display_status(&TurnState::Interrupted, &working_act),
            ActDisplay { label: "Interrupted", tone: ActTone::Muted }
        );
        assert_eq!(
            act_display_status(&TurnState::Interrupted, &pending_act),
            ActDisplay { label: "Interrupted", tone: ActTone::Muted }
        );
        assert_eq!(
            act_display_status(&TurnState::Failed("err".into()), &working_act),
            ActDisplay { label: "Interrupted", tone: ActTone::Muted }
        );
        assert_eq!(
            act_display_status(&TurnState::Done, &working_act),
            ActDisplay { label: "Done", tone: ActTone::Muted }
        );
    }

    #[test]
    fn preserved_thread_reconnect_unwedges_working_turn_and_keeps_done_memory() {
        let mut state = AgentState::new(true);
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
                preserved_thread: None,
                resume_error: None,
            },
            false,
        );
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, false);
        state.apply(
            AgentEvent::Message {
                thread: "t-1".into(),
                turn: 1,
                text: "slow work".into(),
                context: None,
            },
            false,
        );
        assert!(state.working());
        assert!(!state.can_send());

        // Disconnect while turn 1 is working.
        state.apply(AgentEvent::Disconnected, false);
        assert_eq!(state.mark, Mark::Idle);

        // Reconnect hits transient storage-error on `resume("t-1")`.
        // Preserves `t-1`, unwedges stale Working turn so `can_send()` is true,
        // and retains working memory for a subsequent resume.
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
                preserved_thread: Some("t-1".into()),
                resume_error: Some("Could not load the previous thread right now.".into()),
            },
            false,
        );
        assert_eq!(state.thread.as_deref(), Some("t-1"));
        assert!(!state.working());
        assert!(state.can_send());
        assert_eq!(state.turns[0].state, TurnState::Interrupted);

        // Later reconnect succeeds with turn 1 Done -> mark still lights Done!
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: Some(ResumedThread {
                    thread: "t-1".into(),
                    title: "slow".into(),
                    cwd: "/tmp".into(),
                    model: None,
                    mode: None,
                    working: false,
                    turns: vec![Turn {
                        turn: 1,
                        message: "slow work".into(),
                        context: None,
                        acts: Vec::new(),
                        notes: Vec::new(),
                        requests: Vec::new(),
                        mode: None,
                        answer: "all done".into(),
                        state: TurnState::Done,
                    }],
                }),
                preserved_thread: None,
                resume_error: None,
            },
            false,
        );
        assert_eq!(state.mark, Mark::Done);
        assert_eq!(state.turns[0].state, TurnState::Done);
    }

    #[test]
    fn missing_saved_thread_starts_fresh_without_adopting_other_thread() {
        let dir = short_temp_dir("cs-lever-missing-saved");
        let sock = dir.join("surface");
        let state_path = dir.join("active-thread");
        save_active_thread(Some(&state_path), Some("t-deleted"));
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            // Attempt 1: saved thread is gone (`no-thread`), then socket drops.
            {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
                peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })))
                .unwrap();
                assert_eq!(
                    read_frame(&mut peer).unwrap(),
                    serde_json::json!({ "op": "resume", "thread": "t-deleted" })
                );
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "error",
                    "code": "no-thread",
                    "message": "There is no thread t-deleted."
                })))
                .unwrap();
                std::thread::sleep(Duration::from_millis(80));
            }
            // Attempt 2: after reconnect, client STILL must NOT ask `list` to
            // adopt an unrelated thread before the person sends a message.
            {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
                peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
                assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
                peer.write_all(&encode_frame(&serde_json::json!({
                    "reply": "status",
                    "version": "0.1.0",
                    "harness_started": false,
                    "harnesses": [{ "id": "h1", "name": "OpenCode" }],
                    "free_hosted_models": false
                })))
                .unwrap();
            }
        });

        let (tx, mut rx) = session::channel();
        let _sender =
            start_with_options(sock.clone(), tx.clone(), INIT_DEADLINE, Some(state_path.clone()));
        let mut state = AgentState::new(true);
        let mut connect_count = 0usize;
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            let is_conn = matches!(ev, AgentEvent::Connected { .. });
            state.apply(ev, true);
            if is_conn {
                connect_count += 1;
                assert_eq!(state.thread, None);
            }
            connect_count >= 2
        });

        server.join().unwrap();
        let saved = load_saved_thread(Some(&state_path));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(state.connected);
        assert!(state.can_send());
        assert_eq!(state.thread, None);
        assert_eq!(saved, None);
    }

    fn sample_request(id: &str, state: RequestState) -> Request {
        Request {
            id: id.into(),
            act: Some("act-1".into()),
            kind: "command".into(),
            title: "echo hi".into(),
            detail: "echo hi".into(),
            path: None,
            answers: vec![
                RequestAnswer { answer: "once".into(), name: "Allow once".into() },
                RequestAnswer { answer: "always".into(), name: "Always allow".into() },
                RequestAnswer { answer: "no".into(), name: "Reject".into() },
            ],
            state,
            answer: None,
            sending: None,
        }
    }

    fn connected_state(drawer_up: bool) -> AgentState {
        let mut state = AgentState::new(true);
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: None,
                preserved_thread: None,
                resume_error: None,
            },
            drawer_up,
        );
        state
    }

    fn activity(working: &[&str], waiting: &[&str]) -> AgentEvent {
        AgentEvent::Activity {
            working: working.iter().map(|s| s.to_string()).collect(),
            waiting: waiting.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn parses_requests_modes_and_activity() {
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "request", "thread": "t-1", "turn": 2, "request": "r-1", "act": "act-1",
                "kind": "edit", "title": "Edit notes.txt", "detail": "-old\n+new", "path": "/home/crowbar/notes.txt",
                "answers": [{ "answer": "once", "name": "Allow once" }, { "answer": "no", "name": "Reject" }],
                "state": "waiting"
            })),
            Some(LeverMessage::Request {
                thread: "t-1".into(),
                turn: 2,
                request: Request {
                    id: "r-1".into(),
                    act: Some("act-1".into()),
                    kind: "edit".into(),
                    title: "Edit notes.txt".into(),
                    detail: "-old\n+new".into(),
                    path: Some("/home/crowbar/notes.txt".into()),
                    answers: vec![
                        RequestAnswer { answer: "once".into(), name: "Allow once".into() },
                        RequestAnswer { answer: "no".into(), name: "Reject".into() },
                    ],
                    state: RequestState::Waiting,
                    answer: None,
                    sending: None,
                },
            })
        );
        match parse_lever_message(&serde_json::json!({
            "event": "request", "thread": "t-1", "turn": 2, "request": "r-1", "kind": "command",
            "title": "ls", "detail": "ls", "answers": [], "state": "answered", "answer": "always"
        })) {
            Some(LeverMessage::Request { request, .. }) => {
                assert_eq!(request.state, RequestState::Answered);
                assert_eq!(request.answer.as_deref(), Some("always"));
            }
            other => panic!("expected Request, got {other:?}"),
        }
        match parse_lever_message(&serde_json::json!({
            "event": "request", "thread": "t-1", "turn": 2, "request": "r-1", "kind": "command",
            "title": "ls", "detail": "ls", "answers": [], "state": "withdrawn"
        })) {
            Some(LeverMessage::Request { request, .. }) => {
                assert_eq!(request.state, RequestState::Withdrawn)
            }
            other => panic!("expected Request, got {other:?}"),
        }

        // A resumed thread carries its mode, and each turn its mode and requests.
        match parse_lever_message(&serde_json::json!({
            "reply": "resumed", "thread": "t-1", "title": "", "cwd": "/tmp", "mode": "full-access",
            "working": true,
            "turns": [{
                "turn": 1, "message": "hi", "answer": "", "state": "working", "mode": "ask-first",
                "acts": [], "notes": [],
                "requests": [{ "request": "r-9", "kind": "command", "title": "id", "detail": "id",
                               "answers": [{ "answer": "once", "name": "Allow once" }], "state": "waiting" }]
            }, { "turn": 2, "message": "old", "answer": "", "state": "done", "acts": [], "notes": [] }]
        })) {
            Some(LeverMessage::Resumed(r)) => {
                assert_eq!(r.mode.as_deref(), Some("full-access"));
                assert_eq!(r.turns[0].mode.as_deref(), Some("ask-first"));
                assert_eq!(r.turns[0].requests.len(), 1);
                assert_eq!(r.turns[0].requests[0].id, "r-9");
                assert_eq!(r.turns[1].mode, None);
                assert!(r.turns[1].requests.is_empty());
            }
            other => panic!("expected Resumed, got {other:?}"),
        }

        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "event": "thread-updated", "thread": "t-1", "mode": "plan"
            })),
            Some(LeverMessage::ThreadUpdated { thread: "t-1".into(), title: None, mode: Some("plan".into()) })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({
                "reply": "models", "supported": true, "models": [{ "id": "stub/stub-1", "name": "Stub One" }],
                "default": "stub/stub-1", "free_hosted_models": false,
                "modes": [{ "id": "ask-first", "name": "Ask first" }, { "id": "full-access", "name": "Full access" },
                          { "id": "plan", "name": "Plan" }]
            })),
            Some(LeverMessage::Models {
                modes: vec![
                    ModeInfo { id: "ask-first".into(), name: "Ask first".into() },
                    ModeInfo { id: "full-access".into(), name: "Full access".into() },
                    ModeInfo { id: "plan".into(), name: "Plan".into() },
                ]
            })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "models", "supported": false, "models": [] })),
            Some(LeverMessage::Models { modes: Vec::new() })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "mode-set", "thread": "t-1", "mode": "full-access" })),
            Some(LeverMessage::ModeSet { thread: "t-1".into(), mode: "full-access".into() })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "default-mode", "mode": "full-access" })),
            Some(LeverMessage::DefaultMode { mode: "full-access".into() })
        );
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "reply": "activity", "working": ["t-1"], "waiting": [] })),
            Some(LeverMessage::Activity { working: vec!["t-1".into()], waiting: Vec::new() })
        );
        // The `activity` event names no thread of its own.
        assert_eq!(
            parse_lever_message(&serde_json::json!({ "event": "activity", "working": [], "waiting": ["t-2"] })),
            Some(LeverMessage::Activity { working: Vec::new(), waiting: vec!["t-2".into()] })
        );
    }

    #[test]
    fn a_request_after_its_turn_is_answered_and_stop_withdraws_it() {
        let mut state = connected_state(true);
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "run".into(), context: None },
            true,
        );
        state.apply(AgentEvent::Done { thread: "t-1".into(), turn: 1 }, true);
        assert!(!state.can_stop(), "nothing works and nothing waits");

        // The harness asks after its turn ended, as a subagent's would.
        state.apply(
            AgentEvent::Request {
                thread: "t-1".into(),
                turn: 1,
                request: sample_request("r-1", RequestState::Waiting),
            },
            true,
        );
        assert_eq!(
            request_view(&state.turns[0].state, &state.turns[0].requests[0]),
            RequestView::Waiting
        );
        assert!(state.waiting());
        assert!(state.can_stop(), "Stop withdraws what waits");
        assert_eq!(state.current_turn(), None, "Stop names no turn: none works");
        assert_eq!(state.begin_answer("r-1", "once").as_deref(), Some("t-1"));
    }

    #[test]
    fn a_request_waits_then_is_answered_or_withdrawn() {
        let mut state = connected_state(true);
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "run".into(), context: None },
            true,
        );
        // A request for a thread the drawer does not hold is not drawn here.
        assert!(!state.apply(
            AgentEvent::Request {
                thread: "t-other".into(),
                turn: 1,
                request: sample_request("r-x", RequestState::Waiting),
            },
            true,
        ));
        assert!(state.apply(
            AgentEvent::Request {
                thread: "t-1".into(),
                turn: 1,
                request: sample_request("r-1", RequestState::Waiting),
            },
            true,
        ));
        assert!(state.waiting());
        // Before Lever's `activity` arrives, the drawer's own thread shows waiting.
        assert_eq!(state.mark, Mark::Waiting);
        assert_eq!(request_view(&state.turns[0].state, &state.turns[0].requests[0]), RequestView::Waiting);

        // An answer the request does not offer is not sent.
        assert_eq!(state.begin_answer("r-1", "never"), None);
        // Answering once marks it as sending, and a second click sends nothing.
        assert_eq!(state.begin_answer("r-1", "once").as_deref(), Some("t-1"));
        assert_eq!(
            request_view(&state.turns[0].state, &state.turns[0].requests[0]),
            RequestView::Sending("Allow once".into())
        );
        assert_eq!(state.begin_answer("r-1", "no"), None);
        // Lever did not take it: the person can answer again.
        state.apply(
            AgentEvent::Error { code: "no-request".into(), message: "No such request.".into() },
            true,
        );
        assert_eq!(state.turns[0].requests[0].sending, None);
        assert_eq!(state.error.as_deref(), Some("That request no longer waits for an answer."));
        assert_eq!(state.begin_answer("r-1", "once").as_deref(), Some("t-1"));

        // The same request ID comes again, answered.
        let mut answered = sample_request("r-1", RequestState::Answered);
        answered.answer = Some("once".into());
        state.apply(AgentEvent::Request { thread: "t-1".into(), turn: 1, request: answered }, true);
        assert_eq!(state.turns[0].requests.len(), 1);
        assert_eq!(
            request_view(&state.turns[0].state, &state.turns[0].requests[0]),
            RequestView::Answered("Allow once".into())
        );
        assert!(!state.waiting());
        assert_eq!(state.mark, Mark::Working);
        assert_eq!(state.begin_answer("r-1", "once"), None);

        // A second request is withdrawn, as Stop withdraws a turn's requests.
        state.apply(
            AgentEvent::Request {
                thread: "t-1".into(),
                turn: 1,
                request: sample_request("r-2", RequestState::Waiting),
            },
            true,
        );
        assert_eq!(state.mark, Mark::Waiting);
        state.apply(
            AgentEvent::Request {
                thread: "t-1".into(),
                turn: 1,
                request: sample_request("r-2", RequestState::Withdrawn),
            },
            true,
        );
        assert_eq!(
            request_view(&state.turns[0].state, &state.turns[0].requests[1]),
            RequestView::Withdrawn
        );
        assert_eq!(state.mark, Mark::Working);

        // One Lever says waits, waits, whether or not its turn still works:
        // a harness may ask after its turn. Only one in a turn the drawer saw
        // interrupted shows no answer controls.
        let waiting = sample_request("r-3", RequestState::Waiting);
        assert_eq!(request_view(&TurnState::Interrupted, &waiting), RequestView::Withdrawn);
        assert_eq!(request_view(&TurnState::Working, &waiting), RequestView::Waiting);
        assert_eq!(request_view(&TurnState::Done, &waiting), RequestView::Waiting);
        assert_eq!(request_view(&TurnState::Stopped, &waiting), RequestView::Waiting);

        // An unnamed answer gets a plain name.
        assert_eq!(RequestAnswer { answer: "no".into(), name: " ".into() }.label(), "Refuse");
    }

    #[test]
    fn a_request_comes_back_with_its_thread_after_a_restart() {
        let mut state = AgentState::new(true);
        state.apply(
            AgentEvent::Connected {
                status: sample_status(vec!["OpenCode"]),
                resumed: Some(ResumedThread {
                    thread: "t-1".into(),
                    title: "Waits".into(),
                    cwd: "/tmp".into(),
                    model: None,
                    mode: Some("ask-first".into()),
                    working: true,
                    turns: vec![Turn {
                        turn: 1,
                        message: "run".into(),
                        context: None,
                        acts: Vec::new(),
                        notes: Vec::new(),
                        requests: vec![sample_request("r-1", RequestState::Waiting)],
                        mode: Some("ask-first".into()),
                        answer: String::new(),
                        state: TurnState::Working,
                    }],
                }),
                preserved_thread: None,
                resume_error: None,
            },
            false,
        );
        assert!(state.waiting());
        assert_eq!(state.mark, Mark::Waiting);
        assert_eq!(state.mode.as_deref(), Some("ask-first"));
        // Summoning the drawer leaves it waiting.
        assert!(!state.on_drawer_opened());
        assert_eq!(state.mark, Mark::Waiting);
        assert!(state.mode_locked());
    }

    #[test]
    fn the_mark_follows_activity_waiting_over_working_over_done() {
        let mut state = connected_state(false);
        assert_eq!(state.mark, Mark::Idle);

        // Lever's `watch` reply: nothing works.
        state.apply(activity(&[], &[]), false);
        assert_eq!(state.mark, Mark::Idle);

        // A thread the drawer does not hold works (as one `crowbar-agent` runs):
        // the mark shows it, and the drawer's own thread can still send.
        assert!(state.apply(activity(&["t-term"], &[]), false));
        assert_eq!(state.mark, Mark::Working);
        assert!(state.can_send());

        // It asks: waiting outranks working.
        state.apply(activity(&["t-term"], &["t-term"]), false);
        assert_eq!(state.mark, Mark::Waiting);
        // The drawer summoned and put away leaves it waiting.
        assert!(!state.on_drawer_opened());
        assert_eq!(state.mark, Mark::Waiting);
        assert!(!state.on_drawer_closed());
        assert_eq!(state.mark, Mark::Waiting);

        // Answered, it works again; then it ends behind the put-away drawer.
        state.apply(activity(&["t-term"], &[]), false);
        assert_eq!(state.mark, Mark::Working);
        state.apply(activity(&[], &[]), false);
        assert_eq!(state.mark, Mark::Done);
        // Summoning the drawer acknowledges it.
        assert!(state.on_drawer_opened());
        assert_eq!(state.mark, Mark::Idle);

        // Work that ends with the drawer up leaves the mark idle.
        state.apply(activity(&["t-term"], &[]), true);
        assert_eq!(state.mark, Mark::Working);
        state.apply(activity(&[], &[]), true);
        assert_eq!(state.mark, Mark::Idle);

        // New work clears a done no one has seen yet.
        state.apply(activity(&["t-a"], &[]), false);
        state.apply(activity(&[], &[]), false);
        assert_eq!(state.mark, Mark::Done);
        state.apply(activity(&["t-b"], &[]), false);
        assert_eq!(state.mark, Mark::Working);

        // Once Lever watches, the drawer's own waiting request does not light the
        // mark by itself: Lever's word for every thread does.
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, false);
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "run".into(), context: None },
            false,
        );
        state.apply(activity(&["t-b", "t-1"], &[]), false);
        state.apply(
            AgentEvent::Request {
                thread: "t-1".into(),
                turn: 1,
                request: sample_request("r-1", RequestState::Waiting),
            },
            false,
        );
        assert_eq!(state.mark, Mark::Working);
        state.apply(activity(&["t-b", "t-1"], &["t-1"]), false);
        assert_eq!(state.mark, Mark::Waiting);

        // A lost connection forgets what Lever said until it says it again.
        state.apply(AgentEvent::Disconnected, false);
        assert_eq!(state.mark, Mark::Idle);
        assert!(state.working_threads.is_empty() && state.waiting_threads.is_empty());
    }

    #[test]
    fn the_mark_overlay_shows_waiting_over_a_fullscreen_window() {
        let snap = session::Snapshot {
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
                output: "Virtual-1".into(),
            }],
            ..Default::default()
        };
        assert_eq!(
            fullscreen_mark_overlay_output(Mark::Waiting, &snap, false).as_deref(),
            Some("Virtual-1")
        );
        assert_eq!(fullscreen_mark_overlay_output(Mark::Waiting, &snap, true), None);
        assert_eq!(Mark::Waiting.label(), "waiting");
        assert_eq!(serde_json::to_value(Mark::Waiting).unwrap(), "waiting");
    }

    #[test]
    fn a_thread_keeps_its_mode_and_the_drawer_picks_one() {
        let mut state = connected_state(true);
        assert_eq!(state.default_mode.as_deref(), Some("ask-first"));
        // The drawer's new thread shows the starting mode.
        assert_eq!(state.thread, None);
        assert_eq!(state.shown_mode().as_deref(), Some("ask-first"));

        // Opening the menu asks for the modes once connected.
        assert!(state.open_mode_menu());
        assert!(state.modes_loading);
        state.apply(
            AgentEvent::Modes(vec![
                ModeInfo { id: "ask-first".into(), name: "Ask first".into() },
                ModeInfo { id: "full-access".into(), name: "Full access".into() },
                ModeInfo { id: "plan".into(), name: "Plan".into() },
            ]),
            true,
        );
        assert!(!state.modes_loading);
        assert_eq!(state.modes.len(), 3);

        // Picked for the new thread, it is held and goes with the first message.
        assert_eq!(state.pick_mode("plan"), ModePick::Pending);
        assert_eq!(state.shown_mode().as_deref(), Some("plan"));
        state.apply(AgentEvent::Sent { thread: "t-1".into(), turn: 1 }, true);
        assert_eq!(state.mode.as_deref(), Some("plan"));
        assert_eq!(state.pending_mode, None);
        state.apply(
            AgentEvent::Message { thread: "t-1".into(), turn: 1, text: "plan it".into(), context: None },
            true,
        );
        // While the thread works, its mode does not change.
        assert!(state.mode_locked());
        assert_eq!(state.pick_mode("full-access"), ModePick::Refused);
        state.apply(AgentEvent::Done { thread: "t-1".into(), turn: 1 }, true);
        assert_eq!(
            state.pick_mode("full-access"),
            ModePick::SetThread { thread: "t-1".into(), mode: "full-access".into() }
        );
        // Lever's reply, and its `thread-updated`, set the thread's mode.
        state.apply(AgentEvent::ModeSet { thread: "t-1".into(), mode: "full-access".into() }, true);
        assert_eq!(state.mode.as_deref(), Some("full-access"));
        state.apply(
            AgentEvent::ThreadUpdated { thread: "t-1".into(), title: Some("Plan".into()), mode: None },
            true,
        );
        assert_eq!(state.mode.as_deref(), Some("full-access"));
        assert_eq!(state.title.as_deref(), Some("Plan"));
        state.apply(
            AgentEvent::ThreadUpdated { thread: "t-1".into(), title: None, mode: Some("plan".into()) },
            true,
        );
        assert_eq!(state.mode.as_deref(), Some("plan"));
        assert_eq!(state.title.as_deref(), Some("Plan"));
        // Another thread's mode is not this one's.
        state.apply(AgentEvent::ModeSet { thread: "t-2".into(), mode: "ask-first".into() }, true);
        assert_eq!(state.mode.as_deref(), Some("plan"));

        // The mode new threads start in.
        state.apply(AgentEvent::DefaultMode("full-access".into()), true);
        assert_eq!(state.default_mode.as_deref(), Some("full-access"));
        // A status without one keeps it.
        let mut status = sample_status(vec!["OpenCode"]);
        status.default_mode = None;
        state.apply(AgentEvent::Status(status), true);
        assert_eq!(state.default_mode.as_deref(), Some("full-access"));

        // Resuming another thread takes its mode.
        state.apply(
            AgentEvent::Resumed(ResumedThread {
                thread: "t-2".into(),
                title: String::new(),
                cwd: "/tmp".into(),
                model: None,
                mode: Some("ask-first".into()),
                working: false,
                turns: Vec::new(),
            }),
            true,
        );
        assert_eq!(state.mode.as_deref(), Some("ask-first"));

        // A new thread with no mode picked starts in the starting mode.
        state.apply(AgentEvent::Error { code: "no-thread".into(), message: String::new() }, true);
        assert_eq!(state.mode, None);
        assert_eq!(state.shown_mode().as_deref(), Some("full-access"));
        state.apply(AgentEvent::Sent { thread: "t-3".into(), turn: 1 }, true);
        assert_eq!(state.mode.as_deref(), Some("full-access"));

        // `models` failing while the menu loads says so in the menu alone.
        assert!(state.open_mode_menu());
        state.apply(AgentEvent::Error { code: "harness-error".into(), message: "boom".into() }, true);
        assert!(!state.modes_loading);
        assert_eq!(state.modes_error.as_deref(), Some("Modes are not available right now."));
        assert_eq!(state.error, None);

        assert_eq!(mode_name("full-access", &[]), "Full access");
        assert_eq!(mode_name("plan", &state.modes), "Plan");
    }

    #[test]
    fn request_detail_shows_a_command_as_it_is_and_an_edit_as_a_diff() {
        assert_eq!(
            request_detail_lines("command", "rm -rf build\n+x"),
            vec![
                (DetailLine::Plain, "rm -rf build".to_string()),
                (DetailLine::Plain, "+x".to_string()),
            ]
        );
        assert_eq!(
            request_detail_lines("edit", " keep\n-old line\n+new line\n"),
            vec![
                (DetailLine::Plain, " keep".to_string()),
                (DetailLine::Removed, "-old line".to_string()),
                (DetailLine::Added, "+new line".to_string()),
            ]
        );
        // A change Lever cut, as long as Lever makes one, shows whole: what
        // it takes out and what it puts in.
        let mut cut: Vec<String> = (0..200).map(|i| format!("-old {i}")).collect();
        cut.push("… 250 more lines taken out".into());
        cut.extend((0..200).map(|i| format!("+new {i}")));
        cut.push("… 250 more lines put in".into());
        let lines = request_detail_lines("edit", &cut.join("\n"));
        assert_eq!(lines.len(), 402);
        assert_eq!(lines[201], (DetailLine::Added, "+new 0".to_string()));
        assert_eq!(lines[401], (DetailLine::Plain, "… 250 more lines put in".to_string()));
        // Longer detail is cut, saying how much.
        let long: String = (0..450).map(|i| format!("{i}\n")).collect();
        let lines = request_detail_lines("command", &long);
        assert_eq!(lines.len(), 403);
        assert_eq!(lines[402], (DetailLine::Plain, "… 48 more lines".to_string()));
    }

    #[test]
    fn surface_client_watches_answers_and_sets_modes() {
        let dir = short_temp_dir("cs-lever-requests");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();

        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status", "version": "0.1.0", "harness_started": false,
                "harnesses": [{ "id": "h1", "name": "OpenCode" }], "free_hosted_models": false,
                "default_mode": "ask-first"
            })))
            .unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "list");
            peer.write_all(&encode_frame(&serde_json::json!({ "reply": "list", "threads": [] })))
                .unwrap();
            // The shell asks to watch at connect, and never for `models`.
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "op": "watch" }));
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "activity", "working": [], "waiting": []
            })))
            .unwrap();

            // The person opens the mode menu.
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "op": "models" }));
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "models", "supported": true, "models": [], "default": null,
                "free_hosted_models": false,
                "modes": [{ "id": "ask-first", "name": "Ask first" }, { "id": "full-access", "name": "Full access" }]
            })))
            .unwrap();
            // A new thread's first message carries the mode picked for it.
            assert_eq!(
                read_frame(&mut peer).unwrap(),
                serde_json::json!({ "op": "send", "thread": null, "text": "run it", "mode": "full-access" })
            );
            peer.write_all(&encode_frame(&serde_json::json!({ "reply": "sent", "thread": "t-1", "turn": 1 })))
                .unwrap();
            for frame in [
                serde_json::json!({ "event": "activity", "working": ["t-1"], "waiting": [] }),
                serde_json::json!({ "event": "message", "thread": "t-1", "turn": 1, "text": "run it" }),
                serde_json::json!({
                    "event": "request", "thread": "t-1", "turn": 1, "request": "r-1", "kind": "command",
                    "title": "echo hi", "detail": "echo hi",
                    "answers": [{ "answer": "once", "name": "Allow once" }, { "answer": "no", "name": "Reject" }],
                    "state": "waiting"
                }),
                serde_json::json!({ "event": "activity", "working": ["t-1"], "waiting": ["t-1"] }),
            ] {
                peer.write_all(&encode_frame(&frame)).unwrap();
            }
            assert_eq!(
                read_frame(&mut peer).unwrap(),
                serde_json::json!({ "op": "answer", "thread": "t-1", "request": "r-1", "answer": "once" })
            );
            peer.write_all(&encode_frame(&serde_json::json!({ "reply": "ok" }))).unwrap();
            for frame in [
                serde_json::json!({
                    "event": "request", "thread": "t-1", "turn": 1, "request": "r-1", "kind": "command",
                    "title": "echo hi", "detail": "echo hi",
                    "answers": [{ "answer": "once", "name": "Allow once" }, { "answer": "no", "name": "Reject" }],
                    "state": "answered", "answer": "once"
                }),
                serde_json::json!({ "event": "activity", "working": ["t-1"], "waiting": [] }),
                serde_json::json!({ "event": "done", "thread": "t-1", "turn": 1 }),
                serde_json::json!({ "event": "activity", "working": [], "waiting": [] }),
            ] {
                peer.write_all(&encode_frame(&frame)).unwrap();
            }
            assert_eq!(
                read_frame(&mut peer).unwrap(),
                serde_json::json!({ "op": "set-mode", "thread": "t-1", "mode": "ask-first" })
            );
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "mode-set", "thread": "t-1", "mode": "ask-first"
            })))
            .unwrap();
            assert_eq!(
                read_frame(&mut peer).unwrap(),
                serde_json::json!({ "op": "set-default-mode", "mode": "full-access" })
            );
            peer.write_all(&encode_frame(&serde_json::json!({ "reply": "default-mode", "mode": "full-access" })))
                .unwrap();
        });

        let (tx, mut rx) = session::channel();
        let sender = start(sock.clone(), tx.clone());
        let mut state = AgentState::new(true);
        let mut step = 0;
        let mut saw_waiting = false;
        run_with_deadline(&mut rx, &tx, Duration::from_secs(10), |ev| {
            // The drawer is put away throughout.
            state.apply(ev, false);
            if step == 0 && state.connected && state.watching {
                step = 1;
                assert!(state.open_mode_menu());
                sender.fetch_modes();
            } else if step == 1 && !state.modes.is_empty() {
                step = 2;
                assert_eq!(state.pick_mode("full-access"), ModePick::Pending);
                state.sending = true;
                sender.send_message(None, "run it".into(), state.pending_mode.clone());
            } else if step == 2 && state.mark == Mark::Waiting {
                step = 3;
                saw_waiting = true;
                let thread = state.begin_answer("r-1", "once").expect("request waits");
                sender.answer(thread, "r-1".into(), "once".into());
            } else if step == 3 && state.mark == Mark::Done {
                step = 4;
                match state.pick_mode("ask-first") {
                    ModePick::SetThread { thread, mode } => sender.set_mode(thread, mode),
                    other => panic!("expected SetThread, got {other:?}"),
                }
            } else if step == 4 && state.mode.as_deref() == Some("ask-first") {
                step = 5;
                sender.set_default_mode("full-access".into());
            }
            step == 5 && state.default_mode.as_deref() == Some("full-access")
        });

        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(saw_waiting);
        assert_eq!(state.thread.as_deref(), Some("t-1"));
        assert_eq!(state.turns[0].requests[0].state, RequestState::Answered);
        assert_eq!(state.turns[0].requests[0].answer.as_deref(), Some("once"));
        assert_eq!(state.mark, Mark::Done);
    }

    #[test]
    fn a_fresh_shell_restores_the_thread_that_waits_for_the_person() {
        let dir = short_temp_dir("cs-lever-waiting-first");
        let sock = dir.join("surface");
        let listener = UnixListener::bind(&sock).unwrap();
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "version": 1 }));
            peer.write_all(&encode_frame(&serde_json::json!({ "version": 1 }))).unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "status");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "status", "version": "0.1.0", "harness_started": true,
                "harnesses": [{ "id": "h1", "name": "OpenCode" }], "free_hosted_models": false
            })))
            .unwrap();
            assert_eq!(read_frame(&mut peer).unwrap()["op"], "list");
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "list",
                "threads": [
                    { "thread": "t-working", "title": "", "cwd": "/tmp", "started": 3, "updated": 4,
                      "turns": 1, "state": "working", "working": true, "waiting": false, "mode": "full-access" },
                    { "thread": "t-waiting", "title": "", "cwd": "/tmp", "started": 1, "updated": 2,
                      "turns": 1, "state": "working", "working": true, "waiting": true, "mode": "ask-first" }
                ]
            })))
            .unwrap();
            assert_eq!(
                read_frame(&mut peer).unwrap(),
                serde_json::json!({ "op": "resume", "thread": "t-waiting" })
            );
            peer.write_all(&encode_frame(&serde_json::json!({
                "reply": "resumed", "thread": "t-waiting", "title": "", "cwd": "/tmp", "mode": "ask-first",
                "working": true, "turns": []
            })))
            .unwrap();
            assert_eq!(read_frame(&mut peer).unwrap(), serde_json::json!({ "op": "watch" }));
        });
        let (tx, mut rx) = session::channel();
        let _sender = start(sock.clone(), tx.clone());
        let mut state = AgentState::new(true);
        run_with_deadline(&mut rx, &tx, Duration::from_secs(5), |ev| {
            state.apply(ev, false);
            state.connected
        });
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(state.thread.as_deref(), Some("t-waiting"));
        assert_eq!(state.mode.as_deref(), Some("ask-first"));
    }
    fn summary(id: &str, cwd: &str, working: bool) -> ThreadSummary {
        ThreadSummary { archived: false, thread: id.into(), title: id.into(), cwd: cwd.into(), model: None,
            started: 1, updated: 2, turns: 1, state: if working { "working" } else { "done" }.into(), working }
    }

    #[test]
    fn all_threads_drive_the_mark_but_only_the_selected_thread_blocks_the_composer() {
        let mut s = AgentState::new(true);
        s.connected = true;
        s.apply(AgentEvent::Threads { threads: vec![summary("terminal", "/tmp", true)], finished: false }, false);
        assert_eq!(s.mark, Mark::Working);
        assert!(s.can_send(), "terminal work must not prevent a second thread");
        assert!(s.select_thread(None));
        s.apply(AgentEvent::Message { thread: "old-subscription".into(), turn: 1, text: "late".into(), context: None }, true);
        assert!(s.thread.is_none());
        assert!(s.turns.is_empty());
        s.apply(AgentEvent::Threads { threads: vec![summary("terminal", "/tmp", false)], finished: true }, false);
        assert_eq!(s.mark, Mark::Done);
        s.on_drawer_opened();
        assert_eq!(s.mark, Mark::Idle);
        s.apply(AgentEvent::Threads { threads: vec![summary("a", "/tmp", false), summary("b", "/tmp", true)], finished: true }, false);
        assert_eq!(s.mark, Mark::Working, "one completion must not hide another working thread");
    }

    #[test]
    fn list_groups_newest_folders_and_threads_and_hides_archived_work_without_losing_the_mark() {
        let mut s = AgentState::new(true);
        s.connected = true;
        let mut archived = summary("hidden", "/archive", true); archived.archived = true;
        s.apply(AgentEvent::Threads { threads: vec![summary("new", "/b", false), summary("middle", "/a", false), summary("old", "/b", false), archived], finished: false }, true);
        let groups = s.groups();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].0, "/b");
        assert_eq!(groups[0].1.iter().map(|t| t.thread.as_str()).collect::<Vec<_>>(), vec!["new", "old"]);
        assert_eq!(s.mark, Mark::Working);
        s.on_drawer_closed();
        assert!(s.list_view);
        s.select_thread(Some("new".into()));
        assert!(!s.select_thread(Some("old".into())), "a pending history read must not land in another thread");
        s.on_drawer_closed(); s.on_drawer_opened();
        assert!(!s.list_view);
    }

    #[test]
    fn a_background_archive_does_not_unlock_a_pending_history_read() {
        let mut s = AgentState::new(true);
        s.connected = true;
        s.select_thread(Some("selected".into()));
        s.apply(AgentEvent::Lifecycle { thread: "background".into(), deleted: false, warning: None }, true);
        assert!(s.loading);
        assert!(!s.select_thread(Some("another".into())));
        assert_eq!(s.thread.as_deref(), Some("selected"));
    }

}
