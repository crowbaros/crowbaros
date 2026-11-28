//! Protocol-agnostic Harness port and domain types (ADR 0036).
//!
//! Every harness adapter (ACP today, Codex and Claude Code next, and in-memory
//! test fakes) implements [`Harness`] behind this contract, and is constructed
//! on demand via [`HarnessFactory`]. No harness protocol's wire types, method
//! names, or internal IDs cross this boundary into Lever's service or store.

use crate::mcp::McpServerSpec;
use std::os::fd::RawFd;
use std::path::PathBuf;

/// The mode a thread starts in when the person has chosen none.
pub const STARTING_MODE: &str = "ask-first";

/// Lever's canonical modes in display order, each with the person's name for it.
const CANONICAL_MODES: [(&str, &str); 3] = [
    ("ask-first", "Ask first"),
    ("full-access", "Full access"),
    ("plan", "Plan"),
];

/// The person's name for one of Lever's modes.
pub fn mode_name(lever_id: &str) -> Option<&'static str> {
    CANONICAL_MODES
        .iter()
        .find(|(id, _)| *id == lever_id)
        .map(|(_, name)| *name)
}

/// Whether `id` is one of Lever's modes.
pub fn is_mode(id: &str) -> bool {
    mode_name(id).is_some()
}

/// Configuration for a harness Lever can start on demand.
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    pub id: String,
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Additional on-disk store paths to watch for sessions started outside
    /// Lever (used by tests or harnesses without built-in default paths).
    pub watch_paths: Vec<PathBuf>,
}

/// Per-launch options passed when spawning a harness instance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HarnessLaunchOptions {
    /// Whether free hosted Models (when the harness offers them) are enabled.
    pub free_hosted_models: bool,
}

/// Structured declaration of what a running harness adapter supports.
///
/// Adapters declare these honestly after initialization so Lever offers the
/// person what the harness can do and no more (ADR 0036).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HarnessCapabilities {
    /// Whether the harness offers mode switching.
    pub supports_modes: bool,
    /// Whether the harness can steer a message into an active turn.
    pub supports_steering: bool,
    /// Whether the running harness can list sessions live.
    pub supports_list_sessions: bool,
    /// Whether the harness can load/resume an existing native session.
    pub supports_load_session: bool,
    /// Whether the harness can delete a native session from its store.
    pub supports_delete_session: bool,
    /// Whether sending a parent prompt while a subagent has a waiting request
    /// strands that request, requiring Lever to hold prompts until answered.
    pub holds_prompt_during_subagent_request: bool,
}

/// One Model option offered by a harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOption {
    pub id: String,
    /// The Model's name, without its provider.
    pub name: String,
    /// The provider the harness has this Model from: its ID and its name.
    pub provider: Option<(String, String)>,
}

/// One effort level a harness offers for a session's Model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortOption {
    pub id: String,
    pub name: String,
}

/// A session's effort selector: the levels of the session's Model and the one
/// it is currently at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortSelector {
    pub config_id: String,
    pub options: Vec<EffortOption>,
    pub current: String,
}

impl EffortSelector {
    pub fn offers(&self, effort: &str) -> bool {
        self.options.iter().any(|o| o.id == effort)
    }
}

/// One slash command a harness offers the person, to send as a message that
/// begins with `/` and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessCommand {
    pub name: String,
    pub description: String,
    /// What the harness says to type after the name, when it says.
    pub hint: Option<String>,
}

/// The Models and modes a harness offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalog {
    /// Whether this harness exposes Model selection (or reported that no
    /// Models are currently available).
    pub supported: bool,
    pub models: Vec<ModelOption>,
    pub default_model: Option<String>,
    pub config_id: Option<String>,
    /// Lever's modes the harness offers, in Lever's order.
    pub modes: Vec<String>,
}

/// How a prompt turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Done,
    Stopped,
    Failed(String),
}

/// One session reported by a harness's own store or listing command, before
/// Lever maps it to its own `t-...` thread ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalSession {
    pub harness_session_id: String,
    pub cwd: String,
    pub title: Option<String>,
    pub created: Option<i64>,
    pub updated: Option<i64>,
    /// The harness's own `updated` stamp, at its full precision: what tells
    /// one version of a session from the next.
    pub stamp: Option<String>,
    /// True when the harness reports this session is a child/subagent session
    /// rather than a top-level thread.
    pub is_subagent: bool,
}

