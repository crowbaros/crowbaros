// SPDX-License-Identifier: MIT
//! Domain types, wire message representations, and pure formatting helpers
//! for Lever's surface socket (ADR 0036).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::compositor;

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
pub enum SubagentState {
    Working,
    Done,
    Stopped,
    Interrupted,
    Failed(String),
}

impl SubagentState {
    pub fn is_working(&self) -> bool {
        matches!(self, SubagentState::Working)
    }

    pub fn label(&self) -> &'static str {
        match self {
            SubagentState::Working => "Working…",
            SubagentState::Done => "Done",
            SubagentState::Stopped => "Stopped",
            SubagentState::Interrupted => "Interrupted",
            SubagentState::Failed(_) => "Failed",
        }
    }

    pub fn as_turn_state(&self) -> TurnState {
        match self {
            SubagentState::Working => TurnState::Working,
            SubagentState::Done => TurnState::Done,
            SubagentState::Stopped => TurnState::Stopped,
            SubagentState::Interrupted => TurnState::Interrupted,
            SubagentState::Failed(msg) => TurnState::Failed(msg.clone()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentMessage {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subagent {
    #[serde(rename = "subagent")]
    pub id: String,
    pub title: String,
    pub state: SubagentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<i64>,
    #[serde(default)]
    pub messages: Vec<SubagentMessage>,
    #[serde(default)]
    pub acts: Vec<Act>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent: Option<String>,
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

/// Splits a request's detail into lines for the drawer.
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
    if let Some(mode) = modes
        .iter()
        .find(|m| m.id == id && !m.name.trim().is_empty())
    {
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

/// A Model the harness offers, with the provider it has it from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    /// The Model's name, without its provider.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
}

impl ModelInfo {
    /// The name the drawer shows: the Model's, or its ID when it has none.
    pub fn label(&self) -> &str {
        if self.name.trim().is_empty() {
            &self.id
        } else {
            &self.name
        }
    }
}

/// One effort level of a Model.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortInfo {
    pub id: String,
    pub name: String,
}

/// A command the harness offers, sent as a message that begins with `/` and
/// its name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// What Lever says the harness offers, in its `models` reply or event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offer {
    pub supported: bool,
    pub models: Vec<ModelInfo>,
    /// The Model a thread works with when none was chosen for it.
    pub default_model: Option<String>,
    pub free_hosted_models: bool,
    pub modes: Vec<ModeInfo>,
    /// The harness can take a message into a turn under way.
    pub steering: bool,
}

/// A provider in the Model picker's rail: its ID, and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provider {
    pub id: String,
    pub name: String,
}

/// The providers the Models come from, in the order the harness lists them.
/// Models with no provider go under one with no ID, named `Other`.
pub fn providers(models: &[ModelInfo]) -> Vec<Provider> {
    let mut out: Vec<Provider> = Vec::new();
    for model in models {
        let id = model.provider.clone().unwrap_or_default();
        if out.iter().any(|p| p.id == id) {
            continue;
        }
        let name = match (&model.provider_name, id.is_empty()) {
            (Some(name), _) if !name.trim().is_empty() => name.clone(),
            (_, true) => "Other".to_string(),
            (_, false) => id.clone(),
        };
        out.push(Provider { id, name });
    }
    out
}

/// How long a turn worked, as the thread shows it: `8s`, `1m 05s`, `1h 02m`.
pub fn format_elapsed(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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
    #[serde(default)]
    pub requests: Vec<Request>,
    #[serde(default)]
    pub subagents: Vec<Subagent>,
    /// The mode the turn began in, when Lever recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub answer: String,
    pub state: TurnState,
    /// When the turn began and ended, in seconds: Lever's for a turn read
    /// from its record, and the shell's clock for one it watched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<i64>,
    /// The harness began this turn itself, with no message from the person,
    /// as OpenCode does when a subagent reports back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_harness: bool,
    /// Messages the person sent while the turn worked, which the harness
    /// took into it, in the order it took them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steered: Vec<Steer>,
    /// Messages that wait behind the turn, and those a Stop dropped, in the
    /// order sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queued: Vec<Queued>,
}

/// A message that waits behind a turn, to go as its own turn once the turns
/// before it end, or one a Stop dropped.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Queued {
    /// Lever's ID for it.
    pub id: String,
    pub text: String,
    /// `queued`, `dropped`, or, in an event alone, `sent` or `removed`.
    pub state: String,
}

/// A message the harness took into a turn under way.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Steer {
    pub text: String,
    /// How many bytes of the turn's answer had come when the harness took
    /// it: where in the answer it shows.
    pub at: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<DesktopContext>,
}

