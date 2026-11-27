//! Lever's Agent Client Protocol (ACP) v1 client over a harness child's
//! standard input and output, using a non-blocking JSON-RPC 2.0 line loop and
//! the pinned `agent-client-protocol-schema` v1 types.

use crate::channel::LEVER_VERSION;
use crate::discovery::{ExternalSession, ImportedAct, ImportedSubagent, ImportedTurn, SessionHistory};
use crate::opencode;
use crate::sys;
use agent_client_protocol_schema::v1::{
    AGENT_METHOD_NAMES, Annotations, AvailableCommandInput, CLIENT_METHOD_NAMES, CancelNotification,
    CancelRequestNotification, ClientCapabilities, CloseSessionRequest, ContentBlock, Error as AcpError, ErrorCode,
    IdleStopReason, Implementation, InitializeRequest, InitializeResponse, JsonRpcMessage, ListSessionsRequest,
    ListSessionsResponse, LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, Notification,
    PROTOCOL_LEVEL_METHOD_NAMES, PermissionOptionKind, PromptRequest, PromptResponse, Request, RequestId,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, Response, Role,
    SelectedPermissionOutcome, SessionConfigId, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory,
    SessionConfigSelectOptions, SessionId, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, StateUpdate, StopReason, SubagentCapabilities, TextContent, ToolCall,
    ToolCallContent, ToolCallStatus, ToolCallUpdate, ToolKind,
};
use agent_client_protocol_schema::{MaybeUndefined, ProtocolVersion};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout};
use std::time::{Duration, Instant};

/// Most bytes Lever reads from the harness in one wake. A harness that never
/// stops writing then holds back neither Lever's peers nor a stop signal, and
/// a peer that watches a thread is handed its events a part at a time, with
/// the chance to read them in between.
const READ_BUDGET: usize = 256 * 1024;
/// The longest line Lever takes from the harness. ACP is a message to a
/// line, so a harness that writes more than this with no line end is not
/// speaking it, and Lever would otherwise keep all of it in memory.
const MAX_LINE: usize = 16 * 1024 * 1024;
const LONG_LINE: &str = "wrote a line longer than 16 MiB";

/// The OpenCode agent that is Lever's Ask first mode.
pub const ASK_FIRST_AGENT: &str = "crowbar-ask-first";
/// Lever's modes, in the order it offers them, each with the harness's value
/// for it and the person's name for it.
const MODES: [(&str, &str, &str); 3] = [
    ("ask-first", ASK_FIRST_AGENT, "Ask first"),
    ("full-access", "build", "Full access"),
    ("plan", "plan", "Plan"),
];
/// The longest a request's detail grows: a change to a long file is shown in
/// part, and says how much was left out.
const MAX_DETAIL_LINES: usize = 400;
/// Lines of a file shown around a change, as a diff has them.
const DIFF_CONTEXT: usize = 3;

/// Lever's ID for a mode, from the harness's value for it.
fn lever_mode(harness_value: &str) -> Option<&'static str> {
    MODES
        .iter()
        .find(|(_, value, _)| *value == harness_value)
        .map(|(id, _, _)| *id)
}

/// The harness's value for one of Lever's modes.
fn harness_mode(lever_id: &str) -> Option<&'static str> {
    MODES
        .iter()
        .find(|(id, _, _)| *id == lever_id)
        .map(|(_, value, _)| *value)
}

/// The person's name for one of Lever's modes.
pub fn mode_name(lever_id: &str) -> Option<&'static str> {
    MODES
        .iter()
        .find(|(id, _, _)| *id == lever_id)
        .map(|(_, _, name)| *name)
}

/// Whether `id` is one of Lever's modes.
pub fn is_mode(id: &str) -> bool {
    harness_mode(id).is_some()
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

/// A session's effort selector, as Lever reads it from its configuration
/// options: the levels of the session's Model, and the one it is at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortSelector {
    config_id: String,
    pub options: Vec<EffortOption>,
    pub current: String,
}

impl EffortSelector {
    pub fn offers(&self, effort: &str) -> bool {
        self.options.iter().any(|o| o.id == effort)
    }
}

/// One command a harness offers the person, to send as a message that begins
/// with `/` and its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessCommand {
    pub name: String,
    pub description: String,
    /// What the harness says to type after the name, when it says.
    pub hint: Option<String>,
}

/// The Models a harness offers via ACP session configuration options.
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

/// A session's mode selector, as Lever reads it from its configuration
/// options.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModeSelector {
    config_id: String,
    /// Lever's modes the harness offers, in Lever's order.
    modes: Vec<String>,
    /// The session's mode, when it is one of Lever's.
    current: Option<String>,
    /// The harness's value for the session's mode, whatever it is.
    current_value: String,
}

/// How an ACP prompt turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Done,
    Stopped,
    Failed(String),
}

/// Events produced by the ACP harness connection for Lever's service layer.
#[derive(Debug, Clone)]
pub enum AcpEvent {
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
    /// History of an externally-started session read via `session/load` into
    /// Lever's harness-agnostic [`SessionHistory`] format.
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
    /// Sessions listed via `session/list` on the already-running harness.
    SessionsListed(Result<Vec<ExternalSession>, String>),
    Exited,
}

enum PendingRpc {
    DeleteSession {
        thread_id: String,
    },
    Initialize,
    ProbeModels,
    CloseProbe,
    NewSession {
        thread_id: String,
        turn: u64,
        afresh: bool,
    },
    LoadSession {
        thread_id: String,
        turn: u64,
        session_id: String,
        cwd: String,
    },
    /// Loads an externally-started session via `session/load` and captures its
    /// replayed updates into a [`SessionHistory`].
    LoadHistory {
        thread_id: String,
        session_id: String,
    },
    /// Lists sessions on the already-running harness via paginated `session/list`.
    ListSessions {
        page: usize,
        prev_cursor: Option<String>,
    },
    SetModel {
        thread_id: String,
        turn: Option<u64>,
        session_id: String,
        model: String,
    },
    SetMode {
        thread_id: String,
        turn: Option<u64>,
        session_id: String,
        mode: String,
    },
    SetEffort {
        thread_id: String,
        turn: u64,
        session_id: String,
        effort: String,
    },
    /// A session opened to read a Model's effort levels.
    ProbeEffortSession {
        model: String,
    },
    /// That session, set to the Model.
    ProbeEffortModel {
        model: String,
        session_id: String,
    },
    Prompt {
        thread_id: String,
        turn: u64,
    },
    Steer {
        thread_id: String,
        steer: u64,
    },
}

/// OpenCode's own request that steers a message into a session's running
/// prompt. ACP's first version has none, and OpenCode's documents do not
/// describe it: OpenCode 2.4.1 says it has it with `_meta.steering.supported`
/// in its `initialize` answer.
const OPENCODE_STEERING: &str = "_session/steering";

/// What the person is told when the harness did not take a message into its
/// turn because the turn had ended as the message came.
const STEER_TOO_LATE: &str = "The Agent's turn ended before it took your message. Send it again.";

pub struct AcpHarness {
    child: Child,
    /// When the harness started, so what it left behind is told from what
    /// an earlier one did.
    started: u64,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    write_buf: Vec<u8>,
    read_buf: Vec<u8>,
    /// How much of `read_buf` is known to hold no line end, so a long line
    /// is looked through once and not again at each read.
    scanned: usize,
    next_id: i64,
    initialized: bool,
    supports_load_session: bool,
    supports_list_sessions: bool,
    supports_close_session: bool,
    supports_delete_session: bool,
    /// The harness says it can steer a message into a running prompt.
    supports_steering: bool,
    /// The harness says it is OpenCode: its legacy `task` launcher is told by
    /// its kind alone, which another harness's `Task` act may share.
    is_opencode: bool,
    probing_models: bool,
    listing_sessions: bool,
    live_sessions_acc: Vec<ExternalSession>,
    live_sessions_seen: HashSet<String>,
    model_config_id: Option<String>,
    mode_config_id: Option<String>,
    pending: HashMap<i64, PendingRpc>,
    /// Requests the harness asked that wait for an answer, by Lever's handle:
    /// the harness's ID for the request, and its option for each of Lever's
    /// answers.
    permissions: HashMap<u64, Permission>,
    next_permission: u64,
    /// Each session's mode, as Lever's mode ID.
    session_modes: HashMap<String, String>,
    thread_to_session: HashMap<String, String>,
    session_to_thread: HashMap<String, String>,
    session_models: HashMap<String, String>,
    /// Each session's effort selector, when its Model has effort levels.
    session_efforts: HashMap<String, EffortSelector>,
    /// Sessions Lever opened to read what the harness offers, and closed.
    /// What the harness says of one after is its commands, which Lever keeps.
    /// Bounded so long-running harnesses do not accumulate stale probe IDs.
    probe_sessions: VecDeque<String>,
    new_probe_sessions: Vec<String>,
    /// Models whose effort levels are being read.
    probing_efforts: HashSet<String>,
    loading_sessions: HashSet<String>,
    /// Externally-started sessions whose history is currently being replayed
    /// during `session/load`, keyed by harness session ID.
    history_loads: HashMap<String, HistoryBuilder>,
    /// Why Lever is ending this harness, when the harness did not end itself.
    fault: Option<&'static str>,
    /// Tool call IDs seen per thread, used only to redact harness error messages
    /// and unrecognized updates without altering normal model output.
    thread_tool_ids: HashMap<String, HashSet<String>>,
    /// Subagent session IDs seen per thread, used for redaction and to keep
    /// child sessions separate from top-level threads.
    thread_subagent_ids: HashMap<String, HashSet<String>>,
    /// Maps a harness's child session ID to the owning Lever thread ID.
    child_to_thread: HashMap<String, String>,
    /// Last known title of each harness child session, used to strip the
    /// harness's `"<title>: "` prefix from child acts and requests.
    child_titles: HashMap<String, String>,
    /// Parent tool calls that launch an OpenCode subagent (`name == "subagent"`),
    /// kept so their prompt/description attach to the spawned child session
    /// without exposing OpenCode's wrapper tool call or session ID.
    /// Keyed by the session that made the call and the call's ID: a call ID
    /// is unique only within its session.
    pending_subagent_calls: HashMap<(String, String), PendingSubagentCall>,
    /// Subagents whose prompt Lever has passed on, and where it came from, so
    /// one prompt is never shown twice.
    child_prompts: HashMap<String, PromptFrom>,
    /// The subagent session that launched each nested child, by child.
    child_launchers: HashMap<String, String>,
    /// Transport failure recorded when Lever cancels an endless retry loop on a
    /// session whose Model is unreachable, turned into `TurnOutcome::Failed`
    /// when the harness answers the cancelled prompt.
    transport_failures: HashMap<String, String>,
}

struct PendingSubagentCall {
    thread_id: String,
    title: Option<String>,
    prompt: Option<String>,
    child_session_id: Option<String>,
}

/// Where a subagent's prompt came from: its launcher call, or its own
/// session's stream of the message it was given.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PromptFrom {
    Launcher,
    Stream,
}

struct Permission {
    rpc_id: RequestId,
    options: Vec<(String, String)>,
}