/// One imported act within a turn read from a harness's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedAct {
    pub harness_call_id: String,
    pub kind: String,
    pub title: String,
    pub state: String,
    pub detail: Option<String>,
}

/// One imported subagent within a turn read from a harness's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedSubagent {
    pub harness_session_id: String,
    pub title: String,
    pub state: String,
    pub error: Option<String>,
    pub prompt: Option<String>,
}

/// One imported turn read from a harness's history into Lever's record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedTurn {
    pub message: String,
    pub answer: String,
    pub acts: Vec<ImportedAct>,
    pub notes: Vec<(String, String)>,
    pub subagents: Vec<ImportedSubagent>,
    pub origin: Option<String>,
}

/// Harness-agnostic history read when opening an externally-started thread.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionHistory {
    pub title: Option<String>,
    pub turns: Vec<ImportedTurn>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// Protocol-agnostic events produced by any [`Harness`] adapter for Lever's
/// service layer.
#[derive(Debug, Clone)]
pub enum HarnessEvent {
    SessionDeleted {
        thread_id: String,
        result: Result<(), String>,
    },
    Initialized,
    InitializeFailed {
        message: String,
    },
    ModelsProbed(Result<ModelCatalog, String>),
    SessionReady {
        thread_id: String,
        session_id: String,
        /// The thread had a session before, and this is not it: the harness
        /// begins with none of the thread's earlier turns.
        afresh: bool,
        catalog: Option<ModelCatalog>,
        current_model: Option<String>,
    },
    SessionFailed {
        thread_id: String,
        turn: u64,
        message: String,
        no_models: bool,
    },
    ModelSet {
        thread_id: String,
        turn: Option<u64>,
        model: String,
        catalog: Option<ModelCatalog>,
    },
    ModelSetFailed {
        thread_id: String,
        turn: Option<u64>,
        message: String,
    },
    CatalogUpdated {
        thread_id: String,
        catalog: Option<ModelCatalog>,
        current_model: Option<String>,
        /// The session's mode changed to this one of Lever's.
        current_mode: Option<String>,
        /// Whether the session's effort selector changed in this update.
        effort_changed: bool,
    },
    /// A session took a thread's mode: for `turn` before its prompt, or,
    /// with none, as the person set it between turns.
    ModeSet {
        thread_id: String,
        turn: Option<u64>,
    },
    ModeSetFailed {
        thread_id: String,
        turn: Option<u64>,
        message: String,
    },
    EffortSet {
        thread_id: String,
        turn: u64,
    },
    EffortSetFailed {
        thread_id: String,
        turn: u64,
        message: String,
    },
    /// The effort levels of a Model, read from a session Lever opened for
    /// that alone. A Model with none has an empty list.
    EffortsProbed {
        model: String,
        result: Result<Vec<EffortOption>, String>,
    },
    /// The commands the harness offers: a thread's session's, or with no
    /// thread, those of a session Lever opened to read what the harness
    /// offers.
    Commands {
        thread_id: Option<String>,
        commands: Vec<HarnessCommand>,
    },
    /// The harness asks the person before it acts.
    PermissionRequested {
        thread_id: String,
        /// The harness's session ID for the subagent that asked, when a
        /// subagent asked rather than the parent thread.
        harness_subagent_id: Option<String>,
        subagent_title: Option<String>,
        /// Lever's handle for answering it, never shown to anyone.
        handle: u64,
        harness_call_id: String,
        /// The act's kind, when the harness said it.
        kind: Option<String>,
        title: String,
        detail: Option<String>,
        path: Option<String>,
        /// Lever's answers with the harness's names for them.
        answers: Vec<(String, String)>,
    },
    /// The harness withdrew a request it asked.
    PermissionWithdrawn {
        handle: u64,
    },
    /// A subagent of a thread started, changed title, or changed state.
    SubagentUpdated {
        thread_id: String,
        harness_subagent_id: String,
        title: Option<String>,
        state: Option<String>,
        error: Option<String>,
        /// Only a launcher's output named the subagent: nothing the harness
        /// said of it yet came from the subagent itself.
        from_launcher: bool,
    },
    /// A piece of a subagent's prompt or answer message.
    SubagentMessage {
        thread_id: String,
        harness_subagent_id: String,
        title: Option<String>,
        /// `"prompt"` or `"answer"`.
        role: String,
        text: String,
    },
    /// An act carried out by a subagent.
    SubagentAct {
        thread_id: String,
        harness_subagent_id: String,
        subagent_title: Option<String>,
        harness_call_id: String,
        kind: Option<String>,
        title: Option<String>,
        state: Option<String>,
        detail: Option<String>,
    },
    TextChunk {
        thread_id: String,
        text: String,
    },
    Act {
        thread_id: String,
        harness_call_id: String,
        is_new: bool,
        kind: Option<String>,
        title: Option<String>,
        state: Option<String>,
        detail: Option<String>,
    },
    TitleUpdated {
        thread_id: String,
        title: String,
    },
    UnknownUpdate {
        thread_id: String,
        raw_json: String,
        plain_text: String,
    },
    PromptFinished {
        thread_id: String,
        turn: u64,
        outcome: TurnOutcome,
    },
    /// The harness answered a message Lever steered into a thread's running
    /// turn: it took it, or it did not, and why, in words for the person.
    SteerAnswered {
        steer: u64,
        taken: Result<(), String>,
    },
    /// History of an externally-started session read into Lever's
    /// harness-agnostic [`SessionHistory`] format.
    HistoryLoaded {
        thread_id: String,
        history: SessionHistory,
        catalog: Option<ModelCatalog>,
    },
    /// Reading an externally-started session's history failed (or is unsupported).
    HistoryLoadFailed {
        thread_id: String,
        message: String,
    },
    /// Sessions listed on the already-running harness.
    SessionsListed(Result<Vec<ExternalSession>, String>),
    Exited,
}