/// A piece of a turn's answer as it shows: the answer's own words, or a
/// message the harness took into the turn there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnswerPart<'a> {
    Text(&'a str),
    Steered(&'a Steer),
}

impl Turn {
    /// The turn's answer, with each message the harness took into the turn
    /// at the place in the answer it took it. Words of no length are left
    /// out.
    pub fn answer_parts(&self) -> Vec<AnswerPart<'_>> {
        let mut parts = Vec::new();
        let mut from = 0;
        for steer in &self.steered {
            // Past what the record kept of the answer, it shows at its end.
            let at = steer.at.min(self.answer.len());
            let at = if at >= from && self.answer.is_char_boundary(at) {
                at
            } else {
                from
            };
            if at > from {
                parts.push(AnswerPart::Text(&self.answer[from..at]));
            }
            parts.push(AnswerPart::Steered(steer));
            from = at;
        }
        if from < self.answer.len() {
            parts.push(AnswerPart::Text(&self.answer[from..]));
        }
        parts
    }

    /// How long the turn has worked, or worked: to `now` while it works.
    pub fn elapsed(&self, now: i64) -> Option<i64> {
        let started = self.started?;
        let end = match self.state {
            TurnState::Working => now,
            _ => self.ended?,
        };
        Some((end - started).max(0))
    }
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
    /// Whether the person has chosen a Model source (free hosted Models or a
    /// signed-in/configured provider).
    #[serde(default = "default_true")]
    pub models_chosen: bool,
    /// The mode new threads start in, when Lever says.
    pub default_mode: Option<String>,
}

fn default_true() -> bool {
    true
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
    #[serde(default)]
    pub subagents: u64,
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
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub working: bool,
    pub turns: Vec<Turn>,
}

/// One subdirectory entry returned by Lever's `list-dirs` operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
}

/// A bounded directory listing returned by Lever's `list-dirs` operation for
/// the drawer's inline folder picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirListing {
    pub path: String,
    pub home: String,
    pub parent: Option<String>,
    pub entries: Vec<DirEntry>,
}

/// Every reply or event Lever sends on its surface socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeverMessage {
    Threads {
        threads: Vec<ThreadSummary>,
        finished: bool,
    },
    Lifecycle {
        thread: String,
        deleted: bool,
        warning: Option<String>,
    },
    Status(StatusInfo),
    List {
        threads: Vec<ThreadSummary>,
    },
    Resumed(ResumedThread),
    Sent {
        thread: String,
        turn: u64,
    },
    SteerTaken {
        thread: String,
        turn: u64,
    },
    Steered {
        thread: String,
        turn: u64,
        steer: Steer,
    },
    QueueTaken {
        thread: String,
        turn: u64,
    },
    Queued {
        thread: String,
        turn: u64,
        queued: Queued,
    },
    Ok,
    Error {
        code: String,
        message: String,
    },
    Message {
        thread: String,
        turn: u64,
        text: String,
        context: Option<DesktopContext>,
        from_harness: bool,
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
    Subagent {
        thread: String,
        turn: u64,
        subagent: Subagent,
    },
    SubagentText {
        thread: String,
        turn: u64,
        subagent: String,
        role: String,
        text: String,
    },
    SubagentAct {
        thread: String,
        turn: u64,
        subagent: String,
        act: Act,
    },
    ThreadUpdated {
        thread: String,
        title: Option<String>,
        cwd: Option<String>,
        mode: Option<String>,
        model: Option<String>,
        effort: Option<Option<String>>,
    },
    ProjectMade {
        thread: String,
        cwd: String,
        created: bool,
    },
    Dirs(DirListing),
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
    Models(Offer),
    FreeModels {
        enabled: bool,
        models_chosen: Option<bool>,
    },
    Efforts {
        model: String,
        efforts: Vec<EffortInfo>,
    },
    Commands {
        thread: Option<String>,
        commands: Vec<CommandInfo>,
        known: bool,
    },
    ModelSet {
        thread: String,
        model: String,
    },
    EffortSet {
        thread: String,
        effort: Option<String>,
    },
    ModeSet {
        thread: String,
        mode: String,
    },
    DefaultMode {
        mode: String,
    },
    Activity {
        working: Vec<String>,
        waiting: Vec<String>,
    },
}