impl AcpHarness {
    /// Spawns the configured harness child, configures non-blocking pipes, and
    /// queues the ACP v1 `initialize` request.
    pub fn spawn(config: &HarnessConfig, free_hosted_models: bool) -> Result<Self, String> {
        let existing_config = config
            .env
            .iter()
            .rev()
            .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var("OPENCODE_CONFIG_CONTENT").ok());
        let person = opencode::person_settings(&config.env, existing_config.as_deref());
        let (env_set, env_remove) =
            build_harness_env(&config.env, existing_config.as_deref(), &person, free_hosted_models);
        let mut child = sys::spawn_harness(&config.program, &config.args, &env_set, &env_remove)
            .map_err(|e| format!("could not start harness {}: {e}", config.name))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "harness child had no stdin pipe".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "harness child had no stdout pipe".to_string())?;
        if let Err(e) = sys::set_nonblocking(stdin.as_raw_fd()).and_then(|()| sys::set_nonblocking(stdout.as_raw_fd()))
        {
            // The child is running and nothing will ever speak to it.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "could not set the pipes of harness {} nonblocking: {e}",
                config.name
            ));
        }

        let started = sys::start_time(child.id()).unwrap_or(0);
        let mut harness = Self {
            started,
            child,
            stdin: Some(stdin),
            stdout,
            write_buf: Vec::new(),
            read_buf: Vec::new(),
            scanned: 0,
            next_id: 1,
            initialized: false,
            supports_load_session: false,
            supports_list_sessions: false,
            supports_close_session: false,
            supports_delete_session: false,
            supports_steering: false,
            is_opencode: false,
            probing_models: false,
            listing_sessions: false,
            live_sessions_acc: Vec::new(),
            live_sessions_seen: HashSet::new(),
            model_config_id: None,
            mode_config_id: None,
            pending: HashMap::new(),
            permissions: HashMap::new(),
            next_permission: 1,
            session_modes: HashMap::new(),
            thread_to_session: HashMap::new(),
            session_to_thread: HashMap::new(),
            session_models: HashMap::new(),
            session_efforts: HashMap::new(),
            probe_sessions: VecDeque::new(),
            new_probe_sessions: Vec::new(),
            probing_efforts: HashSet::new(),
            loading_sessions: HashSet::new(),
            history_loads: HashMap::new(),
            fault: None,
            thread_tool_ids: HashMap::new(),
            thread_subagent_ids: HashMap::new(),
            child_to_thread: HashMap::new(),
            child_titles: HashMap::new(),
            pending_subagent_calls: HashMap::new(),
            child_prompts: HashMap::new(),
            child_launchers: HashMap::new(),
            transport_failures: HashMap::new(),
        };

        let mut client_meta = serde_json::Map::new();
        client_meta.insert("opencode/child-session-updates".to_string(), json!(true));
        let capabilities = ClientCapabilities::new()
            .subagents(SubagentCapabilities::new())
            .meta(client_meta);
        let init = InitializeRequest::new(ProtocolVersion::V1)
            .client_capabilities(capabilities)
            .client_info(Implementation::new("crowbar-lever", LEVER_VERSION));
        let id = harness.send_request(AGENT_METHOD_NAMES.initialize, &init);
        harness.pending.insert(id, PendingRpc::Initialize);
        let _ = harness.on_writable();
        Ok(harness)
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn stdout_fd(&self) -> RawFd {
        self.stdout.as_raw_fd()
    }

    pub fn stdin_fd(&self) -> Option<RawFd> {
        self.stdin.as_ref().map(AsRawFd::as_raw_fd)
    }

    pub fn wants_write(&self) -> bool {
        self.stdin.is_some() && !self.write_buf.is_empty()
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    /// What this harness did that Lever is ending it for, if anything.
    pub fn fault(&self) -> Option<&'static str> {
        self.fault
    }

    pub fn session_for_thread(&self, thread_id: &str) -> Option<&str> {
        self.thread_to_session.get(thread_id).map(String::as_str)
    }

    /// Session IDs Lever opened as short-lived probes (for Models or efforts),
    /// plus any sessions currently active or loading, so external session
    /// discovery never lists Lever's own probe or active sessions.
    pub fn own_session_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.probe_sessions.iter().cloned().collect();
        ids.extend(self.session_to_thread.keys().cloned());
        ids.extend(self.loading_sessions.iter().cloned());
        ids.extend(self.history_loads.keys().cloned());
        ids.extend(self.child_to_thread.keys().cloned());
        ids
    }

    /// The subagent session that launched a nested child, if a subagent did.
    pub fn launcher_of(&self, child_session_id: &str) -> Option<&str> {
        self.child_launchers.get(child_session_id).map(String::as_str)
    }

    /// Lets go of a thread's session, which Lever will not use.
    pub fn forget_session(&mut self, thread_id: &str) {
        self.transport_failures.remove(thread_id);
        if let Some(session_id) = self.thread_to_session.remove(thread_id) {
            self.session_to_thread.remove(&session_id);
            self.session_models.remove(&session_id);
            self.session_modes.remove(&session_id);
            self.session_efforts.remove(&session_id);
        }
        let child_ids: Vec<String> = self
            .child_to_thread
            .iter()
            .filter(|(_, owner)| owner.as_str() == thread_id)
            .map(|(cid, _)| cid.clone())
            .collect();
        for cid in child_ids {
            self.child_to_thread.remove(&cid);
            self.child_titles.remove(&cid);
            self.child_prompts.remove(&cid);
            self.child_launchers.remove(&cid);
        }
        self.pending_subagent_calls
            .retain(|_, call| call.thread_id != thread_id);
    }

    pub fn delete_session(&mut self, thread_id: &str, session_id: &str) -> bool {
        if !self.supports_delete_session {
            return false;
        }
        let id = self.send_request(
            AGENT_METHOD_NAMES.session_delete,
            &agent_client_protocol_schema::v1::DeleteSessionRequest::new(session_id.to_string()),
        );
        self.pending.insert(
            id,
            PendingRpc::DeleteSession {
                thread_id: thread_id.into(),
            },
        );
        true
    }

    pub fn session_current_model(&self, session_id: &str) -> Option<&str> {
        self.session_models.get(session_id).map(String::as_str)
    }

    /// A session's mode, as Lever's mode ID, when it is one of Lever's.
    pub fn session_current_mode(&self, session_id: &str) -> Option<&str> {
        self.session_modes.get(session_id).map(String::as_str)
    }

    /// A session's effort selector, when its Model has effort levels.
    pub fn session_effort(&self, session_id: &str) -> Option<&EffortSelector> {
        self.session_efforts.get(session_id)
    }

    /// Sets an active session's effort, one its selector offers, through
    /// `session/set_config_option`.
    pub fn set_session_effort(&mut self, thread_id: &str, turn: u64, session_id: &str, effort: &str) {
        let config_id = self
            .session_efforts
            .get(session_id)
            .map(|sel| sel.config_id.clone())
            .unwrap_or_else(|| "effort".to_string());
        let req = SetSessionConfigOptionRequest::new(
            SessionId::new(session_id.to_string()),
            SessionConfigId::new(config_id),
            effort,
        );
        let id = self.send_request(AGENT_METHOD_NAMES.session_set_config_option, &req);
        self.pending.insert(
            id,
            PendingRpc::SetEffort {
                thread_id: thread_id.to_string(),
                turn,
                session_id: session_id.to_string(),
                effort: effort.to_string(),
            },
        );
        let _ = self.on_writable();
    }

    /// Reads a Model's effort levels by opening a session, setting it to the
    /// Model, reading its `configOptions`, and closing it. One read runs for
    /// a Model at a time.
    pub fn probe_efforts(&mut self, cwd: &str, model: &str) {
        if !self.probing_efforts.insert(model.to_string()) {
            return;
        }
        let req = NewSessionRequest::new(PathBuf::from(cwd));
        let id = self.send_request(AGENT_METHOD_NAMES.session_new, &req);
        self.pending.insert(
            id,
            PendingRpc::ProbeEffortSession {
                model: model.to_string(),
            },
        );
        let _ = self.on_writable();
    }

    /// Records a probe session ID while bounding history to recent probes.
    fn remember_probe_session(&mut self, session_id: String) {
        if self.probe_sessions.iter().any(|s| s == &session_id) {
            return;
        }
        const MAX_PROBE_SESSIONS: usize = 16;
        if self.probe_sessions.len() >= MAX_PROBE_SESSIONS {
            self.probe_sessions.pop_front();
        }
        self.new_probe_sessions.push(session_id.clone());
        self.probe_sessions.push_back(session_id);
    }

    /// Probe sessions opened since this was last asked, for Lever to remember
    /// as its own past this harness's life.
    pub fn take_new_probe_sessions(&mut self) -> Vec<String> {
        std::mem::take(&mut self.new_probe_sessions)
    }

    /// Closes a session Lever opened to read what the harness offers. What
    /// the harness says of it after is taken for its commands alone.
    fn close_probe(&mut self, session_id: SessionId) {
        if session_id.0.is_empty() {
            return;
        }
        self.remember_probe_session(session_id.0.as_ref().to_string());
        if self.supports_close_session {
            let close = CloseSessionRequest::new(session_id);
            let id = self.send_request(AGENT_METHOD_NAMES.session_close, &close);
            self.pending.insert(id, PendingRpc::CloseProbe);
            let _ = self.on_writable();
        }
    }

    /// Whether the live harness is initialized and advertises ACP `session/list`.
    pub fn can_list_sessions(&self) -> bool {
        self.initialized && self.supports_list_sessions
    }

    /// Whether a live `session/list` request is currently in flight.
    pub fn is_listing_sessions(&self) -> bool {
        self.listing_sessions
    }

    /// Gives up on a live `session/list` the harness has not answered: a
    /// reply that comes later is dropped.
    pub fn abandon_list_sessions(&mut self) {
        self.pending
            .retain(|_, pending| !matches!(pending, PendingRpc::ListSessions { .. }));
        self.listing_sessions = false;
        self.live_sessions_acc.clear();
        self.live_sessions_seen.clear();
    }

    /// Starts a paginated `session/list` on the already-running harness so
    /// discovery does not spawn a second harness process while one is live.
    pub fn list_sessions(&mut self) -> bool {
        if !self.can_list_sessions() || self.listing_sessions {
            return false;
        }
        self.listing_sessions = true;
        self.live_sessions_acc.clear();
        self.live_sessions_seen.clear();
        let req = ListSessionsRequest::new();
        let id = self.send_request(AGENT_METHOD_NAMES.session_list, &req);
        self.pending.insert(
            id,
            PendingRpc::ListSessions {
                page: 0,
                prev_cursor: None,
            },
        );
        let _ = self.on_writable();
        true
    }

    /// Reads a session's effort selector from its configuration options, and
    /// keeps it. Options that name the session's Model and no effort say the
    /// Model has no effort levels; options that name neither say nothing.
    fn take_effort(&mut self, session_id: &str, options: &[SessionConfigOption]) {
        match extract_effort(options) {
            Some(selector) => {
                self.session_efforts.insert(session_id.to_string(), selector);
            }
            None if model_option(options).is_some() => {
                self.session_efforts.remove(session_id);
            }
            None => {}
        }
    }

    /// The thread whose session is being loaded under this ID.
    fn loading_thread(&self, session_id: &str) -> Option<String> {
        self.pending.values().find_map(|pending| match pending {
            PendingRpc::LoadSession {
                thread_id,
                session_id: loading,
                ..
            } if loading == session_id => Some(thread_id.clone()),
            _ => None,
        })
    }

    /// Whether a thread's session is being loaded, when what its subagents
    /// say is history the harness replays, which Lever already holds.
    fn thread_loading(&self, thread_id: &str) -> bool {
        self.pending.values().any(
            |pending| matches!(pending, PendingRpc::LoadSession { thread_id: loading, .. } if loading == thread_id),
        )
    }

    /// Whether the harness has a mode selector Lever can set.
    pub fn has_modes(&self) -> bool {
        self.mode_config_id.is_some()
    }

    /// Sets an active session's mode, one of Lever's, through
    /// `session/set_config_option`.
    pub fn set_session_mode(&mut self, thread_id: &str, turn: Option<u64>, session_id: &str, mode: &str) {
        let config_id = self.mode_config_id.clone().unwrap_or_else(|| "mode".to_string());
        let value = harness_mode(mode).unwrap_or(mode);
        let req = SetSessionConfigOptionRequest::new(
            SessionId::new(session_id.to_string()),
            SessionConfigId::new(config_id),
            value,
        );
        let id = self.send_request(AGENT_METHOD_NAMES.session_set_config_option, &req);
        self.pending.insert(
            id,
            PendingRpc::SetMode {
                thread_id: thread_id.to_string(),
                turn,
                session_id: session_id.to_string(),
                mode: mode.to_string(),
            },
        );
        let _ = self.on_writable();
    }

    /// Answers a request the harness asked: with the option it offered for
    /// Lever's `answer`, or `cancelled` when there is none. Returns `false`
    /// when the request no longer waits, or offers no such answer.
    pub fn answer_permission(&mut self, handle: u64, answer: Option<&str>) -> bool {
        let Some(permission) = self.permissions.get(&handle) else {
            return false;
        };
        let outcome = match answer {
            None => RequestPermissionOutcome::Cancelled,
            Some(answer) => match permission.options.iter().find(|(ours, _)| ours == answer) {
                Some((_, option_id)) => {
                    RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(option_id.clone()))
                }
                None => return false,
            },
        };
        let Some(permission) = self.permissions.remove(&handle) else {
            return false;
        };
        self.send_result_response(permission.rpc_id, RequestPermissionResponse::new(outcome));
        let _ = self.on_writable();
        true
    }

    /// Whether a Model catalog probe is currently in flight.
    pub fn probing_models(&self) -> bool {
        self.probing_models
    }

    /// Probes the harness's offered Models by opening a session, reading its
    /// `configOptions`, and closing that session if the harness supports
    /// `session/close`. Only one probe runs at a time.
    pub fn probe_models(&mut self, cwd: &str) {
        if self.probing_models {
            return;
        }
        self.probing_models = true;
        let req = NewSessionRequest::new(PathBuf::from(cwd));
        let id = self.send_request(AGENT_METHOD_NAMES.session_new, &req);
        self.pending.insert(id, PendingRpc::ProbeModels);
        let _ = self.on_writable();
    }

    /// Sets the Model on an active ACP session through `session/set_config_option`.
    pub fn set_session_model(&mut self, thread_id: &str, turn: Option<u64>, session_id: &str, model: &str) {
        let config_id = self.model_config_id.as_deref().unwrap_or("model");
        let req = SetSessionConfigOptionRequest::new(
            SessionId::new(session_id.to_string()),
            SessionConfigId::new(config_id.to_string()),
            model,
        );
        let id = self.send_request(AGENT_METHOD_NAMES.session_set_config_option, &req);
        self.pending.insert(
            id,
            PendingRpc::SetModel {
                thread_id: thread_id.to_string(),
                turn,
                session_id: session_id.to_string(),
                model: model.to_string(),
            },
        );
        let _ = self.on_writable();
    }

    /// Starts or reloads an ACP session for `thread_id`. A thread has one
    /// session request under way at a time: when one is, it is given `turn`,
    /// so its answer goes to the turn that now waits for it.
    pub fn ensure_session(&mut self, thread_id: &str, turn: u64, cwd: &str, existing_session_id: Option<&str>) {
        for pending in self.pending.values_mut() {
            match pending {
                PendingRpc::NewSession {
                    thread_id: waiting,
                    turn: waiting_turn,
                    ..
                }
                | PendingRpc::LoadSession {
                    thread_id: waiting,
                    turn: waiting_turn,
                    ..
                } if waiting == thread_id => {
                    *waiting_turn = turn;
                    return;
                }
                _ => {}
            }
        }
        let Some(sid) = existing_session_id else {
            self.request_new_session(thread_id, turn, cwd, false);
            return;
        };
        // A session another thread holds, or is loading, is not this thread's
        // to load, whatever ID the harness once gave it. Nor is one the
        // record says was given to another thread since: its ID is empty.
        let taken = sid.is_empty()
            || self.session_to_thread.get(sid).is_some_and(|owner| owner != thread_id)
            || self.loading_sessions.contains(sid)
            || self.history_loads.contains_key(sid);
        if !self.supports_load_session || taken {
            self.request_new_session(thread_id, turn, cwd, true);
            return;
        }
        self.loading_sessions.insert(sid.to_string());
        let req = LoadSessionRequest::new(SessionId::new(sid.to_string()), PathBuf::from(cwd));
        let id = self.send_request(AGENT_METHOD_NAMES.session_load, &req);
        self.pending.insert(
            id,
            PendingRpc::LoadSession {
                thread_id: thread_id.to_string(),
                turn,
                session_id: sid.to_string(),
                cwd: cwd.to_string(),
            },
        );
        let _ = self.on_writable();
    }

    fn request_new_session(&mut self, thread_id: &str, turn: u64, cwd: &str, afresh: bool) {
        let req = NewSessionRequest::new(PathBuf::from(cwd));
        let id = self.send_request(AGENT_METHOD_NAMES.session_new, &req);
        self.pending.insert(
            id,
            PendingRpc::NewSession {
                thread_id: thread_id.to_string(),
                turn,
                afresh,
            },
        );
        let _ = self.on_writable();
    }

    /// Loads an externally-started session via `session/load` and captures its
    /// replayed history into a [`SessionHistory`]. Returns `false` if the
    /// harness does not advertise `loadSession` or the session is already held
    /// by another thread.
    pub fn load_session_history(&mut self, thread_id: &str, session_id: &str, cwd: &str) -> bool {
        if !self.supports_load_session || session_id.is_empty() {
            return false;
        }
        if self.history_loads.contains_key(session_id)
            || self.loading_sessions.contains(session_id)
            || self
                .session_to_thread
                .get(session_id)
                .is_some_and(|owner| owner != thread_id)
        {
            return false;
        }
        self.history_loads
            .insert(session_id.to_string(), HistoryBuilder::new(self.is_opencode));
        let req = LoadSessionRequest::new(SessionId::new(session_id.to_string()), PathBuf::from(cwd));
        let id = self.send_request(AGENT_METHOD_NAMES.session_load, &req);
        self.pending.insert(
            id,
            PendingRpc::LoadHistory {
                thread_id: thread_id.to_string(),
                session_id: session_id.to_string(),
            },
        );
        let _ = self.on_writable();
        true
    }

    /// Makes `session_id` the session of `thread_id`, and of no other. Says
    /// no when another thread holds it or is loading it: two threads on one
    /// session would have each other's answers. A thread that loaded a
    /// session is no longer loading it when it comes here.
    fn bind_session(&mut self, thread_id: &str, session_id: &str) -> bool {
        if self.loading_sessions.contains(session_id)
            || self.history_loads.contains_key(session_id)
            || self
                .session_to_thread
                .get(session_id)
                .is_some_and(|owner| owner != thread_id)
        {
            return false;
        }
        if let Some(before) = self
            .thread_to_session
            .insert(thread_id.to_string(), session_id.to_string())
            && before != session_id
        {
            self.session_to_thread.remove(&before);
        }
        self.session_to_thread
            .insert(session_id.to_string(), thread_id.to_string());
        true
    }

    /// Sends a `session/prompt` request for an active session, attaching
    /// desktop context as assistant-audience context so harnesses still see
    /// leading slash commands in the person's text.
    pub fn send_prompt(&mut self, thread_id: &str, turn: u64, session_id: &str, context: Option<&str>, text: &str) {
        self.transport_failures.remove(thread_id);
        let req = PromptRequest::new(SessionId::new(session_id.to_string()), prompt_blocks(context, text));
        let id = self.send_request(AGENT_METHOD_NAMES.session_prompt, &req);
        self.pending.insert(
            id,
            PendingRpc::Prompt {
                thread_id: thread_id.to_string(),
                turn,
            },
        );
        let _ = self.on_writable();
    }

    /// Whether the harness can take a message into a turn under way.
    pub fn can_steer(&self) -> bool {
        self.initialized && self.supports_steering
    }

    /// Steers a message into the running prompt of a thread's session, with
    /// desktop context as a prompt carries it. The harness's answer comes as
    /// `AcpEvent::SteerAnswered` with Lever's `steer` for it.
    pub fn steer(&mut self, thread_id: &str, steer: u64, session_id: &str, context: Option<&str>, text: &str) {
        let params = json!({
            "sessionId": session_id,
            "prompt": prompt_blocks(context, text),
        });
        let id = self.send_request(OPENCODE_STEERING, &params);
        self.pending.insert(
            id,
            PendingRpc::Steer {
                thread_id: thread_id.to_string(),
                steer,
            },
        );
        let _ = self.on_writable();
    }

    /// Sends a `session/cancel` notification for an active session.
    pub fn cancel_session(&mut self, session_id: &str) {
        if let Some(thread_id) = self.session_to_thread.get(session_id).cloned() {
            self.transport_failures.remove(&thread_id);
        }
        let cancel = CancelNotification::new(SessionId::new(session_id.to_string()));
        self.send_notification(AGENT_METHOD_NAMES.session_cancel, &cancel);
        let _ = self.on_writable();
    }

    /// Flushes queued JSON-RPC bytes to the child's stdin. Returns `false` if
    /// writing failed permanently.
    pub fn on_writable(&mut self) -> bool {
        let Some(stdin) = self.stdin.as_mut() else {
            return false;
        };
        while !self.write_buf.is_empty() {
            match stdin.write(&self.write_buf) {
                Ok(0) => return false,
                Ok(n) => {
                    self.write_buf.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
        true
    }

    /// Reads JSON-RPC lines from the child's stdout, up to `READ_BUDGET` in
    /// one call, and returns any resulting events. What is left wakes Lever
    /// again.
    pub fn on_readable(&mut self) -> Vec<AcpEvent> {
        let mut events = Vec::new();
        let mut buf = [0u8; 16384];
        let mut taken = 0;
        while taken < READ_BUDGET {
            match self.stdout.read(&mut buf) {
                Ok(0) => {
                    self.drain_lines(&mut events);
                    events.push(AcpEvent::Exited);
                    return events;
                }
                Ok(n) => {
                    taken += n;
                    self.read_buf.extend_from_slice(&buf[..n]);
                    self.drain_lines(&mut events);
                    // What is left is the start of a line with no end yet.
                    if self.read_buf.len() > MAX_LINE {
                        self.fault = Some(LONG_LINE);
                    }
                    if self.fault.is_some() {
                        events.push(AcpEvent::Exited);
                        return events;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    events.push(AcpEvent::Exited);
                    return events;
                }
            }
        }
        events
    }

    /// Ends the harness and every process under it at once, for a harness
    /// that did not stop a turn when asked: what the turn started must not go
    /// on.
    pub fn kill_tree(mut self) {
        self.stdin.take();
        let pid = self.child.id();
        sys::kill_all_of_harness(pid, self.started);
        let _ = self.child.kill();
        let _ = self.child.wait();
        // What it left behind ended too, and is Lever's to wait for. One
        // that is still there is ended again.
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            sys::reap_orphans(None);
            if sys::live_of_harness(pid, self.started).is_empty() || Instant::now() >= deadline {
                break;
            }
            sys::kill_all_of_harness(pid, self.started);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Stops and reaps the harness child process.
    pub fn stop(mut self) {
        self.stdin.take();
        if self.wait_exit(Duration::from_millis(200)) {
            return;
        }
        sys::signal_pid(self.child.id(), libc::SIGTERM);
        if self.wait_exit(Duration::from_millis(500)) {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn wait_exit(&mut self, patience: Duration) -> bool {
        let deadline = Instant::now() + patience;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                Ok(None) => return false,
                Err(_) => return true,
            }
        }
    }

    fn send_request<P: serde::Serialize>(&mut self, method: &str, params: &P) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let msg = JsonRpcMessage::wrap(Request {
            id: RequestId::Number(id),
            method: method.into(),
            params: Some(params),
        });
        self.queue_json(&serde_json::to_value(msg).expect("serialize JSON-RPC request"));
        id
    }

    fn send_notification<P: serde::Serialize>(&mut self, method: &str, params: &P) {
        let msg = JsonRpcMessage::wrap(Notification {
            method: method.into(),
            params: Some(params),
        });
        self.queue_json(&serde_json::to_value(msg).expect("serialize JSON-RPC notification"));
    }

    fn send_result_response<R: serde::Serialize>(&mut self, id: RequestId, result: R) {
        let msg = JsonRpcMessage::wrap(Response::<R, AcpError>::new(id, Ok(result)));
        self.queue_json(&serde_json::to_value(msg).expect("serialize JSON-RPC result response"));
    }

    fn send_error_response(&mut self, id: RequestId, err: AcpError) {
        let msg = JsonRpcMessage::wrap(Response::<Value, AcpError>::new(id, Err(err)));
        self.queue_json(&serde_json::to_value(msg).expect("serialize JSON-RPC error response"));
    }

    fn queue_json(&mut self, value: &Value) {
        let line = value.to_string();
        self.write_buf.extend_from_slice(line.as_bytes());
        self.write_buf.push(b'\n');
    }

    fn drain_lines(&mut self, events: &mut Vec<AcpEvent>) {
        while let Some(found) = self.read_buf[self.scanned..].iter().position(|&b| b == b'\n') {
            let pos = self.scanned + found;
            // A line that is too long is too long wherever its end came in
            // what was read.
            if pos > MAX_LINE {
                self.fault = Some(LONG_LINE);
                return;
            }
            let line = self.read_buf[..pos].to_vec();
            self.read_buf.drain(..=pos);
            self.scanned = 0;
            let Ok(text) = std::str::from_utf8(&line) else {
                continue;
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            self.handle_message(value, events);
        }
        self.scanned = self.read_buf.len();
    }

    fn handle_message(&mut self, value: Value, events: &mut Vec<AcpEvent>) {
        let has_method = value.get("method").and_then(Value::as_str).is_some();
        // A notification that carries `"id": null` is still a notification.
        let has_id = value.get("id").is_some_and(|id| !id.is_null());

        if has_method && has_id {
            if let Ok(id) = serde_json::from_value::<RequestId>(value["id"].clone()) {
                let method = value.get("method").and_then(Value::as_str).unwrap_or("");
                if method == CLIENT_METHOD_NAMES.session_request_permission {
                    self.handle_permission_request(id, value.get("params"), events);
                } else {
                    self.send_error_response(id, AcpError::method_not_found());
                }
                let _ = self.on_writable();
            }
            return;
        }

        if has_method {
            match value.get("method").and_then(Value::as_str) {
                Some(m) if m == PROTOCOL_LEVEL_METHOD_NAMES.cancel_request => {
                    self.handle_cancel_request(value.get("params"), events);
                }
                Some("opencode/session/child_update") => {
                    self.handle_opencode_child_update(value.get("params"), events);
                }
                _ => {
                    self.handle_notification(&value, events);
                }
            }
            return;
        }

        if has_id {
            self.handle_response(value, events);
        }
    }

    /// Takes a `session/request_permission` to the person, as a request in
    /// the thread whose session (or subagent session) asks it. One for a
    /// session Lever holds for no thread is answered `cancelled` at once:
    /// nobody would see it.
    fn handle_permission_request(&mut self, rpc_id: RequestId, params: Option<&Value>, events: &mut Vec<AcpEvent>) {
        let request = params.and_then(|p| serde_json::from_value::<RequestPermissionRequest>(p.clone()).ok());
        let resolved = request.as_ref().and_then(|r| {
            let sid = r.session_id.0.as_ref();
            if let Some(owner) = self.child_to_thread.get(sid).cloned() {
                if self.thread_loading(&owner) {
                    return None;
                }
                let title = self.child_titles.get(sid).cloned();
                return Some((owner, Some(sid.to_string()), title));
            }
            if self.loading_sessions.contains(sid) {
                return None;
            }
            let owner = self.session_to_thread.get(sid).cloned()?;
            if let Some((child_sid, child_title)) = params
                .and_then(|p| p.get("toolCall"))
                .and_then(extract_opencode_child_meta)
            {
                let effective_title = child_title.or_else(|| self.child_titles.get(&child_sid).cloned());
                Some((owner, Some(child_sid), effective_title))
            } else {
                Some((owner, None, None))
            }
        });
        let (Some(request), Some((thread_id, harness_subagent_id, subagent_title))) = (request, resolved) else {
            self.send_result_response(
                rpc_id,
                RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
            );
            let _ = self.on_writable();
            return;
        };
        // A request in the name of a session that is not this thread's
        // subagent is nobody's to answer here.
        if let Some(ref child_sid) = harness_subagent_id
            && !self.register_child(&thread_id, child_sid, subagent_title.as_deref())
        {
            self.send_result_response(
                rpc_id,
                RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
            );
            let _ = self.on_writable();
            return;
        }
        let mut options = Vec::new();
        let mut answers = Vec::new();
        for option in &request.options {
            let ours = match option.kind {
                PermissionOptionKind::AllowOnce => "once",
                PermissionOptionKind::AllowAlways => "always",
                PermissionOptionKind::RejectOnce => "no",
                PermissionOptionKind::RejectAlways => "never",
                _ => continue,
            };
            if options.iter().any(|(o, _): &(String, String)| o == ours) {
                continue;
            }
            options.push((ours.to_string(), option.option_id.0.as_ref().to_string()));
            answers.push((ours.to_string(), option.name.clone()));
        }
        if answers.is_empty() {
            self.send_result_response(
                rpc_id,
                RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
            );
            let _ = self.on_writable();
            return;
        }
        let call = &request.tool_call;
        let call_id = call.tool_call_id.0.as_ref().to_string();
        self.thread_tool_ids
            .entry(thread_id.clone())
            .or_default()
            .insert(call_id.clone());
        let kind = call.fields.kind.map(|k| map_tool_kind(k).to_string());
        // The command or the change is shown as the Model wrote it, as an
        // act's is: the person answers for exactly that.
        let (detail, path) = request_detail(call.fields.kind, &call.fields);
        let raw_title = call
            .fields
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| path.clone())
            .or_else(|| detail.as_ref().and_then(|d| d.lines().next().map(str::to_string)))
            .unwrap_or_else(|| "Act".to_string());
        let title = if harness_subagent_id.is_some() {
            strip_child_title_prefix(&raw_title, subagent_title.as_deref())
        } else {
            raw_title
        };
        let handle = self.next_permission;
        self.next_permission += 1;
        self.permissions.insert(handle, Permission { rpc_id, options });
        events.push(AcpEvent::PermissionRequested {
            thread_id,
            harness_subagent_id,
            subagent_title,
            handle,
            harness_call_id: call_id,
            kind,
            title,
            detail,
            path,
            answers,
        });
    }

    /// The harness withdrew a request it asked (`$/cancel_request`).
    fn handle_cancel_request(&mut self, params: Option<&Value>, events: &mut Vec<AcpEvent>) {
        let Some(cancel) = params.and_then(|p| serde_json::from_value::<CancelRequestNotification>(p.clone()).ok())
        else {
            return;
        };
        let handle = self
            .permissions
            .iter()
            .find(|(_, permission)| permission.rpc_id == cancel.request_id)
            .map(|(handle, _)| *handle);
        if let Some(handle) = handle
            && let Some(permission) = self.permissions.remove(&handle)
        {
            // The request still gets its answer, as ACP has it: cancelled.
            self.send_result_response(
                permission.rpc_id,
                RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
            );
            let _ = self.on_writable();
            events.push(AcpEvent::PermissionWithdrawn { handle });
        }
    }

    /// Reads a session's mode selector from its configuration options, and
    /// keeps what Lever needs to set it.
    fn take_modes(&mut self, session_id: &str, options: Option<&[SessionConfigOption]>) -> Option<ModeSelector> {
        let selector = options.and_then(extract_modes)?;
        self.mode_config_id = Some(selector.config_id.clone());
        match &selector.current {
            Some(mode) => {
                self.session_modes.insert(session_id.to_string(), mode.clone());
            }
            None => {
                self.session_modes.remove(session_id);
            }
        }
        Some(selector)
    }

    fn handle_response(&mut self, value: Value, events: &mut Vec<AcpEvent>) {
        let Some(id) = value.get("id").and_then(Value::as_i64) else {
            return;
        };
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };

        // A result that carries `"error": null` beside it is still a result.
        if let Some(err_val) = value.get("error").filter(|v| !v.is_null()) {
            let error = serde_json::from_value::<AcpError>(err_val.clone()).ok();
            let cancelled = error.as_ref().is_some_and(|e| e.code == ErrorCode::RequestCancelled);
            let raw_msg = error
                .map(|e| e.message)
                .unwrap_or_else(|| "The harness reported an error.".to_string());
            let no_models = is_no_models_error(err_val, &raw_msg);
            match pending {
                PendingRpc::DeleteSession { thread_id } => {
                    let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                    events.push(AcpEvent::SessionDeleted {
                        thread_id,
                        result: Err(message),
                    });
                }

                PendingRpc::Initialize => events.push(AcpEvent::InitializeFailed {
                    message: self.redact_for_thread(None, &raw_msg),
                }),
                PendingRpc::ProbeModels => {
                    self.probing_models = false;
                    if no_models {
                        events.push(AcpEvent::ModelsProbed(Ok(ModelCatalog {
                            supported: true,
                            models: Vec::new(),
                            default_model: None,
                            config_id: None,
                            modes: Vec::new(),
                        })));
                    } else {
                        events.push(AcpEvent::ModelsProbed(Err(self.redact_for_thread(None, &raw_msg))));
                    }
                }
                PendingRpc::CloseProbe => {}
                PendingRpc::NewSession { thread_id, turn, .. } => {
                    let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                    events.push(AcpEvent::SessionFailed {
                        thread_id,
                        turn,
                        message,
                        no_models,
                    });
                }
                PendingRpc::LoadSession {
                    thread_id,
                    turn,
                    session_id,
                    cwd,
                } => {
                    self.loading_sessions.remove(&session_id);
                    self.request_new_session(&thread_id, turn, &cwd, true);
                }
                PendingRpc::LoadHistory { thread_id, session_id } => {
                    self.history_loads.remove(&session_id);
                    // The session is not yet the thread's, so it is named here.
                    let message = redact_ids(
                        &self.redact_for_thread(Some(&thread_id), &raw_msg),
                        vec![session_id.as_str()],
                    );
                    events.push(AcpEvent::HistoryLoadFailed { thread_id, message });
                }
                PendingRpc::ListSessions { .. } => {
                    self.listing_sessions = false;
                    self.live_sessions_acc.clear();
                    self.live_sessions_seen.clear();
                    events.push(AcpEvent::SessionsListed(Err(self.redact_for_thread(None, &raw_msg))));
                }
                PendingRpc::SetModel { thread_id, turn, .. } => {
                    let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                    events.push(AcpEvent::ModelSetFailed {
                        thread_id,
                        turn,
                        message,
                    });
                }
                PendingRpc::SetMode { thread_id, turn, .. } => {
                    let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                    events.push(AcpEvent::ModeSetFailed {
                        thread_id,
                        turn,
                        message,
                    });
                }
                PendingRpc::SetEffort { thread_id, turn, .. } => {
                    let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                    events.push(AcpEvent::EffortSetFailed {
                        thread_id,
                        turn,
                        message,
                    });
                }
                PendingRpc::ProbeEffortSession { model } => {
                    self.probing_efforts.remove(&model);
                    events.push(AcpEvent::EffortsProbed {
                        model,
                        result: Err(self.redact_for_thread(None, &raw_msg)),
                    });
                }
                PendingRpc::ProbeEffortModel { model, session_id } => {
                    self.probing_efforts.remove(&model);
                    self.close_probe(SessionId::new(session_id));
                    events.push(AcpEvent::EffortsProbed {
                        model,
                        result: Err(self.redact_for_thread(None, &raw_msg)),
                    });
                }
                // The harness says the turn was cancelled, with an error
                // where ACP has `StopReason::Cancelled`. Any other error is
                // a failure, whether or not a stop was asked for.
                PendingRpc::Prompt { thread_id, turn } if cancelled => {
                    let outcome = match self.transport_failures.remove(&thread_id) {
                        Some(message) => TurnOutcome::Failed(message),
                        None => TurnOutcome::Stopped,
                    };
                    events.push(AcpEvent::PromptFinished {
                        thread_id,
                        turn,
                        outcome,
                    });
                }
                PendingRpc::Prompt { thread_id, turn } => {
                    self.transport_failures.remove(&thread_id);
                    let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                    events.push(AcpEvent::PromptFinished {
                        thread_id,
                        turn,
                        outcome: TurnOutcome::Failed(message),
                    });
                }
                PendingRpc::Steer { thread_id, steer } => {
                    let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                    events.push(AcpEvent::SteerAnswered {
                        steer,
                        taken: Err(format!("The Agent did not take your message into its turn: {message}")),
                    });
                }
            }
            return;
        }

        let result = value.get("result").cloned().unwrap_or(Value::Null);
        match pending {
            PendingRpc::DeleteSession { thread_id } => {
                events.push(AcpEvent::SessionDeleted {
                    thread_id,
                    result: Ok(()),
                });
            }
            PendingRpc::Initialize => match serde_json::from_value::<InitializeResponse>(result) {
                Ok(resp) if resp.protocol_version == ProtocolVersion::V1 => {
                    self.initialized = true;
                    self.supports_load_session = resp.agent_capabilities.load_session;
                    self.supports_list_sessions = resp.agent_capabilities.session_capabilities.list.is_some();
                    self.supports_close_session = resp.agent_capabilities.session_capabilities.close.is_some();
                    self.supports_delete_session = resp.agent_capabilities.session_capabilities.delete.is_some();
                    self.supports_steering = steering_supported(resp.meta.as_ref());
                    self.is_opencode = resp
                        .agent_info
                        .as_ref()
                        .is_some_and(|info| info.name.eq_ignore_ascii_case("opencode"));
                    events.push(AcpEvent::Initialized);
                }
                Ok(_) => events.push(AcpEvent::InitializeFailed {
                    message: "The harness uses an unsupported protocol version.".to_string(),
                }),
                Err(_) => events.push(AcpEvent::InitializeFailed {
                    message: "The harness returned an invalid initialization response.".to_string(),
                }),
            },
            PendingRpc::ProbeModels => {
                self.probing_models = false;
                match serde_json::from_value::<NewSessionResponse>(result) {
                    Ok(resp) => {
                        let mut catalog = resp
                            .config_options
                            .as_deref()
                            .and_then(extract_model_catalog)
                            .unwrap_or(ModelCatalog {
                                supported: false,
                                models: Vec::new(),
                                default_model: None,
                                config_id: None,
                                modes: Vec::new(),
                            });
                        if let Some(selector) = resp.config_options.as_deref().and_then(extract_modes) {
                            self.mode_config_id = Some(selector.config_id);
                            catalog.modes = selector.modes;
                        }
                        if let Some(ref cid) = catalog.config_id {
                            self.model_config_id = Some(cid.clone());
                        }
                        // The session is at the harness's default Model, so
                        // its options say that Model's effort levels too.
                        let efforts = catalog.default_model.clone().map(|model| AcpEvent::EffortsProbed {
                            model,
                            result: Ok(resp
                                .config_options
                                .as_deref()
                                .and_then(extract_effort)
                                .map(|selector| selector.options)
                                .unwrap_or_default()),
                        });
                        self.close_probe(resp.session_id);
                        events.push(AcpEvent::ModelsProbed(Ok(catalog)));
                        events.extend(efforts);
                    }
                    Err(_) => events.push(AcpEvent::ModelsProbed(Err(
                        "The harness returned an invalid session response.".to_string(),
                    ))),
                }
            }
            PendingRpc::CloseProbe => {}
            PendingRpc::NewSession {
                thread_id,
                turn,
                afresh,
            } => match serde_json::from_value::<NewSessionResponse>(result) {
                // A session with no ID is no session: Lever could neither
                // tell it from another nor load it again.
                Ok(resp) if !resp.session_id.0.is_empty() => {
                    let sid = resp.session_id.0.as_ref().to_string();
                    let mut catalog = resp.config_options.as_deref().and_then(extract_model_catalog);
                    let current_model = catalog.as_ref().and_then(|c| c.default_model.clone());
                    if let Some(ref cid) = catalog.as_ref().and_then(|c| c.config_id.clone()) {
                        self.model_config_id = Some(cid.clone());
                    }
                    if self.bind_session(&thread_id, &sid) {
                        if let Some(ref m) = current_model {
                            self.session_models.insert(sid.clone(), m.clone());
                        }
                        self.take_effort(&sid, resp.config_options.as_deref().unwrap_or(&[]));
                        let modes = self.take_modes(&sid, resp.config_options.as_deref());
                        if let (Some(cat), Some(selector)) = (catalog.as_mut(), modes.as_ref()) {
                            cat.modes = selector.modes.clone();
                        }
                        events.push(AcpEvent::SessionReady {
                            thread_id,
                            session_id: sid,
                            afresh,
                            catalog,
                            current_model,
                        });
                    } else {
                        events.push(AcpEvent::SessionFailed {
                            thread_id,
                            turn,
                            message: "The harness gave this thread a session that another thread is using.".to_string(),
                            no_models: false,
                        });
                    }
                }
                _ => events.push(AcpEvent::SessionFailed {
                    thread_id,
                    turn,
                    message: "The harness could not start a thread.".to_string(),
                    no_models: false,
                }),
            },
            PendingRpc::LoadSession {
                thread_id,
                turn,
                session_id,
                cwd,
            } => {
                self.loading_sessions.remove(&session_id);
                match serde_json::from_value::<LoadSessionResponse>(result) {
                    Ok(resp) if self.bind_session(&thread_id, &session_id) => {
                        let mut catalog = resp.config_options.as_deref().and_then(extract_model_catalog);
                        let current_model = catalog.as_ref().and_then(|c| c.default_model.clone());
                        if let Some(ref cid) = catalog.as_ref().and_then(|c| c.config_id.clone()) {
                            self.model_config_id = Some(cid.clone());
                        }
                        if let Some(ref m) = current_model {
                            self.session_models.insert(session_id.clone(), m.clone());
                        }
                        self.take_effort(&session_id, resp.config_options.as_deref().unwrap_or(&[]));
                        let modes = self.take_modes(&session_id, resp.config_options.as_deref());
                        if let (Some(cat), Some(selector)) = (catalog.as_mut(), modes.as_ref()) {
                            cat.modes = selector.modes.clone();
                        }
                        events.push(AcpEvent::SessionReady {
                            thread_id,
                            session_id,
                            afresh: false,
                            catalog,
                            current_model,
                        });
                    }
                    _ => self.request_new_session(&thread_id, turn, &cwd, true),
                }
            }
            PendingRpc::LoadHistory { thread_id, session_id } => {
                let builder = self.history_loads.remove(&session_id);
                match serde_json::from_value::<LoadSessionResponse>(result) {
                    Ok(resp) if self.bind_session(&thread_id, &session_id) => {
                        let mut catalog = resp.config_options.as_deref().and_then(extract_model_catalog);
                        let current_model = catalog.as_ref().and_then(|c| c.default_model.clone());
                        if let Some(ref cid) = catalog.as_ref().and_then(|c| c.config_id.clone()) {
                            self.model_config_id = Some(cid.clone());
                        }
                        if let Some(ref m) = current_model {
                            self.session_models.insert(session_id.clone(), m.clone());
                        }
                        self.take_effort(&session_id, resp.config_options.as_deref().unwrap_or(&[]));
                        let effort = self.session_efforts.get(&session_id).map(|e| e.current.clone());
                        let modes = self.take_modes(&session_id, resp.config_options.as_deref());
                        let current_mode = modes.as_ref().and_then(|s| s.current.clone());
                        if let (Some(cat), Some(selector)) = (catalog.as_mut(), modes.as_ref()) {
                            cat.modes = selector.modes.clone();
                        }
                        let (mut history, tool_ids, subagent_ids) = builder
                            .unwrap_or_else(|| HistoryBuilder::new(self.is_opencode))
                            .finish(current_model.clone(), effort, current_mode);
                        if !tool_ids.is_empty() {
                            self.remember_tool_ids(&thread_id, tool_ids);
                        }
                        if !subagent_ids.is_empty() {
                            for cid in &subagent_ids {
                                let _ = self.register_child(&thread_id, cid, None);
                            }
                        }
                        if let Some(ref title) = history.title {
                            let clean = title.trim();
                            if clean.is_empty() {
                                history.title = None;
                            }
                        }
                        events.push(AcpEvent::HistoryLoaded {
                            thread_id,
                            history,
                            catalog,
                        });
                    }
                    Ok(_) => events.push(AcpEvent::HistoryLoadFailed {
                        thread_id,
                        message: "The session is already in use by another thread.".to_string(),
                    }),
                    Err(_) => events.push(AcpEvent::HistoryLoadFailed {
                        thread_id,
                        message: "The harness returned an invalid session response.".to_string(),
                    }),
                }
            }
            PendingRpc::ListSessions { page, prev_cursor } => {
                match serde_json::from_value::<ListSessionsResponse>(result) {
                    Ok(resp) => {
                        let mut added = 0;
                        for info in &resp.sessions {
                            if let Some(ext) = crate::discovery::session_info_to_external(info)
                                && self.live_sessions_seen.insert(ext.harness_session_id.clone())
                            {
                                self.live_sessions_acc.push(ext);
                                added += 1;
                            }
                        }
                        let next = resp
                            .next_cursor
                            .filter(|c| !c.is_empty() && Some(c) != prev_cursor.as_ref());
                        if added > 0
                            && page + 1 < crate::discovery::MAX_LIST_PAGES
                            && let Some(cursor) = next
                        {
                            let req = ListSessionsRequest::new().cursor(cursor.clone());
                            let id = self.send_request(AGENT_METHOD_NAMES.session_list, &req);
                            self.pending.insert(
                                id,
                                PendingRpc::ListSessions {
                                    page: page + 1,
                                    prev_cursor: Some(cursor),
                                },
                            );
                            let _ = self.on_writable();
                        } else {
                            self.listing_sessions = false;
                            let sessions = std::mem::take(&mut self.live_sessions_acc);
                            self.live_sessions_seen.clear();
                            events.push(AcpEvent::SessionsListed(Ok(sessions)));
                        }
                    }
                    Err(_) => {
                        self.listing_sessions = false;
                        self.live_sessions_acc.clear();
                        self.live_sessions_seen.clear();
                        events.push(AcpEvent::SessionsListed(Err(
                            "The harness returned an invalid session list response.".to_string(),
                        )));
                    }
                }
            }
            PendingRpc::SetModel {
                thread_id,
                turn,
                session_id,
                model,
            } => match serde_json::from_value::<SetSessionConfigOptionResponse>(result) {
                Ok(resp) => {
                    let mut catalog = extract_model_catalog(&resp.config_options);
                    let applied = catalog.as_ref().and_then(|c| c.default_model.clone()).unwrap_or(model);
                    if let Some(ref cid) = catalog.as_ref().and_then(|c| c.config_id.clone()) {
                        self.model_config_id = Some(cid.clone());
                    }
                    if let (Some(cat), Some(selector)) = (
                        catalog.as_mut(),
                        self.take_modes(&session_id, Some(&resp.config_options)),
                    ) {
                        cat.modes = selector.modes;
                    }
                    self.take_effort(&session_id, &resp.config_options);
                    self.session_models.insert(session_id, applied.clone());
                    events.push(AcpEvent::ModelSet {
                        thread_id,
                        turn,
                        model: applied,
                        catalog,
                    });
                }
                Err(_) => events.push(AcpEvent::ModelSetFailed {
                    thread_id,
                    turn,
                    message: "The harness returned an invalid configuration response.".to_string(),
                }),
            },
            PendingRpc::SetMode {
                thread_id,
                turn,
                session_id,
                mode,
            } => match serde_json::from_value::<SetSessionConfigOptionResponse>(result) {
                Ok(resp) => {
                    // What the harness says the mode is now is what it is. A
                    // harness that says nothing of it took what was asked.
                    self.take_effort(&session_id, &resp.config_options);
                    let selector = self.take_modes(&session_id, Some(&resp.config_options));
                    match selector {
                        Some(sel) if sel.current.as_deref() != Some(mode.as_str()) => {
                            events.push(AcpEvent::ModeSetFailed {
                                thread_id,
                                turn,
                                message: "The harness did not take this thread's mode.".to_string(),
                            });
                        }
                        _ => {
                            self.session_modes.insert(session_id, mode);
                            events.push(AcpEvent::ModeSet { thread_id, turn });
                        }
                    }
                }
                Err(_) => events.push(AcpEvent::ModeSetFailed {
                    thread_id,
                    turn,
                    message: "The harness returned an invalid configuration response.".to_string(),
                }),
            },
            PendingRpc::SetEffort {
                thread_id,
                turn,
                session_id,
                effort,
            } => match serde_json::from_value::<SetSessionConfigOptionResponse>(result) {
                Ok(resp) => {
                    // A harness that answers with no error took the effort.
                    // What its answer says the effort is does not decide
                    // it: OpenCode, while another of its sessions works,
                    // answers with the effort the session was at, and says
                    // the new one in an update that follows. That update,
                    // as any does, says what the session is at from then on.
                    self.take_modes(&session_id, Some(&resp.config_options));
                    let told =
                        extract_effort(&resp.config_options).or_else(|| self.session_efforts.get(&session_id).cloned());
                    if let Some(mut selector) = told {
                        selector.current = effort;
                        self.session_efforts.insert(session_id, selector);
                    }
                    events.push(AcpEvent::EffortSet { thread_id, turn });
                }
                Err(_) => events.push(AcpEvent::EffortSetFailed {
                    thread_id,
                    turn,
                    message: "The harness returned an invalid configuration response.".to_string(),
                }),
            },
            PendingRpc::ProbeEffortSession { model } => match serde_json::from_value::<NewSessionResponse>(result) {
                Ok(resp) if !resp.session_id.0.is_empty() => {
                    let options = resp.config_options.as_deref().unwrap_or(&[]);
                    let at_model = model_option(options)
                        .and_then(|(_, select)| (select.current_value.0.as_ref() == model).then_some(()))
                        .is_some();
                    if at_model {
                        let efforts = extract_effort(options).map(|s| s.options).unwrap_or_default();
                        self.probing_efforts.remove(&model);
                        self.close_probe(resp.session_id);
                        events.push(AcpEvent::EffortsProbed {
                            model,
                            result: Ok(efforts),
                        });
                    } else {
                        let config_id = model_option(options)
                            .map(|(option, _)| option.id.0.as_ref().to_string())
                            .or_else(|| self.model_config_id.clone())
                            .unwrap_or_else(|| "model".to_string());
                        let session_id = resp.session_id.0.as_ref().to_string();
                        self.remember_probe_session(session_id.clone());
                        let req = SetSessionConfigOptionRequest::new(
                            resp.session_id,
                            SessionConfigId::new(config_id),
                            model.as_str(),
                        );
                        let id = self.send_request(AGENT_METHOD_NAMES.session_set_config_option, &req);
                        self.pending
                            .insert(id, PendingRpc::ProbeEffortModel { model, session_id });
                        let _ = self.on_writable();
                    }
                }
                _ => {
                    self.probing_efforts.remove(&model);
                    events.push(AcpEvent::EffortsProbed {
                        model,
                        result: Err("The harness returned an invalid session response.".to_string()),
                    });
                }
            },
            PendingRpc::ProbeEffortModel { model, session_id } => {
                self.probing_efforts.remove(&model);
                let result = match serde_json::from_value::<SetSessionConfigOptionResponse>(result) {
                    Ok(resp) => Ok(extract_effort(&resp.config_options)
                        .map(|s| s.options)
                        .unwrap_or_default()),
                    Err(_) => Err("The harness returned an invalid configuration response.".to_string()),
                };
                self.close_probe(SessionId::new(session_id));
                events.push(AcpEvent::EffortsProbed { model, result });
            }
            PendingRpc::Steer { steer, .. } => {
                events.push(AcpEvent::SteerAnswered {
                    steer,
                    taken: steer_taken(&result),
                });
            }
            PendingRpc::Prompt { thread_id, turn } => {
                let transport_failure = self.transport_failures.remove(&thread_id);
                match serde_json::from_value::<PromptResponse>(result) {
                    Ok(resp) => {
                        let outcome = match resp.stop_reason {
                            StopReason::EndTurn => TurnOutcome::Done,
                            StopReason::Cancelled => match transport_failure {
                                Some(message) => TurnOutcome::Failed(message),
                                None => TurnOutcome::Stopped,
                            },
                            StopReason::Refusal => TurnOutcome::Failed("The harness refused to continue.".to_string()),
                            StopReason::MaxTokens => {
                                TurnOutcome::Failed("The turn reached its token limit.".to_string())
                            }
                            StopReason::MaxTurnRequests => {
                                TurnOutcome::Failed("The turn reached its request limit.".to_string())
                            }
                            _ => TurnOutcome::Failed("The turn ended unexpectedly.".to_string()),
                        };
                        events.push(AcpEvent::PromptFinished {
                            thread_id,
                            turn,
                            outcome,
                        });
                    }
                    Err(_) => events.push(AcpEvent::PromptFinished {
                        thread_id,
                        turn,
                        outcome: TurnOutcome::Failed("The harness returned an invalid turn response.".to_string()),
                    }),
                }
            }
        }
    }

    /// Makes `child_sid` a subagent of `thread_id`. Says no, and makes it
    /// nothing, when it is a thread's own session or another thread's
    /// subagent: what it says is then not this thread's.
    #[must_use]
    fn register_child(&mut self, thread_id: &str, child_sid: &str, title: Option<&str>) -> bool {
        if child_sid.is_empty()
            || self.session_to_thread.contains_key(child_sid)
            || self
                .child_to_thread
                .get(child_sid)
                .is_some_and(|owner| owner != thread_id)
        {
            return false;
        }
        self.thread_subagent_ids
            .entry(thread_id.to_string())
            .or_default()
            .insert(child_sid.to_string());
        self.child_to_thread
            .insert(child_sid.to_string(), thread_id.to_string());
        if let Some(clean) = title.map(str::trim).filter(|s| !s.is_empty()) {
            self.child_titles.insert(child_sid.to_string(), clean.to_string());
        }
        true
    }

    /// Takes a tool call that may be OpenCode's subagent launcher, made by
    /// `session_id`: the thread's own session or, for a nested launch, a
    /// subagent's. `title` is the call's title as its session gave it. Lever shows the subagent the call starts, never
    /// the call itself, whose output names the child's session. Says whether
    /// the call was a launcher.
    fn take_launcher_call(
        &mut self,
        thread_id: &str,
        session_id: &str,
        tc: &ToolCall,
        title: &str,
        raw_update: &Value,
        events: &mut Vec<AcpEvent>,
    ) -> bool {
        if !is_opencode_subagent_tool(title, tc.kind, tc.raw_input.as_ref(), raw_update, self.is_opencode) {
            return false;
        }
        let call_id = tc.tool_call_id.0.as_ref().to_string();
        let (title, prompt, child_sid) = extract_subagent_call_fields(tc.raw_input.as_ref(), tc.raw_output.as_ref());
        self.pending_subagent_calls.insert(
            (session_id.to_string(), call_id.clone()),
            PendingSubagentCall {
                thread_id: thread_id.to_string(),
                title,
                prompt,
                child_session_id: child_sid,
            },
        );
        self.bind_launcher(thread_id, session_id, &call_id, None, None, events);
        true
    }

    /// Takes an update of a launcher call `take_launcher_call` kept, made by
    /// `session_id`; `launched_by` is that session when it is a subagent's.
    /// Says whether the update was of one.
    fn take_launcher_update(
        &mut self,
        thread_id: &str,
        session_id: &str,
        launched_by: Option<&str>,
        tcu: &ToolCallUpdate,
        events: &mut Vec<AcpEvent>,
    ) -> bool {
        let call_id = tcu.tool_call_id.0.as_ref().to_string();
        let key = (session_id.to_string(), call_id.clone());
        let Some(call) = self.pending_subagent_calls.get_mut(&key) else {
            return false;
        };
        if call.thread_id != thread_id {
            return true;
        }
        let (new_title, new_prompt, new_child_sid) =
            extract_subagent_call_fields(tcu.fields.raw_input.as_ref(), tcu.fields.raw_output.as_ref());
        let launcher_title = tcu
            .fields
            .title
            .as_deref()
            .map(|t| match launched_by {
                Some(parent) => strip_child_title_prefix(t, self.child_titles.get(parent).map(String::as_str)),
                None => t.to_string(),
            })
            .map(|t| t.trim().to_string())
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("subagent") && !s.eq_ignore_ascii_case("task"));
        if let Some(t) = new_title.or(launcher_title) {
            call.title = Some(t);
        }
        if let Some(p) = new_prompt {
            call.prompt = Some(p);
        }
        // A launcher names one child. One that later names another is not
        // believed: the first child keeps its prompt and its thread.
        if call.child_session_id.is_none() {
            call.child_session_id = new_child_sid;
        }
        // The child it names is taken out of what it says, though the thread
        // has yet to take the child as its own.
        let named_child = call.child_session_id.clone();
        let failed = tcu.fields.status == Some(ToolCallStatus::Failed);
        let error = failed.then(|| {
            tool_detail(
                tcu.fields.content.as_deref().unwrap_or(&[]),
                tcu.fields.raw_output.as_ref(),
                tcu.fields.raw_input.as_ref(),
            )
            .map(|d| {
                let mut ids = self.known_ids(Some(thread_id));
                ids.extend(named_child.as_deref());
                redact_ids(&d, ids)
            })
        });
        let child_known = self.bind_launcher(
            thread_id,
            session_id,
            &call_id,
            failed.then_some("failed"),
            error.clone().flatten(),
            events,
        );
        if matches!(
            tcu.fields.status,
            Some(ToolCallStatus::Completed | ToolCallStatus::Failed)
        ) {
            self.pending_subagent_calls.remove(&key);
            if failed && !child_known {
                let detail = error.flatten();
                match launched_by {
                    None => events.push(AcpEvent::Act {
                        thread_id: thread_id.to_string(),
                        harness_call_id: call_id,
                        is_new: false,
                        kind: Some("other".to_string()),
                        title: Some("Subagent".to_string()),
                        state: Some("failed".to_string()),
                        detail,
                    }),
                    Some(parent) => events.push(AcpEvent::SubagentAct {
                        thread_id: thread_id.to_string(),
                        harness_subagent_id: parent.to_string(),
                        subagent_title: self.child_titles.get(parent).cloned(),
                        harness_call_id: call_id,
                        kind: Some("other".to_string()),
                        title: Some("Subagent".to_string()),
                        state: Some("failed".to_string()),
                        detail,
                    }),
                }
            }
        }
        true
    }

    /// Once a launcher call names its child, makes the child the thread's
    /// subagent and gives it the launcher's prompt, once. `end` is how the
    /// launch ended, when it failed. Says whether the child is known.
    fn bind_launcher(
        &mut self,
        thread_id: &str,
        session_id: &str,
        call_id: &str,
        end: Option<&str>,
        error: Option<String>,
        events: &mut Vec<AcpEvent>,
    ) -> bool {
        let key = (session_id.to_string(), call_id.to_string());
        let Some(call) = self.pending_subagent_calls.get(&key) else {
            return false;
        };
        let Some(cid) = call.child_session_id.clone() else {
            return false;
        };
        // A child that is a thread's own session, or another thread's
        // subagent, is not this launcher's to take.
        if self.session_to_thread.contains_key(&cid)
            || self.child_to_thread.get(&cid).is_some_and(|owner| owner != thread_id)
        {
            return false;
        }
        let title = call.title.clone();
        let prompt = call
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let had_child = self.child_to_thread.contains_key(&cid);
        let title_changed = title.is_some() && self.child_titles.get(&cid) != title.as_ref();
        if !self.register_child(thread_id, &cid, title.as_deref()) {
            return false;
        }
        if self.child_to_thread.contains_key(session_id) {
            self.child_launchers.insert(cid.clone(), session_id.to_string());
        }
        let state = end
            .map(str::to_string)
            .or_else(|| (!had_child).then(|| "working".to_string()));
        if state.is_some() || title_changed {
            events.push(AcpEvent::SubagentUpdated {
                thread_id: thread_id.to_string(),
                harness_subagent_id: cid.clone(),
                title: title.clone(),
                state,
                error,
                from_launcher: true,
            });
        }
        if let Some(text) = prompt
            && !self.child_prompts.contains_key(&cid)
        {
            self.child_prompts.insert(cid.clone(), PromptFrom::Launcher);
            events.push(AcpEvent::SubagentMessage {
                thread_id: thread_id.to_string(),
                harness_subagent_id: cid,
                title,
                role: "prompt".to_string(),
                text,
            });
        }
        true
    }

    /// Translates OpenCode's `opencode/session/child_update` notifications
    /// into Lever's subagent events so no harness-specific method or session
    /// ID crosses Lever's sockets.
    fn handle_opencode_child_update(&mut self, params: Option<&Value>, events: &mut Vec<AcpEvent>) {
        let Some(obj) = params.and_then(Value::as_object) else {
            return;
        };
        let Some(child_sid) = obj
            .get("childSessionId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        let root_sid = obj.get("rootSessionId").and_then(Value::as_str);
        let parent_sid = obj.get("parentSessionId").and_then(Value::as_str);
        let thread_id = self
            .child_to_thread
            .get(child_sid)
            .cloned()
            .or_else(|| root_sid.and_then(|s| self.session_to_thread.get(s).cloned()))
            .or_else(|| {
                parent_sid.and_then(|s| {
                    self.session_to_thread
                        .get(s)
                        .or_else(|| self.child_to_thread.get(s))
                        .cloned()
                })
            });
        let Some(thread_id) = thread_id else {
            return;
        };
        if self.thread_loading(&thread_id) {
            return;
        }
        let title = obj
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if !self.register_child(&thread_id, child_sid, title) {
            return;
        }
        // A child of another of the thread's subagents was launched by it,
        // though the launcher has yet to name it.
        if let Some(parent) = parent_sid
            && parent != child_sid
            && self.child_to_thread.get(parent) == Some(&thread_id)
        {
            self.child_launchers
                .entry(child_sid.to_string())
                .or_insert_with(|| parent.to_string());
        }
        let effective_title = title
            .map(str::to_string)
            .or_else(|| self.child_titles.get(child_sid).cloned());

        match obj.get("type").and_then(Value::as_str) {
            Some("status") => {
                let raw_status = obj.get("status").and_then(Value::as_str);
                let state = match raw_status {
                    Some("created" | "running") => Some("working".to_string()),
                    Some("completed") => Some("done".to_string()),
                    Some("cancelled" | "canceled") => Some("stopped".to_string()),
                    Some("interrupted") => Some("interrupted".to_string()),
                    Some("failed") => Some("failed".to_string()),
                    _ => None,
                };
                let error = if state.as_deref() == Some("failed") {
                    extract_opencode_error(obj.get("error"))
                        .map(|raw| self.redact_for_thread(Some(&thread_id), &raw))
                        .or_else(|| Some("The subagent failed.".to_string()))
                } else {
                    None
                };
                events.push(AcpEvent::SubagentUpdated {
                    thread_id: thread_id.clone(),
                    harness_subagent_id: child_sid.to_string(),
                    title: effective_title.clone(),
                    state,
                    error,
                    from_launcher: false,
                });
            }
            Some("update") => {
                if let Some(raw_update) = obj.get("update") {
                    self.handle_child_session_update(
                        &thread_id,
                        child_sid,
                        effective_title.as_deref(),
                        raw_update,
                        events,
                    );
                }
            }
            _ => {}
        }
    }

    /// Translates an ACP `SessionUpdate` belonging to a child subagent session
    /// into Lever's subagent events.
    fn handle_child_session_update(
        &mut self,
        thread_id: &str,
        child_sid: &str,
        known_title: Option<&str>,
        raw_update: &Value,
        events: &mut Vec<AcpEvent>,
    ) {
        if !self.register_child(thread_id, child_sid, known_title) {
            return;
        }
        let child_title = known_title
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| self.child_titles.get(child_sid).cloned());

        match serde_json::from_value::<SessionUpdate>(raw_update.clone()) {
            Ok(SessionUpdate::AgentMessageChunk(chunk)) => {
                if let Some(text) = content_block_text(&chunk.content) {
                    events.push(AcpEvent::SubagentMessage {
                        thread_id: thread_id.to_string(),
                        harness_subagent_id: child_sid.to_string(),
                        title: child_title,
                        role: "answer".to_string(),
                        text,
                    });
                }
            }
            Ok(SessionUpdate::UserMessageChunk(chunk)) => {
                // The launcher's prompt is the one shown when it came first;
                // otherwise each piece of the stream is.
                let from_launcher = self.child_prompts.get(child_sid) == Some(&PromptFrom::Launcher);
                if !from_launcher && let Some(text) = content_block_text(&chunk.content) {
                    self.child_prompts.insert(child_sid.to_string(), PromptFrom::Stream);
                    events.push(AcpEvent::SubagentMessage {
                        thread_id: thread_id.to_string(),
                        harness_subagent_id: child_sid.to_string(),
                        title: child_title,
                        role: "prompt".to_string(),
                        text,
                    });
                }
            }
            Ok(SessionUpdate::ToolCall(tc)) => {
                let call_id = tc.tool_call_id.0.as_ref().to_string();
                self.thread_tool_ids
                    .entry(thread_id.to_string())
                    .or_default()
                    .insert(call_id.clone());
                let clean_title = strip_child_title_prefix(&tc.title, child_title.as_deref());
                // A subagent that launches a subagent: the launch is kept as
                // the root's is, and the grandchild becomes the thread's.
                if self.take_launcher_call(thread_id, child_sid, &tc, &clean_title, raw_update, events) {
                    return;
                }
                let detail = tool_detail(&tc.content, tc.raw_output.as_ref(), tc.raw_input.as_ref());
                events.push(AcpEvent::SubagentAct {
                    thread_id: thread_id.to_string(),
                    harness_subagent_id: child_sid.to_string(),
                    subagent_title: child_title,
                    harness_call_id: call_id,
                    kind: Some(map_tool_kind(tc.kind).to_string()),
                    title: Some(clean_title),
                    state: Some(map_tool_status(tc.status).to_string()),
                    detail,
                });
            }
            Ok(SessionUpdate::ToolCallUpdate(tcu)) => {
                let call_id = tcu.tool_call_id.0.as_ref().to_string();
                self.thread_tool_ids
                    .entry(thread_id.to_string())
                    .or_default()
                    .insert(call_id.clone());
                if self.take_launcher_update(thread_id, child_sid, Some(child_sid), &tcu, events) {
                    return;
                }
                let detail = tool_detail(
                    tcu.fields.content.as_deref().unwrap_or(&[]),
                    tcu.fields.raw_output.as_ref(),
                    tcu.fields.raw_input.as_ref(),
                );
                let clean_title = tcu
                    .fields
                    .title
                    .map(|t| strip_child_title_prefix(&t, child_title.as_deref()));
                events.push(AcpEvent::SubagentAct {
                    thread_id: thread_id.to_string(),
                    harness_subagent_id: child_sid.to_string(),
                    subagent_title: child_title,
                    harness_call_id: call_id,
                    kind: tcu.fields.kind.map(|k| map_tool_kind(k).to_string()),
                    title: clean_title,
                    state: tcu.fields.status.map(|s| map_tool_status(s).to_string()),
                    detail,
                });
            }
            Ok(SessionUpdate::SessionInfoUpdate(info)) => {
                if let MaybeUndefined::Value(title) = info.title {
                    let clean = title.trim().to_string();
                    if !clean.is_empty() {
                        self.child_titles.insert(child_sid.to_string(), clean.clone());
                        events.push(AcpEvent::SubagentUpdated {
                            thread_id: thread_id.to_string(),
                            harness_subagent_id: child_sid.to_string(),
                            title: Some(clean),
                            state: None,
                            error: None,
                            from_launcher: false,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_notification(&mut self, value: &Value, events: &mut Vec<AcpEvent>) {
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            return;
        };
        if method != CLIENT_METHOD_NAMES.session_update {
            return;
        }
        let Some(params) = value.get("params") else {
            return;
        };
        let Some(session_id) = params.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        let Some(raw_update) = params.get("update") else {
            return;
        };
        // When importing an externally-started session's history, capture the
        // replayed updates into the session's `HistoryBuilder` instead of
        // dropping them or streaming them as live turn events.
        if let Some(builder) = self.history_loads.get_mut(session_id) {
            if let Ok(SessionUpdate::AvailableCommandsUpdate(update)) =
                serde_json::from_value::<SessionUpdate>(raw_update.clone())
            {
                let thread_id = self.pending.values().find_map(|p| match p {
                    PendingRpc::LoadHistory {
                        thread_id,
                        session_id: loading,
                    } if loading == session_id => Some(thread_id.clone()),
                    _ => None,
                });
                if let Some(thread_id) = thread_id {
                    events.push(AcpEvent::Commands {
                        thread_id: Some(thread_id),
                        commands: harness_commands(update.available_commands),
                    });
                }
            } else {
                builder.push_update(raw_update);
            }
            return;
        }
        // Ignore history replayed by the harness while `session/load` is in flight:
        // Lever already owns the authoritative record in SQLite. The session's
        // commands are no history, and are kept, as are those of a session
        // Lever opened to read what the harness offers.
        let loading = self.loading_sessions.contains(session_id);
        if loading || self.probe_sessions.iter().any(|s| s == session_id) {
            let thread_id = if loading { self.loading_thread(session_id) } else { None };
            if (thread_id.is_some() || !loading)
                && let Ok(SessionUpdate::AvailableCommandsUpdate(update)) =
                    serde_json::from_value::<SessionUpdate>(raw_update.clone())
            {
                events.push(AcpEvent::Commands {
                    thread_id,
                    commands: harness_commands(update.available_commands),
                });
            }
            return;
        }
        if let Some(thread_id) = self.child_to_thread.get(session_id).cloned() {
            if !self.thread_loading(&thread_id) {
                self.handle_child_session_update(&thread_id, session_id, None, raw_update, events);
            }
            return;
        }
        let Some(thread_id) = self.session_to_thread.get(session_id).cloned() else {
            return;
        };
        if let Some((child_sid, child_title)) = extract_opencode_child_meta(raw_update) {
            self.handle_child_session_update(&thread_id, &child_sid, child_title.as_deref(), raw_update, events);
            return;
        }

        match serde_json::from_value::<SessionUpdate>(raw_update.clone()) {
            Ok(SessionUpdate::AgentMessageChunk(chunk)) => {
                if let Some(text) = content_block_text(&chunk.content) {
                    events.push(AcpEvent::TextChunk { thread_id, text });
                } else {
                    let plain = self.render_unknown_update(&thread_id, raw_update);
                    events.push(AcpEvent::UnknownUpdate {
                        thread_id,
                        raw_json: raw_update.to_string(),
                        plain_text: plain,
                    });
                }
            }
            Ok(SessionUpdate::ToolCall(tc)) => {
                let call_id = tc.tool_call_id.0.as_ref().to_string();
                self.thread_tool_ids
                    .entry(thread_id.clone())
                    .or_default()
                    .insert(call_id.clone());
                if self.take_launcher_call(&thread_id, session_id, &tc, &tc.title, raw_update, events) {
                    return;
                }
                let detail = tool_detail(&tc.content, tc.raw_output.as_ref(), tc.raw_input.as_ref());
                events.push(AcpEvent::Act {
                    thread_id,
                    harness_call_id: call_id,
                    is_new: true,
                    kind: Some(map_tool_kind(tc.kind).to_string()),
                    title: Some(tc.title),
                    state: Some(map_tool_status(tc.status).to_string()),
                    detail,
                });
            }
            Ok(SessionUpdate::ToolCallUpdate(tcu)) => {
                let call_id = tcu.tool_call_id.0.as_ref().to_string();
                self.thread_tool_ids
                    .entry(thread_id.clone())
                    .or_default()
                    .insert(call_id.clone());
                if self.take_launcher_update(&thread_id, session_id, None, &tcu, events) {
                    return;
                }
                let detail = tool_detail(
                    tcu.fields.content.as_deref().unwrap_or(&[]),
                    tcu.fields.raw_output.as_ref(),
                    tcu.fields.raw_input.as_ref(),
                );
                events.push(AcpEvent::Act {
                    thread_id,
                    harness_call_id: call_id,
                    is_new: false,
                    kind: tcu.fields.kind.map(|k| map_tool_kind(k).to_string()),
                    title: tcu.fields.title,
                    state: tcu.fields.status.map(|s| map_tool_status(s).to_string()),
                    detail,
                });
            }
            Ok(SessionUpdate::SubagentUpdate(sub)) => {
                let child_sid = sub.session_id.0.as_ref().to_string();
                if child_sid.is_empty() {
                    return;
                }
                let title = match sub.title {
                    MaybeUndefined::Value(t) if !t.trim().is_empty() => Some(t.trim().to_string()),
                    _ => match sub.description {
                        MaybeUndefined::Value(d)
                            if !d.trim().is_empty() && !self.child_titles.contains_key(&child_sid) =>
                        {
                            Some(d.trim().to_string())
                        }
                        _ => None,
                    },
                };
                if !self.register_child(&thread_id, &child_sid, title.as_deref()) {
                    return;
                }
                let (state, error) = match sub.state {
                    MaybeUndefined::Value(StateUpdate::Running(_) | StateUpdate::RequiresAction(_)) => {
                        (Some("working".to_string()), None)
                    }
                    MaybeUndefined::Value(StateUpdate::Idle(idle)) => match idle.stop_reason {
                        None | Some(IdleStopReason::EndTurn) => (Some("done".to_string()), None),
                        Some(IdleStopReason::Cancelled) => (Some("stopped".to_string()), None),
                        Some(IdleStopReason::Refusal) => (
                            Some("failed".to_string()),
                            Some("The subagent refused to continue.".to_string()),
                        ),
                        Some(IdleStopReason::MaxTokens) => (
                            Some("failed".to_string()),
                            Some("The subagent reached its token limit.".to_string()),
                        ),
                        Some(IdleStopReason::MaxTurnRequests) => (
                            Some("failed".to_string()),
                            Some("The subagent reached its request limit.".to_string()),
                        ),
                        Some(IdleStopReason::Error(err)) => {
                            let raw = err
                                .error
                                .as_deref()
                                .map(|e| e.message.as_str())
                                .filter(|s| !s.trim().is_empty())
                                .unwrap_or("The subagent failed.");
                            let clean = self.redact_for_thread(Some(&thread_id), raw);
                            (Some("failed".to_string()), Some(clean))
                        }
                        Some(_) => (Some("done".to_string()), None),
                    },
                    _ => (None, None),
                };
                events.push(AcpEvent::SubagentUpdated {
                    thread_id,
                    harness_subagent_id: child_sid,
                    title,
                    state,
                    error,
                    from_launcher: false,
                });
            }
            Ok(SessionUpdate::SessionMessage(msg)) => {
                let text = match msg.content {
                    MaybeUndefined::Value(blocks) => blocks
                        .iter()
                        .filter_map(content_block_text)
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
                if text.is_empty() {
                    return;
                }
                if let MaybeUndefined::Value(rec) = msg.recipient_session_id
                    && self.child_to_thread.get(rec.0.as_ref()).map(String::as_str) == Some(thread_id.as_str())
                {
                    let cid = rec.0.as_ref().to_string();
                    let title = self.child_titles.get(&cid).cloned();
                    events.push(AcpEvent::SubagentMessage {
                        thread_id,
                        harness_subagent_id: cid,
                        title,
                        role: "prompt".to_string(),
                        text,
                    });
                } else if let MaybeUndefined::Value(snd) = msg.sender_session_id
                    && self.child_to_thread.get(snd.0.as_ref()).map(String::as_str) == Some(thread_id.as_str())
                {
                    let cid = snd.0.as_ref().to_string();
                    let title = self.child_titles.get(&cid).cloned();
                    events.push(AcpEvent::SubagentMessage {
                        thread_id,
                        harness_subagent_id: cid,
                        title,
                        role: "answer".to_string(),
                        text,
                    });
                }
            }
            Ok(SessionUpdate::SessionInfoUpdate(info)) => {
                if let MaybeUndefined::Value(title) = info.title {
                    let clean = title.trim().to_string();
                    if !clean.is_empty() {
                        events.push(AcpEvent::TitleUpdated {
                            thread_id: thread_id.clone(),
                            title: clean,
                        });
                    }
                }
                if let Some(raw_msg) = extract_transport_retry_failure(raw_update)
                    && !self.transport_failures.contains_key(&thread_id)
                {
                    let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                    self.transport_failures.insert(thread_id, message);
                    let cancel = CancelNotification::new(SessionId::new(session_id.to_string()));
                    self.send_notification(AGENT_METHOD_NAMES.session_cancel, &cancel);
                    let _ = self.on_writable();
                }
            }
            Ok(SessionUpdate::AvailableCommandsUpdate(update)) => {
                events.push(AcpEvent::Commands {
                    thread_id: Some(thread_id),
                    commands: harness_commands(update.available_commands),
                });
            }
            Ok(SessionUpdate::ConfigOptionUpdate(cfg)) => {
                let before = self.session_modes.get(session_id).cloned();
                let effort_before = self.session_efforts.get(session_id).cloned();
                self.take_effort(session_id, &cfg.config_options);
                let effort_changed = effort_before.as_ref() != self.session_efforts.get(session_id);
                let modes = self.take_modes(session_id, Some(&cfg.config_options));
                let mut catalog = extract_model_catalog(&cfg.config_options);
                let mut current_model = None;
                if let Some(cat) = catalog.as_mut() {
                    if let Some(ref cid) = cat.config_id {
                        self.model_config_id = Some(cid.clone());
                    }
                    current_model = cat.default_model.clone();
                    if let Some(ref m) = current_model {
                        self.session_models.insert(session_id.to_string(), m.clone());
                    }
                    if let Some(selector) = modes.as_ref() {
                        cat.modes = selector.modes.clone();
                    }
                }
                let current_mode = modes.and_then(|m| m.current).filter(|now| before.as_ref() != Some(now));
                if catalog.is_some() || current_mode.is_some() || effort_changed {
                    events.push(AcpEvent::CatalogUpdated {
                        thread_id,
                        catalog,
                        current_model,
                        current_mode,
                        effort_changed,
                    });
                }
            }
            // Known ACP v1 updates that Lever does not show as thread notes:
            // thoughts stream a few tokens at a time, and modes, plans, usage,
            // and notices are either dropped on purpose or handled in their
            // own slices.
            Ok(
                SessionUpdate::UserMessageChunk(_)
                | SessionUpdate::AgentThoughtChunk(_)
                | SessionUpdate::CurrentModeUpdate(_)
                | SessionUpdate::UsageUpdate(_)
                | SessionUpdate::Plan(_)
                | SessionUpdate::PlanUpdate(_)
                | SessionUpdate::PlanRemoved(_)
                | SessionUpdate::Notice(_)
                | SessionUpdate::CompactionUpdate(_)
                | SessionUpdate::CompactionSummaryChunk(_)
                | SessionUpdate::SessionMessageChunk(_),
            ) => {}
            // Any unrecognized or future update is kept as JSON in the record
            // and rendered plainly.
            Ok(_) | Err(_) => {
                let plain = self.render_unknown_update(&thread_id, raw_update);
                events.push(AcpEvent::UnknownUpdate {
                    thread_id,
                    raw_json: raw_update.to_string(),
                    plain_text: plain,
                });
            }
        }
    }

    /// The harness's IDs that Lever knows for a thread, or its session IDs
    /// when no thread is named.
    fn known_ids(&self, thread_id: Option<&str>) -> Vec<&str> {
        let mut ids: Vec<&str> = Vec::new();
        if let Some(tid) = thread_id {
            ids.extend(self.thread_to_session.get(tid).map(String::as_str));
            if let Some(tools) = self.thread_tool_ids.get(tid) {
                ids.extend(tools.iter().map(String::as_str));
            }
            if let Some(subs) = self.thread_subagent_ids.get(tid) {
                ids.extend(subs.iter().map(String::as_str));
            }
        } else {
            ids.extend(self.session_to_thread.keys().map(String::as_str));
            ids.extend(self.child_to_thread.keys().map(String::as_str));
        }
        ids
    }

    /// Redacts harness-internal IDs from error strings and unrecognized updates
    /// without touching normal model answers.
    fn redact_for_thread(&self, thread_id: Option<&str>, input: &str) -> String {
        redact_ids(input, self.known_ids(thread_id))
    }

    /// Takes the tool call IDs of a thread's recorded acts as known, so a
    /// harness that was started again has them taken out of what it says of
    /// acts from before.
    pub fn remember_tool_ids(&mut self, thread_id: &str, ids: Vec<String>) {
        self.thread_tool_ids
            .entry(thread_id.to_string())
            .or_default()
            .extend(ids);
    }

    /// Takes the subagent session IDs of a thread's recorded subagents as
    /// known, so a harness that was started again still has them redacted.
    /// They route nothing: a subagent of a harness that ended is over, and
    /// its ID may be given to another session since.
    pub fn remember_subagent_ids(&mut self, thread_id: &str, ids: Vec<String>) {
        for id in ids {
            if !id.is_empty() {
                self.thread_subagent_ids
                    .entry(thread_id.to_string())
                    .or_default()
                    .insert(id);
            }
        }
    }

    /// Turns an unknown ACP update payload into plain text without exposing
    /// ACP's discriminator tag (`sessionUpdate`), schema field names, metadata,
    /// or harness IDs.
    fn render_unknown_update(&self, thread_id: &str, raw: &Value) -> String {
        let Some(obj) = raw.as_object() else {
            return self.redact_for_thread(Some(thread_id), &raw.to_string());
        };
        // The update's own session and tool call IDs are taken out of what
        // it says too, with the IDs Lever knows, in one pass: an update can
        // name a tool call Lever has seen no act for, and the longest ID
        // must go first whichever kind it is.
        let own_ids: Vec<String> = ["sessionId", "toolCallId"]
            .iter()
            .filter_map(|field| match obj.get(*field) {
                Some(Value::String(id)) => Some(id.clone()),
                Some(Value::Number(id)) => Some(id.to_string()),
                _ => None,
            })
            .collect();
        let mut ids = self.known_ids(Some(thread_id));
        ids.extend(own_ids.iter().map(String::as_str));
        let redact = |said: &str| redact_ids(said, ids.clone());
        for preferred in ["summary", "message", "text", "note", "detail", "title"] {
            if let Some(s) = obj.get(preferred).and_then(Value::as_str)
                && !s.trim().is_empty()
            {
                return redact(s.trim());
            }
        }
        if let Some(content_text) = obj.get("content").and_then(|c| c.get("text")).and_then(Value::as_str)
            && !content_text.trim().is_empty()
        {
            return redact(content_text.trim());
        }
        let mut parts = Vec::new();
        for (k, v) in obj {
            if matches!(
                k.as_str(),
                "sessionUpdate" | "_meta" | "sessionId" | "toolCallId" | "id"
            ) {
                continue;
            }
            match v {
                Value::String(s) if !s.trim().is_empty() => parts.push(s.trim().to_string()),
                Value::Number(n) => parts.push(n.to_string()),
                Value::Bool(b) => parts.push(b.to_string()),
                _ => {}
            }
        }
        if parts.is_empty() {
            "Unrecognized update from harness.".to_string()
        } else {
            redact(&parts.join(" "))
        }
    }
}

/// A prompt's content: desktop context, for the Model's eyes alone so a slash
/// command at the start of the person's text is still one, then the text.
fn prompt_blocks(context: Option<&str>, text: &str) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    if let Some(ctx) = context {
        blocks.push(ContentBlock::Text(
            TextContent::new(format!("{ctx}\n")).annotations(Annotations::new().audience(vec![Role::Assistant])),
        ));
    }
    blocks.push(ContentBlock::from(text.to_string()));
    blocks
}

/// Whether an `initialize` answer's `_meta` says the harness steers, as
/// OpenCode 2.4.1 says it.
fn steering_supported(meta: Option<&serde_json::Map<String, Value>>) -> bool {
    meta.and_then(|m| m.get("steering"))
        .and_then(|s| s.get("supported"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// What a steering answer says: OpenCode answers `injected` for a message it
/// took into the running prompt, and `promptRequired` when no prompt runs.
fn steer_taken(result: &Value) -> Result<(), String> {
    match result.get("outcome").and_then(Value::as_str) {
        Some("injected") => Ok(()),
        Some("promptRequired") => Err(STEER_TOO_LATE.to_string()),
        _ => Err("The Agent did not take your message into its turn. Send it again.".to_string()),
    }
}

/// Builds a sanitized environment for one-shot session discovery commands:
/// never enables free hosted Models, never contacts `models.opencode.ai`,
/// disables self-update, and removes `PWD` so `.current_dir(...)` is honored.
pub fn build_discovery_env(config: &HarnessConfig) -> (Vec<(String, String)>, Vec<&'static str>) {
    let existing_config = config
        .env
        .iter()
        .rev()
        .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
        .map(|(_, v)| v.clone())
        .or_else(|| std::env::var("OPENCODE_CONFIG_CONTENT").ok());
    let mut env_set: Vec<(String, String)> = config
        .env
        .iter()
        .filter(|(k, _)| {
            k != "OPENCODE_CONFIG_CONTENT"
                && k != "OPENCODE_DISABLE_FREE_MODELS"
                && k != "OPENCODE_DISABLE_MODELS_FETCH"
                && k != "PWD"
        })
        .cloned()
        .collect();
    let mut cfg_map = existing_config
        .as_deref()
        .and_then(opencode::parse_jsonc)
        .and_then(|val| val.as_object().cloned())
        .unwrap_or_default();
    cfg_map.insert("update".to_string(), json!("disable"));
    env_set.push((
        "OPENCODE_CONFIG_CONTENT".to_string(),
        Value::Object(cfg_map).to_string(),
    ));
    env_set.push(("OPENCODE_DISABLE_FREE_MODELS".to_string(), "1".to_string()));
    env_set.push(("OPENCODE_DISABLE_MODELS_FETCH".to_string(), "1".to_string()));
    (env_set, vec!["PWD"])
}

/// Accumulates replayed ACP `session/update` notifications during `session/load`
/// for an externally-started session into Lever's harness-agnostic
/// [`SessionHistory`].
struct HistoryBuilder {
    is_opencode: bool,
    title: Option<String>,
    turns: Vec<ImportedTurn>,
    in_user_message: bool,
    last_user_message_id: Option<String>,
    last_agent_message_id: Option<String>,
    tool_ids: Vec<String>,
    subagent_ids: Vec<String>,
    /// Tracks subagent launcher calls by `toolCallId` so their `ToolCallUpdate`
    /// output attaches to the imported subagent instead of creating a raw act.
    launcher_calls: HashMap<String, (usize, usize)>,
    pending_launchers: HashMap<String, (Option<String>, Option<String>)>,
}

impl HistoryBuilder {
    fn new(is_opencode: bool) -> Self {
        Self {
            is_opencode,
            title: None,
            turns: Vec::new(),
            in_user_message: false,
            last_user_message_id: None,
            last_agent_message_id: None,
            tool_ids: Vec::new(),
            subagent_ids: Vec::new(),
            launcher_calls: HashMap::new(),
            pending_launchers: HashMap::new(),
        }
    }

    fn current_turn_mut(&mut self) -> (usize, &mut ImportedTurn) {
        if self.turns.is_empty() {
            self.turns.push(ImportedTurn {
                message: String::new(),
                answer: String::new(),
                acts: Vec::new(),
                notes: Vec::new(),
                subagents: Vec::new(),
                origin: Some("harness".to_string()),
            });
        }
        let idx = self.turns.len() - 1;
        (idx, &mut self.turns[idx])
    }

    fn push_update(&mut self, raw_update: &Value) {
        let msg_id = raw_update.get("messageId").and_then(Value::as_str).map(str::to_string);
        match serde_json::from_value::<SessionUpdate>(raw_update.clone()) {
            Ok(SessionUpdate::UserMessageChunk(chunk)) => {
                if let Some(text) = content_block_text(&chunk.content) {
                    // Two messages in a row, the first left unanswered, are
                    // two turns.
                    let new_message =
                        msg_id.is_some() && self.last_user_message_id.is_some() && msg_id != self.last_user_message_id;
                    if msg_id.is_some() {
                        self.last_user_message_id = msg_id.clone();
                    }
                    if !self.in_user_message || self.turns.is_empty() || new_message {
                        self.turns.push(ImportedTurn {
                            message: text,
                            answer: String::new(),
                            acts: Vec::new(),
                            notes: Vec::new(),
                            subagents: Vec::new(),
                            origin: None,
                        });
                        self.in_user_message = true;
                        self.last_agent_message_id = None;
                    } else if let Some(turn) = self.turns.last_mut() {
                        turn.message.push_str(&text);
                    }
                }
            }
            Ok(SessionUpdate::AgentMessageChunk(chunk)) => {
                self.in_user_message = false;
                if let Some(text) = content_block_text(&chunk.content) {
                    let sep_needed = self.last_agent_message_id.is_some()
                        && msg_id.is_some()
                        && self.last_agent_message_id != msg_id;
                    let (_, turn) = self.current_turn_mut();
                    if sep_needed && !turn.answer.is_empty() && !turn.answer.ends_with('\n') {
                        turn.answer.push_str("\n\n");
                    }
                    turn.answer.push_str(&text);
                    if msg_id.is_some() {
                        self.last_agent_message_id = msg_id;
                    }
                }
            }
            Ok(SessionUpdate::ToolCall(tc)) => {
                self.in_user_message = false;
                let call_id = tc.tool_call_id.0.as_ref().to_string();
                self.tool_ids.push(call_id.clone());
                if is_opencode_subagent_tool(&tc.title, tc.kind, tc.raw_input.as_ref(), raw_update, self.is_opencode) {
                    let (title, prompt, child_sid) =
                        extract_subagent_call_fields(tc.raw_input.as_ref(), tc.raw_output.as_ref());
                    if let Some(cid) = child_sid {
                        self.record_subagent_call(&call_id, cid, title, prompt, tc.status);
                    } else {
                        self.pending_launchers.insert(call_id, (title, prompt));
                    }
                    return;
                }
                let detail = tool_detail(&tc.content, tc.raw_output.as_ref(), tc.raw_input.as_ref());
                let kind = map_tool_kind(tc.kind).to_string();
                let state = map_tool_status(tc.status).to_string();
                let (_, turn) = self.current_turn_mut();
                if let Some(existing) = turn.acts.iter_mut().find(|a| a.harness_call_id == call_id) {
                    existing.kind = kind;
                    existing.title = tc.title;
                    existing.state = state;
                    if detail.is_some() {
                        existing.detail = detail;
                    }
                } else {
                    turn.acts.push(ImportedAct {
                        harness_call_id: call_id,
                        kind,
                        title: tc.title,
                        state,
                        detail,
                    });
                }
            }
            Ok(SessionUpdate::ToolCallUpdate(tcu)) => {
                self.in_user_message = false;
                let call_id = tcu.tool_call_id.0.as_ref().to_string();
                self.tool_ids.push(call_id.clone());
                if let Some((turn_idx, sub_idx)) = self.launcher_calls.get(&call_id).copied() {
                    if let Some(sub) = self.turns.get_mut(turn_idx).and_then(|t| t.subagents.get_mut(sub_idx))
                        && let Some(status) = tcu.fields.status
                    {
                        sub.state = match status {
                            ToolCallStatus::Failed => "failed".to_string(),
                            _ => "done".to_string(),
                        };
                    }
                    return;
                }
                if let Some((prev_title, prev_prompt)) = self.pending_launchers.remove(&call_id) {
                    let (new_title, new_prompt, child_sid) =
                        extract_subagent_call_fields(tcu.fields.raw_input.as_ref(), tcu.fields.raw_output.as_ref());
                    let child_sid = child_sid.or_else(|| {
                        tool_detail(
                            tcu.fields.content.as_deref().unwrap_or(&[]),
                            tcu.fields.raw_output.as_ref(),
                            tcu.fields.raw_input.as_ref(),
                        )
                        .and_then(|d| extract_session_id_from_text(&d))
                    });
                    let title = new_title.or(prev_title);
                    let prompt = new_prompt.or(prev_prompt);
                    if let Some(cid) = child_sid {
                        self.record_subagent_call(
                            &call_id,
                            cid,
                            title,
                            prompt,
                            tcu.fields.status.unwrap_or(ToolCallStatus::Completed),
                        );
                    } else if !matches!(
                        tcu.fields.status,
                        Some(ToolCallStatus::Completed | ToolCallStatus::Failed)
                    ) {
                        self.pending_launchers.insert(call_id, (title, prompt));
                    }
                    return;
                }
                let detail = tool_detail(
                    tcu.fields.content.as_deref().unwrap_or(&[]),
                    tcu.fields.raw_output.as_ref(),
                    tcu.fields.raw_input.as_ref(),
                );
                let kind = tcu.fields.kind.map(|k| map_tool_kind(k).to_string());
                let state = tcu.fields.status.map(|s| map_tool_status(s).to_string());
                let (_, turn) = self.current_turn_mut();
                if let Some(existing) = turn.acts.iter_mut().find(|a| a.harness_call_id == call_id) {
                    if let Some(k) = kind {
                        existing.kind = k;
                    }
                    if let Some(t) = tcu.fields.title {
                        existing.title = t;
                    }
                    if let Some(s) = state {
                        existing.state = s;
                    }
                    if detail.is_some() {
                        existing.detail = detail;
                    }
                } else {
                    turn.acts.push(ImportedAct {
                        harness_call_id: call_id,
                        kind: kind.unwrap_or_else(|| "other".to_string()),
                        title: tcu.fields.title.unwrap_or_else(|| "Act".to_string()),
                        state: state.unwrap_or_else(|| "done".to_string()),
                        detail,
                    });
                }
            }
            Ok(SessionUpdate::SessionInfoUpdate(info)) => {
                if let MaybeUndefined::Value(title) = info.title {
                    let clean = title.trim();
                    if !clean.is_empty() {
                        self.title = Some(clean.to_string());
                    }
                }
            }
            Ok(
                SessionUpdate::AgentThoughtChunk(_)
                | SessionUpdate::CurrentModeUpdate(_)
                | SessionUpdate::UsageUpdate(_)
                | SessionUpdate::Plan(_)
                | SessionUpdate::ConfigOptionUpdate(_)
                | SessionUpdate::AvailableCommandsUpdate(_)
                | SessionUpdate::Notice(_),
            ) => {}
            _ => {}
        }
    }

    fn record_subagent_call(
        &mut self,
        call_id: &str,
        child_sid: String,
        title: Option<String>,
        prompt: Option<String>,
        status: ToolCallStatus,
    ) {
        self.subagent_ids.push(child_sid.clone());
        let state = match status {
            ToolCallStatus::Failed => "failed".to_string(),
            _ => "done".to_string(),
        };
        let (turn_idx, turn) = self.current_turn_mut();
        let sub_idx = turn.subagents.len();
        turn.subagents.push(ImportedSubagent {
            harness_session_id: child_sid,
            title: title
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| "Subagent".to_string()),
            state,
            error: None,
            prompt,
        });
        self.launcher_calls.insert(call_id.to_string(), (turn_idx, sub_idx));
    }

    fn finish(
        mut self,
        model: Option<String>,
        effort: Option<String>,
        mode: Option<String>,
    ) -> (SessionHistory, Vec<String>, Vec<String>) {
        // Normalize any still-pending replayed acts to done since the historical
        // turn already completed before import.
        for turn in &mut self.turns {
            for act in &mut turn.acts {
                if matches!(act.state.as_str(), "pending" | "working") {
                    act.state = "done".to_string();
                }
            }
        }
        (
            SessionHistory {
                title: self.title,
                turns: self.turns,
                model,
                effort,
                mode,
            },
            self.tool_ids,
            self.subagent_ids,
        )
    }
}

/// Extracts a `sessionID: <id>` token from a subagent launcher's text content
/// when `rawOutput` omitted structured metadata.
fn extract_session_id_from_text(text: &str) -> Option<String> {
    let idx = text.find("sessionID:")?;
    let rest = text[idx + "sessionID:".len()..].trim_start();
    let id: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
        .collect();
    (!id.is_empty()).then_some(id)
}

/// Builds the environment overrides Lever passes to a harness child:
/// any `--harness-env` entries, `OPENCODE_CONFIG_CONTENT` with OpenCode's
/// self-update disabled (preserving any keys already present), and
/// `OPENCODE_DISABLE_FREE_MODELS=1` plus `OPENCODE_DISABLE_MODELS_FETCH=1`
/// until the person chooses free hosted Models (so nothing contacts
/// `models.opencode.ai` before the person's choice).
fn build_harness_env(
    extra: &[(String, String)],
    existing_config_content: Option<&str>,
    person: &[Value],
    free_hosted_models: bool,
) -> (Vec<(String, String)>, Vec<&'static str>) {
    let mut env_set: Vec<(String, String)> = extra
        .iter()
        .filter(|(k, _)| {
            k != "OPENCODE_CONFIG_CONTENT"
                && k != "OPENCODE_DISABLE_FREE_MODELS"
                && (free_hosted_models || k != "OPENCODE_DISABLE_MODELS_FETCH")
        })
        .cloned()
        .collect();
    env_set.push((
        "OPENCODE_CONFIG_CONTENT".to_string(),
        opencode_config_content(existing_config_content, person),
    ));
    let mut env_remove = Vec::new();
    if free_hosted_models {
        env_remove.push("OPENCODE_DISABLE_FREE_MODELS");
    } else {
        env_set.push(("OPENCODE_DISABLE_FREE_MODELS".to_string(), "1".to_string()));
        env_set.push(("OPENCODE_DISABLE_MODELS_FETCH".to_string(), "1".to_string()));
    }
    (env_set, env_remove)
}

/// Produces the `OPENCODE_CONFIG_CONTENT` JSON string: OpenCode's
/// self-update switched off, and the Ask first agent, built from OpenCode's
/// own permission settings, added and made the agent a session starts with.
/// Ask first keeps the person's own rules for commands and edits (`person`,
/// their settings documents). It merges into any existing JSON object from
/// the environment, keeping the other agents there, and touches no file in
/// the person's home.
fn opencode_config_content(existing: Option<&str>, person: &[Value]) -> String {
    let mut map = existing
        .and_then(opencode::parse_jsonc)
        .and_then(|val| val.as_object().cloned())
        .unwrap_or_default();
    map.insert("update".to_string(), json!("disable"));
    map.insert("default_agent".to_string(), json!(ASK_FIRST_AGENT));
    let mut agents = map.get("agent").and_then(Value::as_object).cloned().unwrap_or_default();
    agents.insert(
        ASK_FIRST_AGENT.to_string(),
        json!({
            "mode": "primary",
            "description": "Asks before it runs a command or edits a file.",
            "permission": opencode::ask_first_permission(person),
        }),
    );
    map.insert("agent".to_string(), Value::Object(agents));
    Value::Object(map).to_string()
}

/// Reads a session's mode selector: the configuration option of category
/// `mode`, or with the ID `mode`.
fn extract_modes(options: &[SessionConfigOption]) -> Option<ModeSelector> {
    let opt = options
        .iter()
        .find(|o| o.category == Some(SessionConfigOptionCategory::Mode))
        .or_else(|| options.iter().find(|o| o.id.0.as_ref() == "mode"))?;
    let SessionConfigKind::Select(select) = &opt.kind else {
        return None;
    };
    let mut values = Vec::new();
    match &select.options {
        SessionConfigSelectOptions::Ungrouped(items) => {
            values.extend(items.iter().map(|item| item.value.0.as_ref().to_string()));
        }
        SessionConfigSelectOptions::Grouped(groups) => {
            for group in groups {
                values.extend(group.options.iter().map(|item| item.value.0.as_ref().to_string()));
            }
        }
        _ => {}
    }
    let modes: Vec<String> = MODES
        .iter()
        .filter(|(_, value, _)| values.iter().any(|v| v == value))
        .map(|(id, _, _)| (*id).to_string())
        .collect();
    // A selector that offers none of Lever's modes is one Lever leaves alone.
    if modes.is_empty() {
        return None;
    }
    let current_value = select.current_value.0.as_ref().to_string();
    Some(ModeSelector {
        config_id: opt.id.0.as_ref().to_string(),
        modes,
        current: lever_mode(&current_value).map(str::to_string),
        current_value,
    })
}

/// What a request shows the person: the exact command, or the file and the
/// change, and the file's path for an edit.
fn request_detail(
    kind: Option<ToolKind>,
    fields: &agent_client_protocol_schema::v1::ToolCallUpdateFields,
) -> (Option<String>, Option<String>) {
    let content = fields.content.as_deref().unwrap_or(&[]);
    let diffs: Vec<_> = content
        .iter()
        .filter_map(|c| match c {
            ToolCallContent::Diff(diff) => Some(diff),
            _ => None,
        })
        .collect();
    if !diffs.is_empty() {
        let path = diffs[0].path.display().to_string();
        let parts: Vec<String> = diffs
            .iter()
            .map(|d| {
                let body = line_diff(d.old_text.as_deref(), &d.new_text);
                if diffs.len() > 1 {
                    format!("{}\n{body}", d.path.display())
                } else {
                    body
                }
            })
            .collect();
        return (Some(parts.join("\n")), Some(path));
    }
    let input = fields.raw_input.as_ref();
    let text = |name: &str| {
        input
            .and_then(|v| v.get(name))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    // A path or a command is something only when it says something. A text
    // a change takes out or puts in may be empty, as when lines are deleted.
    let field = |name: &str| text(name).filter(|s| !s.is_empty());
    let path = field("path").or_else(|| field("filePath")).or_else(|| {
        fields
            .locations
            .as_ref()
            .and_then(|l| l.first())
            .map(|l| l.path.display().to_string())
    });
    match kind {
        Some(ToolKind::Execute) => (field("command").or_else(|| fields.title.clone()), None),
        Some(ToolKind::Read) => (
            if path.is_some() {
                None
            } else {
                tool_detail(content, None, input)
            },
            path,
        ),
        Some(ToolKind::Edit | ToolKind::Delete | ToolKind::Move) => {
            let change = match (text("oldString"), text("newString"), text("content")) {
                (old, Some(new), _) => Some(line_diff(old.as_deref(), &new)),
                (None, None, Some(content)) => Some(line_diff(None, &content)),
                _ => None,
            };
            (change, path)
        }
        _ => (tool_detail(content, None, input), None),
    }
}

/// The change from `old` to `new` as lines that begin `-` and `+`, with a few
/// unchanged lines around it that begin with a space. Only the part between
/// the lines both share at their start and their end is shown as changed. A
/// long change is cut short in what it takes out and in what it puts in
/// alike, so both show, each with a line that says how much was left.
fn line_diff(old: Option<&str>, new: &str) -> String {
    let old_lines: Vec<&str> = old.map(|o| o.lines().collect()).unwrap_or_default();
    let new_lines: Vec<&str> = new.lines().collect();
    let prefix = old_lines.iter().zip(&new_lines).take_while(|(a, b)| a == b).count();
    let suffix = old_lines[prefix..]
        .iter()
        .rev()
        .zip(new_lines[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let before = &old_lines[prefix.saturating_sub(DIFF_CONTEXT)..prefix];
    let removed = &old_lines[prefix..old_lines.len() - suffix];
    let added = &new_lines[prefix..new_lines.len() - suffix];
    let after = &new_lines[new_lines.len() - suffix..(new_lines.len() - suffix + DIFF_CONTEXT).min(new_lines.len())];

    let budget = MAX_DETAIL_LINES.saturating_sub(before.len() + after.len());
    let (keep_removed, keep_added) = if removed.len() + added.len() <= budget {
        (removed.len(), added.len())
    } else {
        let half = budget / 2;
        let keep_removed = removed.len().min(half.max(budget.saturating_sub(added.len())));
        (keep_removed, added.len().min(budget - keep_removed))
    };
    let mut out: Vec<String> = Vec::new();
    out.extend(before.iter().map(|l| format!(" {l}")));
    out.extend(removed[..keep_removed].iter().map(|l| format!("-{l}")));
    if keep_removed < removed.len() {
        out.push(format!("… {} more lines taken out", removed.len() - keep_removed));
    }
    out.extend(added[..keep_added].iter().map(|l| format!("+{l}")));
    if keep_added < added.len() {
        out.push(format!("… {} more lines put in", added.len() - keep_added));
    }
    out.extend(after.iter().map(|l| format!(" {l}")));
    out.join("\n")
}

/// Whether a JSON-RPC error from `session/new` means the harness has no Models
/// available (for example OpenCode's `CatalogNotReady` when free hosted Models
/// are switched off and no provider is signed in).
fn is_no_models_error(err_val: &Value, message: &str) -> bool {
    let error_name = err_val
        .get("data")
        .and_then(|d| d.get("errorName"))
        .and_then(Value::as_str);
    if error_name == Some("CatalogNotReady") {
        return true;
    }
    message.to_ascii_lowercase().contains("no models are available")
}

/// The Model selector among a session's configuration options: the option of
/// category `model`, or with the ID `model`.
fn model_option(
    options: &[SessionConfigOption],
) -> Option<(
    &SessionConfigOption,
    &agent_client_protocol_schema::v1::SessionConfigSelect,
)> {
    let opt = options
        .iter()
        .find(|o| o.category == Some(SessionConfigOptionCategory::Model))
        .or_else(|| options.iter().find(|o| o.id.0.as_ref() == "model"))?;
    match &opt.kind {
        SessionConfigKind::Select(select) => Some((opt, select)),
        _ => None,
    }
}

/// A Model as the harness lists it with no group: its provider is what its ID
/// has before the first `/`, as OpenCode's IDs do, and its name loses that
/// provider where the harness put it there too.
fn ungrouped_model(id: &str, name: &str) -> ModelOption {
    let provider = id
        .split_once('/')
        .map(|(provider, _)| provider)
        .filter(|p| !p.is_empty());
    let short = provider
        .and_then(|p| name.strip_prefix(p))
        .and_then(|rest| rest.strip_prefix('/'))
        .filter(|rest| !rest.trim().is_empty())
        .unwrap_or(name);
    ModelOption {
        id: id.to_string(),
        name: short.to_string(),
        provider: provider.map(|p| (p.to_string(), opencode::provider_name(p))),
    }
}

/// Extracts the Model selector from ACP session configuration options. A
/// Model's provider is the group the harness lists it under, or else what
/// its ID says.
fn extract_model_catalog(options: &[SessionConfigOption]) -> Option<ModelCatalog> {
    let (opt, select) = model_option(options)?;
    let mut models = Vec::new();
    match &select.options {
        SessionConfigSelectOptions::Ungrouped(items) => {
            for item in items {
                models.push(ungrouped_model(item.value.0.as_ref(), &item.name));
            }
        }
        SessionConfigSelectOptions::Grouped(groups) => {
            for group in groups {
                for item in &group.options {
                    models.push(ModelOption {
                        id: item.value.0.as_ref().to_string(),
                        name: item.name.clone(),
                        provider: Some((group.group.0.as_ref().to_string(), group.name.clone())),
                    });
                }
            }
        }
        _ => {}
    }
    let default_model = (!select.current_value.0.is_empty()).then(|| select.current_value.0.as_ref().to_string());
    Some(ModelCatalog {
        supported: true,
        models,
        default_model,
        config_id: Some(opt.id.0.as_ref().to_string()),
        modes: Vec::new(),
    })
}

/// Reads a session's effort selector: the configuration option of category
/// `thought_level`. A selector with no levels is none.
fn extract_effort(options: &[SessionConfigOption]) -> Option<EffortSelector> {
    let opt = options
        .iter()
        .find(|o| o.category == Some(SessionConfigOptionCategory::ThoughtLevel))?;
    let SessionConfigKind::Select(select) = &opt.kind else {
        return None;
    };
    let mut levels = Vec::new();
    match &select.options {
        SessionConfigSelectOptions::Ungrouped(items) => {
            levels.extend(items.iter().map(|item| EffortOption {
                id: item.value.0.as_ref().to_string(),
                name: item.name.clone(),
            }));
        }
        SessionConfigSelectOptions::Grouped(groups) => {
            for group in groups {
                levels.extend(group.options.iter().map(|item| EffortOption {
                    id: item.value.0.as_ref().to_string(),
                    name: item.name.clone(),
                }));
            }
        }
        _ => {}
    }
    if levels.is_empty() {
        return None;
    }
    Some(EffortSelector {
        config_id: opt.id.0.as_ref().to_string(),
        options: levels,
        current: select.current_value.0.as_ref().to_string(),
    })
}

/// The commands of an ACP `available_commands_update`, as Lever keeps them.
fn harness_commands(commands: Vec<agent_client_protocol_schema::v1::AvailableCommand>) -> Vec<HarnessCommand> {
    commands
        .into_iter()
        .filter(|command| !command.name.trim().is_empty())
        .map(|command| HarnessCommand {
            name: command.name,
            description: command.description,
            hint: match command.input {
                Some(AvailableCommandInput::Unstructured(input)) if !input.hint.trim().is_empty() => Some(input.hint),
                _ => None,
            },
        })
        .collect()
}

fn map_tool_kind(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "read",
        ToolKind::Edit => "edit",
        ToolKind::Delete => "delete",
        ToolKind::Move => "move",
        ToolKind::Search => "search",
        ToolKind::Execute => "command",
        ToolKind::Think => "think",
        ToolKind::Fetch => "fetch",
        ToolKind::SwitchMode => "mode",
        _ => "other",
    }
}

fn map_tool_status(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "working",
        ToolCallStatus::Completed => "done",
        ToolCallStatus::Failed => "failed",
        _ => "pending",
    }
}

fn content_block_text(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(text) => Some(text.text.clone()),
        ContentBlock::ResourceLink(link) => Some(format!("[resource: {}]", link.name)),
        _ => None,
    }
}

fn tool_detail(content: &[ToolCallContent], raw_output: Option<&Value>, raw_input: Option<&Value>) -> Option<String> {
    let mut parts = Vec::new();
    for item in content {
        match item {
            ToolCallContent::Content(c) => {
                if let Some(text) = content_block_text(&c.content)
                    && !text.is_empty()
                {
                    parts.push(text);
                }
            }
            ToolCallContent::Diff(diff) => {
                parts.push(format!("diff {}", diff.path.display()));
            }
            _ => {}
        }
    }
    if !parts.is_empty() {
        return Some(parts.join("\n"));
    }
    raw_output
        .and_then(value_to_detail)
        .or_else(|| raw_input.and_then(value_to_detail))
}

/// What an act's raw input or output says of it in one line: the text itself,
/// or the command or path it names. Anything else stays with the harness: a
/// raw value has the harness's own field names and IDs in it.
fn value_to_detail(val: &Value) -> Option<String> {
    match val {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Object(map) => ["command", "path"]
            .iter()
            .find_map(|field| map.get(*field).and_then(Value::as_str))
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// Puts `[redacted]` where `input` has one of `ids` as a whole word. The
/// longest ID goes first, and an ID inside a longer word or number is left
/// alone, so `call-1` takes nothing from `call-10` and `c1` nothing from
/// `vec1`.
fn redact_ids(input: &str, mut ids: Vec<&str>) -> String {
    ids.retain(|id| !id.is_empty());
    ids.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    ids.dedup();
    let mut out = input.to_string();
    for id in ids {
        let mut redacted = String::with_capacity(out.len());
        let mut from = 0;
        while let Some(found) = out[from..].find(id) {
            // Both neighbors are read from the whole text, so a match that
            // follows another at once still has the character before it.
            let (at, end) = (from + found, from + found + id.len());
            let before = out[..at].chars().next_back();
            let after = out[end..].chars().next();
            let whole = !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric);
            redacted.push_str(&out[from..at]);
            redacted.push_str(if whole { "[redacted]" } else { id });
            from = end;
        }
        redacted.push_str(&out[from..]);
        out = redacted;
    }
    out
}

/// After this many consecutive `provider.transport` retry attempts (for
/// example `ECONNREFUSED` or offline DNS failures), Lever cancels the retry
/// loop and fails the turn with the transport error instead of letting the
/// harness retry indefinitely.
const MAX_TRANSPORT_RETRY_ATTEMPT: u64 = 3;

fn extract_transport_retry_failure(raw_update: &Value) -> Option<String> {
    let retry = raw_update.get("_meta")?.get("opencode/retry")?;
    let attempt = retry.get("attempt")?.as_u64()?;
    if attempt < MAX_TRANSPORT_RETRY_ATTEMPT {
        return None;
    }
    let err = retry.get("error")?;
    if err.get("type").and_then(Value::as_str) != Some("provider.transport") {
        return None;
    }
    let msg = err
        .get("message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Could not reach the Model.");
    Some(msg.to_string())
}

/// Extracts `(child_session_id, optional_title)` from OpenCode's
/// `_meta["opencode/child-session"]` metadata when present on a tool call,
/// permission request, or update.
fn extract_opencode_child_meta(val: &Value) -> Option<(String, Option<String>)> {
    let meta = val.get("_meta")?.get("opencode/child-session")?;
    let id = meta
        .get("id")
        .or_else(|| meta.get("sessionId"))
        .or_else(|| meta.get("childSessionId"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    let title = meta
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Some((id.to_string(), title))
}

/// Strips OpenCode's `"<childTitle>: "` prefix from child act and request
/// titles so each subagent's acts read cleanly inside its own section.
fn strip_child_title_prefix(title: &str, child_title: Option<&str>) -> String {
    if let Some(prefix) = child_title.map(str::trim).filter(|s| !s.is_empty())
        && let Some(rest) = title.strip_prefix(prefix)
        && let Some(after_colon) = rest.strip_prefix(": ")
        && !after_colon.trim().is_empty()
    {
        return after_colon.to_string();
    }
    title.to_string()
}

/// Extracts a human-readable error message from OpenCode's child status error
/// payload, which may be a plain string or a structured object.
fn extract_opencode_error(error: Option<&Value>) -> Option<String> {
    let val = error?;
    match val {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Object(map) => ["message", "error", "detail"]
            .iter()
            .find_map(|k| map.get(*k).and_then(Value::as_str))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// Whether a session's tool call is OpenCode's subagent launcher tool
/// (`subagent`, or with `opencode`, the harness being OpenCode, its legacy
/// `task`). Lever keeps the subagent itself in the thread record instead of
/// exposing OpenCode's wrapper call and raw `sessionID`. Another harness's
/// act called `task` is its own act.
fn is_opencode_subagent_tool(
    title: &str,
    kind: ToolKind,
    raw_input: Option<&Value>,
    raw_update: &Value,
    opencode: bool,
) -> bool {
    let named = |pointer: &str| {
        matches!(
            raw_update.pointer(pointer).and_then(Value::as_str),
            Some("subagent" | "task")
        )
    };
    if named("/_meta/opencode/toolName") || (opencode && named("/_meta/toolName")) {
        return true;
    }
    let clean = title.trim();
    if clean.eq_ignore_ascii_case("subagent") {
        return true;
    }
    opencode
        && clean.eq_ignore_ascii_case("task")
        && (kind == ToolKind::Think
            || raw_input.is_some_and(|v| {
                v.get("prompt").is_some() && (v.get("agent").is_some() || v.get("subagent_type").is_some())
            }))
}

/// Extracts `(title, prompt, child_session_id)` from an OpenCode subagent tool
/// call's `rawInput` and `rawOutput`.
fn extract_subagent_call_fields(
    raw_input: Option<&Value>,
    raw_output: Option<&Value>,
) -> (Option<String>, Option<String>, Option<String>) {
    let non_empty = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let title = raw_input.and_then(|v| non_empty(v.get("description")));
    let prompt = raw_input.and_then(|v| non_empty(v.get("prompt")));
    let child_sid = raw_output
        .and_then(|v| {
            non_empty(v.pointer("/metadata/sessionID"))
                .or_else(|| non_empty(v.pointer("/metadata/sessionId")))
                .or_else(|| non_empty(v.get("sessionID")))
                .or_else(|| non_empty(v.get("sessionId")))
        })
        .or_else(|| {
            raw_input.and_then(|v| {
                non_empty(v.get("sessionID"))
                    .or_else(|| non_empty(v.get("sessionId")))
                    .or_else(|| non_empty(v.get("session_id")))
                    .or_else(|| non_empty(v.get("task_id")))
            })
        });
    (title, prompt, child_sid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_id_is_redacted_as_a_whole_word_and_the_longest_first() {
        assert_eq!(
            redact_ids("failed on call-10 after call-1", vec!["call-1", "call-10"]),
            "failed on [redacted] after [redacted]"
        );
        assert_eq!(redact_ids("vec1 failed in c1", vec!["c1"]), "vec1 failed in [redacted]");
        assert_eq!(
            redact_ids("failed after 10 seconds", vec!["1"]),
            "failed after 10 seconds"
        );
        assert_eq!(
            redact_ids("session s-9: s-9.", vec!["s-9", ""]),
            "session [redacted]: [redacted]."
        );
        // An ID that follows itself at once is inside a longer word both times.
        assert_eq!(redact_ids("c1c1 and ac1c1", vec!["c1"]), "c1c1 and ac1c1");
        assert_eq!(redact_ids("c1 c1", vec!["c1"]), "[redacted] [redacted]");
    }

    #[test]
    fn a_raw_value_gives_an_act_only_its_text_command_or_path() {
        assert_eq!(value_to_detail(&json!("plain")), Some("plain".to_string()));
        assert_eq!(
            value_to_detail(&json!({ "command": "ls -l", "id": "x" })),
            Some("ls -l".to_string())
        );
        assert_eq!(
            value_to_detail(&json!({ "path": "/tmp/a" })),
            Some("/tmp/a".to_string())
        );
        assert_eq!(value_to_detail(&json!({ "sessionId": "s-1", "exit": 0 })), None);
        assert_eq!(value_to_detail(&json!([{ "toolCallId": "c-1" }])), None);
        assert_eq!(value_to_detail(&json!(7)), None);
        assert_eq!(value_to_detail(&Value::Null), None);
    }

    #[test]
    fn an_act_with_no_text_says_what_its_output_or_else_its_input_names() {
        let (input, output) = (json!({ "command": "cargo test" }), json!({ "exit": 0 }));
        assert_eq!(
            tool_detail(&[], Some(&output), Some(&input)),
            Some("cargo test".to_string())
        );
        assert_eq!(
            tool_detail(&[], Some(&json!("3 passed")), Some(&input)),
            Some("3 passed".to_string())
        );
        assert_eq!(tool_detail(&[], Some(&output), None), None);
    }

    #[test]
    fn opencode_config_content_disables_self_update_adds_ask_first_and_preserves_other_keys() {
        let ask_first = json!({
            "mode": "primary",
            "description": "Asks before it runs a command or edits a file.",
            "permission": { "bash": { "*": "ask" }, "edit": { "*": "ask" } },
        });
        assert_eq!(
            serde_json::from_str::<Value>(&opencode_config_content(None, &[])).unwrap(),
            json!({ "update": "disable", "default_agent": ASK_FIRST_AGENT, "agent": { ASK_FIRST_AGENT: ask_first } })
        );
        assert_eq!(
            serde_json::from_str::<Value>(&opencode_config_content(
                Some(r#"{"model":"stub/stub-1","update":"auto","agent":{"mine":{"mode":"primary"}}}"#),
                &[]
            ))
            .unwrap(),
            json!({
                "model": "stub/stub-1",
                "update": "disable",
                "default_agent": ASK_FIRST_AGENT,
                "agent": { "mine": { "mode": "primary" }, ASK_FIRST_AGENT: ask_first },
            })
        );
        assert_eq!(
            serde_json::from_str::<Value>(&opencode_config_content(Some("not-json"), &[])).unwrap()["update"],
            json!("disable")
        );
    }

    #[test]
    fn harness_env_toggles_free_hosted_models_without_losing_extra_env() {
        // Without extra env, free_hosted_models=false also disables models.opencode.ai fetch.
        let (default_off, _) = build_harness_env(&[], None, &[], false);
        assert!(default_off.contains(&("OPENCODE_DISABLE_FREE_MODELS".into(), "1".into())));
        assert!(default_off.contains(&("OPENCODE_DISABLE_MODELS_FETCH".into(), "1".into())));

        let (default_on, rm_default_on) = build_harness_env(&[], None, &[], true);
        assert!(!default_on.iter().any(|(k, _)| k == "OPENCODE_DISABLE_FREE_MODELS"));
        assert!(!default_on.iter().any(|(k, _)| k == "OPENCODE_DISABLE_MODELS_FETCH"));
        assert_eq!(rm_default_on, vec!["OPENCODE_DISABLE_FREE_MODELS"]);

        // Even an explicit `=0` cannot re-enable models.opencode.ai fetch while free Models are off.
        let override_zero = vec![("OPENCODE_DISABLE_MODELS_FETCH".to_string(), "0".to_string())];
        let (forced_off, _) = build_harness_env(&override_zero, None, &[], false);
        assert!(forced_off.contains(&("OPENCODE_DISABLE_MODELS_FETCH".into(), "1".into())));
        assert!(!forced_off.contains(&("OPENCODE_DISABLE_MODELS_FETCH".into(), "0".into())));

        // Explicit --harness-env OPENCODE_DISABLE_MODELS_FETCH=1 is preserved in both states.
        let extra = vec![("OPENCODE_DISABLE_MODELS_FETCH".to_string(), "1".to_string())];
        let (set_off, rm_off) = build_harness_env(&extra, None, &[], false);
        assert_eq!(
            set_off
                .iter()
                .filter(|(k, _)| k == "OPENCODE_DISABLE_MODELS_FETCH")
                .count(),
            1
        );
        assert!(set_off.contains(&("OPENCODE_DISABLE_FREE_MODELS".into(), "1".into())));
        assert!(
            set_off
                .iter()
                .any(|(k, v)| k == "OPENCODE_CONFIG_CONTENT" && v.contains("disable"))
        );
        assert!(rm_off.is_empty());

        let (set_on, rm_on) = build_harness_env(&extra, None, &[], true);
        assert!(set_on.contains(&("OPENCODE_DISABLE_MODELS_FETCH".into(), "1".into())));
        assert!(!set_on.iter().any(|(k, _)| k == "OPENCODE_DISABLE_FREE_MODELS"));
        assert_eq!(rm_on, vec!["OPENCODE_DISABLE_FREE_MODELS"]);
    }

    #[test]
    fn no_models_error_is_recognized_by_error_name_or_message() {
        assert!(is_no_models_error(
            &json!({ "code": -32603, "message": "Internal error", "data": { "errorName": "CatalogNotReady" } }),
            "Internal error"
        ));
        assert!(is_no_models_error(
            &json!({ "code": -32603, "message": "Internal error: No models are available" }),
            "Internal error: No models are available"
        ));
        assert!(!is_no_models_error(
            &json!({ "code": -32603, "message": "Timeout" }),
            "Timeout"
        ));
    }

    #[test]
    fn a_change_shows_as_the_lines_it_removes_and_adds_with_a_few_around_it() {
        assert_eq!(
            line_diff(Some("alpha line\n"), "beta line\n"),
            "-alpha line\n+beta line"
        );
        assert_eq!(line_diff(None, "hello\nworld\n"), "+hello\n+world");
        let old = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
        let new = "1\n2\n3\n4\nfive\n6\n7\n8\n9\n";
        assert_eq!(line_diff(Some(old), new), " 2\n 3\n 4\n-5\n+five\n 6\n 7\n 8");
        let long: String = (0..1000).map(|n| format!("{n}\n")).collect();
        let shown = line_diff(None, &long);
        assert_eq!(shown.lines().count(), MAX_DETAIL_LINES + 1);
        assert!(shown.ends_with("… 600 more lines put in"));

        // A long replacement shows what it takes out and what it puts in.
        let old: String = (0..450).map(|n| format!("old {n}\n")).collect();
        let new: String = (0..450).map(|n| format!("new {n}\n")).collect();
        let shown = line_diff(Some(&old), &new);
        let lines: Vec<&str> = shown.lines().collect();
        assert_eq!(lines.len(), MAX_DETAIL_LINES + 2);
        assert_eq!(lines[0], "-old 0");
        assert!(lines.contains(&"… 250 more lines taken out"), "{shown}");
        assert!(lines.contains(&"+new 0"), "{shown}");
        assert_eq!(lines.last(), Some(&"… 250 more lines put in"));
    }

    #[test]
    fn a_models_provider_is_its_group_or_what_its_id_says() {
        // As OpenCode lists them: no groups, the provider in the ID and at
        // the front of the name.
        let ungrouped: Vec<SessionConfigOption> = serde_json::from_value(json!([
            { "id": "model", "name": "Model", "category": "model", "type": "select",
              "currentValue": "opencode/big-pickle",
              "options": [
                  { "value": "opencode/big-pickle", "name": "opencode/Big Pickle" },
                  { "value": "openrouter/meta/llama", "name": "openrouter/Llama" },
                  { "value": "acme/one", "name": "One of Acme's" },
                  { "value": "plain", "name": "Plain" }
              ] }
        ]))
        .unwrap();
        let catalog = extract_model_catalog(&ungrouped).unwrap();
        let model = |id: &str, name: &str, provider: Option<(&str, &str)>| ModelOption {
            id: id.into(),
            name: name.into(),
            provider: provider.map(|(id, name)| (id.into(), name.into())),
        };
        assert_eq!(
            catalog.models,
            [
                model("opencode/big-pickle", "Big Pickle", Some(("opencode", "OpenCode"))),
                model("openrouter/meta/llama", "Llama", Some(("openrouter", "OpenRouter"))),
                model("acme/one", "One of Acme's", Some(("acme", "acme"))),
                model("plain", "Plain", None),
            ]
        );
        assert_eq!(catalog.default_model.as_deref(), Some("opencode/big-pickle"));

        // A harness that groups its Models says the provider itself.
        let grouped: Vec<SessionConfigOption> = serde_json::from_value(json!([
            { "id": "model", "name": "Model", "category": "model", "type": "select",
              "currentValue": "m-1",
              "options": [
                  { "group": "acme", "name": "Acme AI", "options": [{ "value": "m-1", "name": "One" }] },
                  { "group": "other", "name": "Other", "options": [{ "value": "o/2", "name": "Two" }] }
              ] }
        ]))
        .unwrap();
        let catalog = extract_model_catalog(&grouped).unwrap();
        assert_eq!(
            catalog.models,
            [
                ModelOption {
                    id: "m-1".into(),
                    name: "One".into(),
                    provider: Some(("acme".into(), "Acme AI".into())),
                },
                ModelOption {
                    id: "o/2".into(),
                    name: "Two".into(),
                    provider: Some(("other".into(), "Other".into())),
                },
            ]
        );
    }

    #[test]
    fn effort_is_the_thought_level_option_and_a_selector_with_no_levels_is_none() {
        let options: Vec<SessionConfigOption> = serde_json::from_value(json!([
            { "id": "model", "name": "Model", "category": "model", "type": "select",
              "currentValue": "a/b", "options": [{ "value": "a/b", "name": "a/B" }] },
            { "id": "effort", "name": "Effort", "category": "thought_level", "type": "select",
              "currentValue": "default",
              "options": [{ "value": "high", "name": "High" }, { "value": "default", "name": "Default" }] }
        ]))
        .unwrap();
        let selector = extract_effort(&options).unwrap();
        assert_eq!(selector.current, "default");
        assert!(selector.offers("high") && !selector.offers("max"));
        assert_eq!(
            selector.options,
            [
                EffortOption {
                    id: "high".into(),
                    name: "High".into()
                },
                EffortOption {
                    id: "default".into(),
                    name: "Default".into()
                },
            ]
        );
        // An option that only happens to be named `effort` is not one.
        let unmarked: Vec<SessionConfigOption> = serde_json::from_value(json!([
            { "id": "effort", "name": "Effort", "type": "select", "currentValue": "x",
              "options": [{ "value": "x", "name": "X" }] },
            { "id": "level", "name": "Level", "category": "thought_level", "type": "select",
              "currentValue": "", "options": [] }
        ]))
        .unwrap();
        assert_eq!(extract_effort(&unmarked), None);
    }

    #[test]
    fn a_command_keeps_its_name_what_the_harness_says_of_it_and_its_hint() {
        let update: agent_client_protocol_schema::v1::AvailableCommandsUpdate = serde_json::from_value(json!({
            "availableCommands": [
                { "name": "review", "description": "review changes", "input": { "hint": "commit, branch, or pr" } },
                { "name": "compact", "description": "Compact the session" },
                { "name": " ", "description": "a command with no name" },
                { "name": "init", "description": "", "input": { "hint": " " } }
            ]
        }))
        .unwrap();
        assert_eq!(
            harness_commands(update.available_commands),
            [
                HarnessCommand {
                    name: "review".into(),
                    description: "review changes".into(),
                    hint: Some("commit, branch, or pr".into()),
                },
                HarnessCommand {
                    name: "compact".into(),
                    description: "Compact the session".into(),
                    hint: None,
                },
                HarnessCommand {
                    name: "init".into(),
                    description: String::new(),
                    hint: None,
                },
            ]
        );
    }

    #[test]
    fn modes_are_lever_s_own_and_only_those_the_harness_reports() {
        let options: Vec<SessionConfigOption> = serde_json::from_value(json!([
            { "id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "m",
              "options": [{ "value": "m", "name": "M" }] },
            { "id": "mode", "name": "Session Mode", "category": "mode", "type": "select",
              "currentValue": "crowbar-ask-first",
              "options": [
                { "value": "crowbar-ask-first", "name": "crowbar-ask-first" },
                { "value": "build", "name": "Build" },
                { "value": "plan", "name": "Plan" },
                { "value": "persons-own", "name": "Theirs" }
              ] }
        ]))
        .unwrap();
        let selector = extract_modes(&options).unwrap();
        assert_eq!(selector.config_id, "mode");
        assert_eq!(selector.modes, vec!["ask-first", "full-access", "plan"]);
        assert_eq!(selector.current.as_deref(), Some("ask-first"));

        let build_only: Vec<SessionConfigOption> = serde_json::from_value(json!([
            { "id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": "persons-own",
              "options": [{ "value": "build", "name": "Build" }, { "value": "persons-own", "name": "Theirs" }] }
        ]))
        .unwrap();
        let selector = extract_modes(&build_only).unwrap();
        assert_eq!(selector.modes, vec!["full-access"]);
        assert_eq!(selector.current, None);

        // A harness whose modes are none of Lever's has none Lever sets.
        let others: Vec<SessionConfigOption> = serde_json::from_value(json!([
            { "id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": "default",
              "options": [{ "value": "default", "name": "Default" }, { "value": "yolo", "name": "Yolo" }] }
        ]))
        .unwrap();
        assert_eq!(extract_modes(&others), None);
    }

    #[test]
    fn a_request_shows_the_exact_command_or_the_file_and_the_change() {
        let command: agent_client_protocol_schema::v1::ToolCallUpdateFields = serde_json::from_value(json!({
            "kind": "execute", "title": "echo hi",
            "rawInput": { "command": "echo hi && id -un", "cwd": "/tmp" }
        }))
        .unwrap();
        assert_eq!(
            request_detail(Some(ToolKind::Execute), &command),
            (Some("echo hi && id -un".to_string()), None)
        );
        let edit: agent_client_protocol_schema::v1::ToolCallUpdateFields = serde_json::from_value(json!({
            "kind": "edit", "title": "/w/seed.txt",
            "rawInput": { "path": "/w/seed.txt", "oldString": "alpha", "newString": "beta" },
            "content": [{ "type": "diff", "path": "/w/seed.txt", "oldText": "alpha line\n", "newText": "beta line\n" }]
        }))
        .unwrap();
        assert_eq!(
            request_detail(Some(ToolKind::Edit), &edit),
            (
                Some("-alpha line\n+beta line".to_string()),
                Some("/w/seed.txt".to_string())
            )
        );
        let raw_edit: agent_client_protocol_schema::v1::ToolCallUpdateFields = serde_json::from_value(json!({
            "kind": "edit", "rawInput": { "filePath": "/w/new.txt", "content": "hello\n" }
        }))
        .unwrap();
        assert_eq!(
            request_detail(Some(ToolKind::Edit), &raw_edit),
            (Some("+hello".to_string()), Some("/w/new.txt".to_string()))
        );
        // Lines taken out, with nothing put in their place, are the change.
        let deletion: agent_client_protocol_schema::v1::ToolCallUpdateFields = serde_json::from_value(json!({
            "kind": "edit", "rawInput": { "filePath": "/w/old.txt", "oldString": "remove me\n", "newString": "" }
        }))
        .unwrap();
        assert_eq!(
            request_detail(Some(ToolKind::Edit), &deletion),
            (Some("-remove me".to_string()), Some("/w/old.txt".to_string()))
        );
    }

    #[test]
    fn transport_retry_failure_triggers_only_for_repeated_transport_errors() {
        let early = json!({
            "sessionUpdate": "session_info_update",
            "_meta": {
                "opencode/retry": {
                    "attempt": 2,
                    "error": { "type": "provider.transport", "message": "ECONNREFUSED: Unable to connect." }
                }
            }
        });
        assert_eq!(extract_transport_retry_failure(&early), None);

        let rate_limit = json!({
            "sessionUpdate": "session_info_update",
            "_meta": {
                "opencode/retry": {
                    "attempt": 4,
                    "error": { "type": "provider.rate_limit", "message": "429 Too Many Requests" }
                }
            }
        });
        assert_eq!(extract_transport_retry_failure(&rate_limit), None);

        let unreachable = json!({
            "sessionUpdate": "session_info_update",
            "_meta": {
                "opencode/retry": {
                    "attempt": 3,
                    "error": { "type": "provider.transport", "message": "ECONNREFUSED: Unable to connect." }
                }
            }
        });
        assert_eq!(
            extract_transport_retry_failure(&unreachable).as_deref(),
            Some("ECONNREFUSED: Unable to connect.")
        );
    }

    #[test]
    fn child_title_prefix_and_opencode_subagent_metadata_are_normalized() {
        assert_eq!(
            strip_child_title_prefix("Inspect files: Read .env", Some("Inspect files")),
            "Read .env"
        );
        assert_eq!(
            strip_child_title_prefix("Read .env", Some("Inspect files")),
            "Read .env"
        );
        assert_eq!(
            strip_child_title_prefix("Inspect files: ", Some("Inspect files")),
            "Inspect files: "
        );

        let meta_val = json!({
            "_meta": {
                "opencode/child-session": {
                    "id": "ses_child_1",
                    "parentID": "ses_root_1",
                    "depth": 1,
                    "title": "Inspect files"
                }
            }
        });
        assert_eq!(
            extract_opencode_child_meta(&meta_val),
            Some(("ses_child_1".to_string(), Some("Inspect files".to_string())))
        );

        let input = json!({
            "description": "Inspect files",
            "prompt": "Read .env and report",
            "agent": "general"
        });
        let output = json!({
            "metadata": {
                "sessionID": "ses_child_1",
                "status": "running"
            }
        });
        assert!(is_opencode_subagent_tool(
            "subagent",
            ToolKind::Think,
            Some(&input),
            &json!({}),
            false
        ));
        // OpenCode's legacy `task` launcher is OpenCode's only: another
        // harness's `Task` act stays an act.
        assert!(is_opencode_subagent_tool(
            "task",
            ToolKind::Think,
            None,
            &json!({}),
            true
        ));
        assert!(!is_opencode_subagent_tool(
            "Task",
            ToolKind::Think,
            None,
            &json!({}),
            false
        ));
        assert!(!is_opencode_subagent_tool(
            "task",
            ToolKind::Think,
            Some(&input),
            &json!({ "_meta": { "toolName": "task" } }),
            false
        ));
        assert_eq!(
            extract_subagent_call_fields(Some(&input), Some(&output)),
            (
                Some("Inspect files".to_string()),
                Some("Read .env and report".to_string()),
                Some("ses_child_1".to_string()),
            )
        );
        assert_eq!(
            extract_opencode_error(Some(&json!({ "message": "boom" }))).as_deref(),
            Some("boom")
        );
    }

    #[test]
    fn history_builder_keeps_consecutive_user_messages_as_their_own_turns() {
        let mut b = HistoryBuilder::new(true);
        for (id, text) in [("u1", "First "), ("u1", "part"), ("u2", "Second")] {
            b.push_update(&json!({
                "sessionUpdate": "user_message_chunk",
                "messageId": id,
                "content": { "type": "text", "text": text }
            }));
        }
        b.push_update(&json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "a1",
            "content": { "type": "text", "text": "Answer" }
        }));
        let turns = b.turns;
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].message, "First part");
        assert_eq!(turns[0].answer, "");
        assert_eq!(turns[1].message, "Second");
        assert_eq!(turns[1].answer, "Answer");
    }

    #[test]
    fn history_builder_reconstructs_multi_turn_replay_with_acts_and_subagents() {
        let mut b = HistoryBuilder::new(true);
        b.push_update(&json!({
            "sessionUpdate": "user_message_chunk",
            "content": { "type": "text", "text": "Hello " }
        }));
        b.push_update(&json!({
            "sessionUpdate": "user_message_chunk",
            "content": { "type": "text", "text": "world" }
        }));
        b.push_update(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_1",
            "title": "echo hi",
            "kind": "execute",
            "status": "pending",
            "rawInput": { "command": "echo hi" }
        }));
        b.push_update(&json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_1",
            "status": "completed",
            "content": [{ "type": "content", "content": { "type": "text", "text": "hi\n" } }]
        }));
        b.push_update(&json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "m1",
            "content": { "type": "text", "text": "Done: hi" }
        }));
        b.push_update(&json!({
            "sessionUpdate": "user_message_chunk",
            "content": { "type": "text", "text": "Spawn helper" }
        }));
        b.push_update(&json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call_sub",
            "name": "subagent",
            "title": "subagent",
            "kind": "think",
            "status": "pending",
            "rawInput": { "description": "Child helper", "prompt": "Check logs" }
        }));
        b.push_update(&json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": "call_sub",
            "status": "completed",
            "content": [{ "type": "content", "content": { "type": "text", "text": "Working (sessionID: ses_child_1)" } }]
        }));
        b.push_update(&json!({
            "sessionUpdate": "agent_message_chunk",
            "messageId": "m2",
            "content": { "type": "text", "text": "Helper finished." }
        }));
        b.push_update(&json!({
            "sessionUpdate": "session_info_update",
            "title": "Terminal session"
        }));

        let (history, tool_ids, sub_ids) = b.finish(Some("stub/stub-1".into()), Some("default".into()), None);
        assert_eq!(history.title.as_deref(), Some("Terminal session"));
        assert_eq!(history.model.as_deref(), Some("stub/stub-1"));
        assert_eq!(history.turns.len(), 2);
        assert_eq!(history.turns[0].message, "Hello world");
        assert_eq!(history.turns[0].answer, "Done: hi");
        assert_eq!(history.turns[0].acts.len(), 1);
        assert_eq!(history.turns[0].acts[0].title, "echo hi");
        assert_eq!(history.turns[0].acts[0].state, "done");
        assert_eq!(history.turns[0].acts[0].detail.as_deref(), Some("hi\n"));
        assert_eq!(history.turns[1].message, "Spawn helper");
        assert_eq!(history.turns[1].answer, "Helper finished.");
        assert!(history.turns[1].acts.is_empty(), "subagent launcher is not a raw act");
        assert_eq!(history.turns[1].subagents.len(), 1);
        assert_eq!(history.turns[1].subagents[0].harness_session_id, "ses_child_1");
        assert_eq!(history.turns[1].subagents[0].title, "Child helper");
        assert_eq!(history.turns[1].subagents[0].prompt.as_deref(), Some("Check logs"));
        assert!(tool_ids.contains(&"call_1".to_string()));
        assert_eq!(sub_ids, vec!["ses_child_1".to_string()]);
    }
}