/// Narrow, protocol-agnostic port for a live harness connection driven by
/// Lever's single-threaded `poll(2)` loop.
pub trait Harness {
    /// OS process ID of the harness child, when backed by a child process.
    fn pid(&self) -> Option<u32>;
    /// Readable file descriptor for `poll(2)`, when backed by an OS pipe/socket.
    fn stdout_fd(&self) -> Option<RawFd>;
    /// Writable file descriptor for `poll(2)` when buffered writes wait to flush.
    fn stdin_fd(&self) -> Option<RawFd>;
    /// Whether outbound bytes wait to be flushed to the harness.
    fn wants_write(&self) -> bool;
    /// Flushes queued outbound bytes. Returns `false` on permanent write failure.
    fn on_writable(&mut self) -> bool;
    /// Reads available protocol traffic and translates it into [`HarnessEvent`]s.
    fn on_readable(&mut self) -> Vec<HarnessEvent>;
    /// Non-empty when Lever itself is ending this harness due to a protocol fault.
    fn fault(&self) -> Option<&'static str>;
    /// Ends the harness and every descendant process at once.
    fn kill_tree(self: Box<Self>);
    /// Gracefully stops and reaps the harness.
    fn stop(self: Box<Self>);

    /// Whether the harness handshake has completed.
    fn is_initialized(&self) -> bool;
    /// Structured capabilities of this harness connection.
    fn capabilities(&self) -> HarnessCapabilities;

    /// Active harness session ID bound to `thread_id`, if any.
    fn session_for_thread(&self, thread_id: &str) -> Option<&str>;
    /// Lever thread ID bound to `session_id`, if any.
    fn thread_for_session(&self, session_id: &str) -> Option<&str>;
    /// Whether `session_id` is a known subagent/child session.
    fn is_subagent_session(&self, session_id: &str) -> bool;
    /// Session IDs currently owned by this harness (active threads + probes).
    fn own_session_ids(&self) -> Vec<String>;
    /// Parent subagent session ID that launched `child_session_id`, if nested.
    fn launcher_of(&self, child_session_id: &str) -> Option<&str>;
    /// Original turn number recorded for a subagent routed from a retired
    /// pre-move root session, if any.
    fn retired_turn_of(&self, child_session_id: &str) -> Option<u64>;
    /// Current Model ID of `session_id`, if known.
    fn session_current_model(&self, session_id: &str) -> Option<&str>;
    /// Current Lever mode ID of `session_id`, if known.
    fn session_current_mode(&self, session_id: &str) -> Option<&str>;
    /// Current effort selector of `session_id`, if its Model exposes efforts.
    fn session_effort(&self, session_id: &str) -> Option<&EffortSelector>;