/// Events sent from the surface-socket client to the shell's UI loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEvent {
    Threads {
        threads: Vec<ThreadSummary>,
        finished: bool,
    },
    Lifecycle {
        thread: String,
        deleted: bool,
        warning: Option<String>,
    },
    Connected {
        status: StatusInfo,
        resumed: Option<ResumedThread>,
        preserved_thread: Option<String>,
        resume_error: Option<String>,
    },
    Disconnected,
    Status(StatusInfo),
    Resumed(ResumedThread),
    Recovered(ResumedThread),
    Sent {
        thread: String,
        turn: u64,
    },
    SteerTaken {
        thread: String,
        turn: u64,
    },
    Steered {
        thread: String,
        turn: u64,
        steer: Steer,
    },
    QueueTaken {
        thread: String,
        turn: u64,
    },
    Queued {
        thread: String,
        turn: u64,
        queued: Queued,
    },
    Message {
        thread: String,
        turn: u64,
        text: String,
        context: Option<DesktopContext>,
        from_harness: bool,
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
    Subagent {
        thread: String,
        turn: u64,
        subagent: Subagent,
    },
    SubagentText {
        thread: String,
        turn: u64,
        subagent: String,
        role: String,
        text: String,
    },
    SubagentAct {
        thread: String,
        turn: u64,
        subagent: String,
        act: Act,
    },
    ThreadUpdated {
        thread: String,
        title: Option<String>,
        cwd: Option<String>,
        mode: Option<String>,
        model: Option<String>,
        effort: Option<Option<String>>,
    },
    ProjectMade {
        thread: String,
        cwd: String,
        created: bool,
    },
    Dirs(DirListing),
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
    Offer(Offer),
    FreeModels {
        enabled: bool,
        models_chosen: Option<bool>,
    },
    Efforts {
        model: String,
        efforts: Vec<EffortInfo>,
    },
    Commands {
        thread: Option<String>,
        commands: Vec<CommandInfo>,
        known: bool,
    },
    ModelSet {
        thread: String,
        model: String,
    },
    EffortSet {
        thread: String,
        effort: Option<String>,
    },
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
    pub(super) fn into_agent_event(self) -> Option<AgentEvent> {
        match self {
            LeverMessage::Threads { threads, finished } => {
                Some(AgentEvent::Threads { threads, finished })
            }
            LeverMessage::List { threads } => Some(AgentEvent::Threads {
                threads,
                finished: false,
            }),
            LeverMessage::Lifecycle {
                thread,
                deleted,
                warning,
            } => Some(AgentEvent::Lifecycle {
                thread,
                deleted,
                warning,
            }),
            LeverMessage::Status(s) => Some(AgentEvent::Status(s)),
            LeverMessage::Resumed(r) => Some(AgentEvent::Resumed(r)),
            LeverMessage::Sent { thread, turn } => Some(AgentEvent::Sent { thread, turn }),
            LeverMessage::SteerTaken { thread, turn } => {
                Some(AgentEvent::SteerTaken { thread, turn })
            }
            LeverMessage::Steered {
                thread,
                turn,
                steer,
            } => Some(AgentEvent::Steered {
                thread,
                turn,
                steer,
            }),
            LeverMessage::QueueTaken { thread, turn } => {
                Some(AgentEvent::QueueTaken { thread, turn })
            }
            LeverMessage::Queued {
                thread,
                turn,
                queued,
            } => Some(AgentEvent::Queued {
                thread,
                turn,
                queued,
            }),
            LeverMessage::Message {
                thread,
                turn,
                text,
                context,
                from_harness,
            } => Some(AgentEvent::Message {
                thread,
                turn,
                text,
                context,
                from_harness,
            }),
            LeverMessage::Text { thread, turn, text } => {
                Some(AgentEvent::Text { thread, turn, text })
            }
            LeverMessage::Act { thread, turn, act } => Some(AgentEvent::Act { thread, turn, act }),
            LeverMessage::Note { thread, turn, note } => {
                Some(AgentEvent::Note { thread, turn, note })
            }
            LeverMessage::Request {
                thread,
                turn,
                request,
            } => Some(AgentEvent::Request {
                thread,
                turn,
                request,
            }),
            LeverMessage::Subagent {
                thread,
                turn,
                subagent,
            } => Some(AgentEvent::Subagent {
                thread,
                turn,
                subagent,
            }),
            LeverMessage::SubagentText {
                thread,
                turn,
                subagent,
                role,
                text,
            } => Some(AgentEvent::SubagentText {
                thread,
                turn,
                subagent,
                role,
                text,
            }),
            LeverMessage::SubagentAct {
                thread,
                turn,
                subagent,
                act,
            } => Some(AgentEvent::SubagentAct {
                thread,
                turn,
                subagent,
                act,
            }),
            LeverMessage::ThreadUpdated {
                thread,
                title,
                cwd,
                mode,
                model,
                effort,
            } => Some(AgentEvent::ThreadUpdated {
                thread,
                title,
                cwd,
                mode,
                model,
                effort,
            }),
            LeverMessage::ProjectMade {
                thread,
                cwd,
                created,
            } => Some(AgentEvent::ProjectMade {
                thread,
                cwd,
                created,
            }),
            LeverMessage::Dirs(listing) => Some(AgentEvent::Dirs(listing)),
            LeverMessage::Models(offer) => Some(AgentEvent::Offer(offer)),
            LeverMessage::FreeModels {
                enabled,
                models_chosen,
            } => Some(AgentEvent::FreeModels {
                enabled,
                models_chosen,
            }),
            LeverMessage::Efforts { model, efforts } => {
                Some(AgentEvent::Efforts { model, efforts })
            }
            LeverMessage::Commands {
                thread,
                commands,
                known,
            } => Some(AgentEvent::Commands {
                thread,
                commands,
                known,
            }),
            LeverMessage::ModelSet { thread, model } => {
                Some(AgentEvent::ModelSet { thread, model })
            }
            LeverMessage::EffortSet { thread, effort } => {
                Some(AgentEvent::EffortSet { thread, effort })
            }
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

/// Commands sent from the shell's UI/controller to the surface-socket adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentCommand {
    Select {
        thread: Option<String>,
    },
    Lifecycle {
        thread: String,
        delete: bool,
    },
    MakeProject {
        thread: String,
        name: Option<String>,
        place: Option<String>,
        path: Option<String>,
    },
    ListDirs {
        path: Option<String>,
    },
    Send {
        thread: Option<String>,
        text: String,
        mode: Option<String>,
        model: Option<String>,
        effort: Option<String>,
    },
    SetModel {
        thread: String,
        model: String,
    },
    SetEffort {
        thread: String,
        effort: Option<String>,
    },
    FetchEfforts {
        model: String,
    },
    FetchCommands {
        thread: Option<String>,
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
    Unqueue {
        thread: String,
        queued: String,
    },
    SetMode {
        thread: String,
        mode: String,
    },
    SetDefaultMode {
        mode: String,
    },
    SetFreeModels {
        enabled: bool,
    },
    FetchModes {
        refresh: bool,
    },
    RefreshStatus,
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
    /// The list open above the message box, if one is.
    pub list: Option<ListSnapshot>,
    /// What the message box holds.
    pub draft: String,
    /// The drawer's controls in view as it last drew.
    pub controls: Vec<ControlBounds>,
    pub state: super::state::AgentState,
}

/// The list open above the drawer's message box, as the control socket
/// says it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ListSnapshot {
    /// `commands`, `models`, `efforts`, `modes`, or `defaultmodes`.
    pub kind: String,
    pub items: Vec<String>,
    pub highlighted: Option<String>,
    /// The provider whose Models a list of Models shows.
    pub provider: Option<String>,
    /// What the list says in place of rows.
    pub note: Option<String>,
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
        || lower.contains("unable to connect")
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
/// appear, or `None` when it should be hidden.
pub fn fullscreen_mark_overlay_output(
    mark: Mark,
    snapshot: &compositor::Snapshot,
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
                    format!(
                        "{} {}x{} at {pos} @{:.2}x",
                        d.name, d.width, d.height, scale
                    )
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

/// Truncates long act detail output to a bounded number of lines and characters.
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