    /// Ensures `thread_id` has a live harness session (resuming `existing_session_id`
    /// when supported and free, or creating a fresh one).
    fn ensure_session(
        &mut self,
        thread_id: &str,
        turn: u64,
        cwd: &str,
        existing_session_id: Option<&str>,
        mcp_servers: &[McpServerSpec],
    );
    /// Loads an externally-started session's history into [`HarnessEvent::HistoryLoaded`].
    fn load_session_history(
        &mut self,
        thread_id: &str,
        session_id: &str,
        cwd: &str,
        mcp_servers: &[McpServerSpec],
    ) -> bool;
    /// Requests deletion of `session_id` from the harness's own store.
    fn delete_session(&mut self, thread_id: &str, session_id: &str) -> bool;
    /// Detaches `thread_id`'s current root session (e.g., on a project move),
    /// keeping it retired for late subagent routing.
    fn detach_session(&mut self, thread_id: &str, retired_turn: Option<u64>);
    /// Forgets all session bindings for `thread_id` (e.g., on thread deletion).
    fn forget_session(&mut self, thread_id: &str);

    /// Sends a user prompt turn to `session_id`.
    fn send_prompt(&mut self, thread_id: &str, turn: u64, session_id: &str, context: Option<&str>, text: &str);
    /// Steers a message into the running turn of `session_id`.
    fn steer(&mut self, thread_id: &str, steer: u64, session_id: &str, context: Option<&str>, text: &str);
    /// Requests cancellation of an active turn or subagent session.
    fn cancel_session(&mut self, session_id: &str);
    /// Answers (or withdraws with `None`) a waiting permission request.
    fn answer_permission(&mut self, handle: u64, answer: Option<&str>) -> bool;

    /// Whether a Model catalog probe is currently in flight.
    fn probing_models(&self) -> bool;
    /// Opens a short-lived probe session to read available Models, modes, and commands.
    fn probe_models(&mut self, cwd: &str);
    /// Opens a short-lived probe session to read effort levels for `model`.
    fn probe_efforts(&mut self, cwd: &str, model: &str);
    /// Drains newly opened probe session IDs so discovery ignores them.
    fn take_new_probe_sessions(&mut self) -> Vec<String>;
    /// Sets `session_id`'s Model.
    fn set_session_model(&mut self, thread_id: &str, turn: Option<u64>, session_id: &str, model: &str);
    /// Sets `session_id`'s Lever mode.
    fn set_session_mode(&mut self, thread_id: &str, turn: Option<u64>, session_id: &str, mode: &str);
    /// Sets `session_id`'s effort level.
    fn set_session_effort(&mut self, thread_id: &str, turn: u64, session_id: &str, effort: &str);

    /// Whether a live session listing is currently in flight.
    fn is_listing_sessions(&self) -> bool;
    /// Abandons an unanswered live session listing.
    fn abandon_list_sessions(&mut self);
    /// Starts a live session listing on the running harness.
    fn list_sessions(&mut self) -> bool;

    /// Seeds previously recorded tool call IDs for redaction after restart.
    fn remember_tool_ids(&mut self, thread_id: &str, ids: Vec<String>);
    /// Seeds previously recorded subagent session IDs for redaction after restart.
    fn remember_subagent_ids(&mut self, thread_id: &str, ids: Vec<String>);
}

/// Factory port that spawns a [`Harness`] instance on demand.
///
/// Production code wires protocol adapters (ACP today, Codex and Claude Code
/// next); tests inject an in-memory fake factory for fast in-process testing.
pub trait HarnessFactory {
    fn spawn(&mut self, config: &HarnessConfig, options: HarnessLaunchOptions) -> Result<Box<dyn Harness>, String>;
}
