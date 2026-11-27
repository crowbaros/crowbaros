//! A scripted ACP v1 stub harness for Lever's and `crowbar-agent`'s tests.
//!
//! Reads newline-delimited JSON-RPC 2.0 messages on standard input and writes
//! responses and `session/update` notifications on standard output, using the
//! official `agent-client-protocol-schema` v1 types for known ACP messages.
//!
//! A session's ID is `session_prefix` and a count. With no prefix in the
//! script, the prefix has the stub's process ID in it, so a stub started
//! again gives no ID the one before it gave. A script that sets the prefix
//! gets the same IDs from every stub, which is how a test has a harness hand
//! out an ID twice.
//!
//! With `modes` in the script, it offers modes as OpenCode does: a session
//! configuration option of category `mode` with each primary agent in
//! `OPENCODE_CONFIG_CONTENT`, then `build` and `plan`, starting in that
//! configuration's `default_agent`. An `ask` step asks before it acts when the
//! session's agent has `ask` for its permission, as OpenCode does: it waits
//! for the answer, and a reject ends the turn as cancelled. A turn that is
//! cancelled while it asks withdraws the request with `$/cancel_request`.
//!
//! A Model with `efforts` in the script has effort levels as OpenCode's have:
//! a session configuration option of category `thought_level`, offered only
//! while the session is at that Model, with those levels and `default`. With
//! `commands` in the script, each new or loaded session is followed by an
//! `available_commands_update` that names them.
//!
//! With `steering` in the script, it steers as OpenCode 2.4.1 does: its
//! `initialize` answer says `_meta.steering.supported`, and a
//! `_session/steering` request for a session whose prompt runs is answered
//! `injected` and handed to that prompt, where an `await_steer` step takes
//! it. For a session with no prompt running, or with `steer_refused`, it is
//! answered `promptRequired`.
//!
//! Usage:
//!   crowbar-stub-harness --script <path> [--log <path>]

use agent_client_protocol_schema::ProtocolVersion;
use agent_client_protocol_schema::v1::{
    AGENT_METHOD_NAMES, AgentCapabilities, AvailableCommand, AvailableCommandInput, AvailableCommandsUpdate,
    CLIENT_METHOD_NAMES, CancelNotification, CloseSessionRequest, CloseSessionResponse, ConfigOptionUpdate,
    ContentBlock, ContentChunk, Diff, Error as AcpError, Implementation, InitializeRequest, InitializeResponse,
    JsonRpcMessage, LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, Notification,
    PROTOCOL_LEVEL_METHOD_NAMES, PermissionOption, PermissionOptionKind, PromptRequest, PromptResponse, Request,
    RequestId, RequestPermissionRequest, Response, SessionCapabilities, SessionCloseCapabilities, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOption, SessionId, SessionInfoUpdate, SessionNotification,
    SessionUpdate, SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, StopReason, TextContent, ToolCall,
    ToolCallContent, ToolCallId, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, ToolKind,
    UnstructuredCommandInput,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
struct StubModel {
    id: String,
    name: String,
    /// The Model's effort levels. A session at it also offers `default`.
    #[serde(default)]
    efforts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct StubCommand {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    hint: Option<String>,
}

/// The effort a session begins at, and goes back to at a Model that does not
/// offer the one it had.
const DEFAULT_EFFORT: &str = "default";

#[derive(Debug, Deserialize)]
struct Script {
    #[serde(default)]
    delete_session: bool,
    #[serde(default)]
    delete_session_error: Option<ScriptError>,
    /// Waits this long before it answers `session/delete`, and goes on with
    /// its other work meanwhile.
    #[serde(default)]
    delete_delay_ms: u64,
    #[serde(default = "default_true")]
    load_session: bool,
    #[serde(default = "default_session_prefix")]
    session_prefix: String,
    /// If set, the ID of every session, in place of the prefix and a count.
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    initialize_error: Option<ScriptError>,
    #[serde(default)]
    new_session_error: Option<ScriptError>,
    /// Waits this long before it answers `session/new`.
    #[serde(default)]
    new_session_delay_ms: u64,
    /// Models advertised via ACP `configOptions`.
    #[serde(default)]
    models: Option<Vec<StubModel>>,
    /// Default Model value for new sessions (defaults to the first entry in `models`).
    #[serde(default)]
    default_model: Option<String>,
    /// If true and `OPENCODE_DISABLE_FREE_MODELS=1` is set in the environment,
    /// `session/new` fails with OpenCode's `CatalogNotReady` error.
    #[serde(default)]
    no_models_when_free_disabled: bool,
    /// If true, sends an `available_commands_update` notification right after `session/new`.
    #[serde(default)]
    emit_available_commands: bool,
    /// The commands named in an `available_commands_update` after each
    /// `session/new` and `session/load`.
    #[serde(default)]
    commands: Vec<StubCommand>,
    /// An effort `session/set_config_option` refuses with an error, though a
    /// Model offers it.
    #[serde(default)]
    refuse_effort: Option<String>,
    /// A mode (a harness value) `session/set_config_option` refuses with an
    /// error, though the harness offers it.
    #[serde(default)]
    refuse_mode: Option<String>,
    /// With `refuse_mode`: the refusal comes in one write with a word that
    /// this subagent (a child session ID) still runs.
    #[serde(default)]
    refusal_rides_with: Option<String>,
    /// A mode `session/set_config_option` takes is said in a
    /// `config_option_update` before the answer.
    #[serde(default)]
    mode_update_first: bool,
    /// The answer to a mode change comes in one write with a request of this
    /// subagent's (a child session ID), as when the subagent asks just then.
    #[serde(default)]
    mode_reply_rides_with_ask: Option<String>,
    /// The answer to a mode change comes in one write with this text of the
    /// session's, as when the harness goes on talking just then.
    #[serde(default)]
    mode_reply_rides_with_text: Option<String>,
    /// If true, `session/set_config_option` for an effort is answered with
    /// the effort the session was at, and the new one comes in a
    /// `config_option_update` that follows, as OpenCode does while another
    /// of its sessions works.
    #[serde(default)]
    effort_answer_lags: bool,
    /// If true, each new session also emits a `config_option_update` on all
    /// already-open sessions with their current options, as OpenCode does when
    /// reloading its catalog.
    #[serde(default)]
    emit_catalog_update_on_new_session: bool,
    /// Updates sent on `session/load` before it is answered, as a harness
    /// replays a session's history.
    #[serde(default)]
    load_replay: Vec<Step>,
    /// Waits this long before it answers `session/load`, and goes on taking
    /// requests meanwhile.
    #[serde(default)]
    load_delay_ms: u64,
    /// Waits this long before it answers `session/set_config_option`, and goes
    /// on taking requests meanwhile.
    #[serde(default)]
    set_config_delay_ms: u64,
    /// If true, every result also carries `"error": null`, and every update
    /// `"id": null`.
    #[serde(default)]
    explicit_nulls: bool,
    /// If true, offers modes as OpenCode does (see the module's doc).
    #[serde(default)]
    modes: bool,
    /// If set, the modes offered in place of OpenCode's, none of which asks,
    /// starting in the first.
    #[serde(default)]
    mode_values: Option<Vec<String>>,
    /// If true, `session/set_config_option` is answered with only the option
    /// it set, as a harness that says only what changed.
    #[serde(default)]
    partial_config_replies: bool,
    /// If true, says it can steer a turn, and steers as OpenCode does (see
    /// the module's doc).
    #[serde(default)]
    steering: bool,
    /// If true, a steering message is never taken: each is answered
    /// `promptRequired`, as when the turn ends just as it comes.
    #[serde(default)]
    steer_refused: bool,
    /// A steering message with this in it is never taken.
    #[serde(default)]
    steer_refused_for: Option<String>,
    /// Waits this long before it answers a steering message, and goes on with
    /// its other work meanwhile, as the turn's end.
    #[serde(default)]
    steer_delay_ms: u64,
    /// If true, the stub ends at once when a steering message comes, with
    /// nothing answered.
    #[serde(default)]
    steer_exits: bool,
    #[serde(default)]
    turns: Vec<TurnScript>,
}

fn default_true() -> bool {
    true
}

fn default_session_prefix() -> String {
    format!("acp-stub-session-{}-", std::process::id())
}

#[derive(Debug, Clone, Deserialize)]
struct ScriptError {
    code: i32,
    message: String,
}

#[derive(Debug, Clone, Deserialize)]
struct TurnScript {
    /// If set, matches prompts whose text contains this substring.
    #[serde(default)]
    r#match: Option<String>,
    #[serde(default)]
    steps: Vec<Step>,
    /// If true, holds the turn open until `session/cancel` arrives.
    #[serde(default)]
    stall: bool,
    /// Stop reason for `PromptResponse` (`end_turn`, `cancelled`, `refusal`, `max_tokens`).
    #[serde(default = "default_stop_reason")]
    stop_reason: String,
    /// If set, responds to `session/prompt` with this JSON-RPC error instead of a result,
    /// whether the turn ran to its end or was cancelled.
    #[serde(default)]
    error: Option<ScriptError>,
    /// If true, returns a JSON-RPC request-cancelled error when cancelled instead of `StopReason::Cancelled`.
    #[serde(default)]
    cancel_with_error: bool,
    /// Updates emitted after `PromptResponse` has been sent.
    #[serde(default)]
    after: Vec<Step>,
    /// If true, exits the stub process abruptly mid-turn (after running `steps`).
    #[serde(default)]
    crash: bool,
    /// If true, takes no notice of `session/cancel`: the turn goes on, and a
    /// `stall` holds it open for ever.
    #[serde(default)]
    ignore_cancel: bool,
    /// If true, the stub ends at once when the turn is cancelled, with
    /// nothing answered, and leaves what it started running.
    #[serde(default)]
    exit_on_cancel: bool,
}

fn default_stop_reason() -> String {
    "end_turn".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Step {
    Text {
        text: String,
    },
    PauseMs {
        ms: u64,
    },
    Act {
        id: String,
        #[serde(default = "default_tool_kind")]
        tool_kind: String,
        title: String,
        #[serde(default = "default_tool_status")]
        status: String,
        #[serde(default)]
        content: Option<String>,
        #[serde(default)]
        raw_input: Option<Value>,
        #[serde(default)]
        raw_output: Option<Value>,
    },
    ActUpdate {
        id: String,
        #[serde(default)]
        tool_kind: Option<String>,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        content: Option<String>,
        #[serde(default)]
        raw_output: Option<Value>,
    },
    Title {
        title: String,
    },
    /// Waits for a steering message to the session, or a cancel, then says
    /// `text` with `{steer}` in it as the message's text.
    AwaitSteer {
        text: String,
    },
    Thought {
        text: String,
    },
    AvailableCommands,
    Unknown {
        update: Value,
    },
    /// Writes this many bytes with no line end.
    Flood {
        bytes: usize,
    },
    /// An act that asks first when the session's agent asks for its
    /// permission, and is carried out at once otherwise.
    Ask(AskStep),
    /// Starts `sleep 600` as a child of the stub and writes its process ID
    /// to `pid_file`. With `detach`, a shell starts it and ends at once, so
    /// it is a child of no process of the stub's, as a command run in the
    /// background is.
    Spawn {
        pid_file: String,
        #[serde(default)]
        detach: bool,
    },
    /// The session's mode changes to `mode` (a harness value), as when the
    /// harness changes it itself.
    ModeChange {
        mode: String,
    },
    /// Starts a subagent session (`protocol`: `"opencode"` or `"acp"`). With
    /// `launched_by`, the subagent of that ID launches it, as OpenCode
    /// reports a nested launch: its launcher call comes as an update of the
    /// launching subagent's session.
    SubagentStart {
        id: String,
        title: String,
        #[serde(default)]
        prompt: Option<String>,
        #[serde(default = "default_subagent_protocol")]
        protocol: String,
        #[serde(default)]
        launched_by: Option<String>,
        /// Leaves the launcher call open, naming no child, and says nothing
        /// of the child: a launch the harness has yet to report.
        #[serde(default)]
        hold_launcher: bool,
        /// Ends the launcher call failed, after it named its child.
        #[serde(default)]
        launch_fails: bool,
        /// For a nested launch: waits this long between telling of the child
        /// and the launcher naming it.
        #[serde(default)]
        name_after_ms: u64,
        /// Resumes a child as OpenCode 2.4.1 does one from before it
        /// restarted: only the launcher's output names it, and nothing of
        /// what it does comes after.
        #[serde(default)]
        unfollowed: bool,
    },
    /// Streams an answer text chunk from a subagent.
    SubagentText {
        subagent: String,
        text: String,
        #[serde(default = "default_subagent_protocol")]
        protocol: String,
    },
    /// Emits a tool call inside a subagent.
    SubagentAct {
        subagent: String,
        #[serde(default)]
        subagent_title: Option<String>,
        id: String,
        #[serde(default = "default_tool_kind")]
        tool_kind: String,
        title: String,
        #[serde(default = "default_tool_status")]
        status: String,
        #[serde(default)]
        content: Option<String>,
        #[serde(default)]
        raw_input: Option<Value>,
        #[serde(default)]
        raw_output: Option<Value>,
        #[serde(default = "default_subagent_protocol")]
        protocol: String,
    },
    /// Emits a permission request from a subagent and waits for the answer.
    SubagentAsk {
        subagent: String,
        #[serde(default)]
        subagent_title: Option<String>,
        #[serde(default = "default_subagent_protocol")]
        protocol: String,
        #[serde(default = "default_true")]
        wait_for_answer: bool,
        #[serde(flatten)]
        ask: AskStep,
    },
    /// Ends a subagent (`completed`, `failed`, `interrupted`, `cancelled`).
    SubagentEnd {
        subagent: String,
        #[serde(default = "default_subagent_end")]
        status: String,
        #[serde(default)]
        error: Option<String>,
        #[serde(default = "default_subagent_protocol")]
        protocol: String,
    },
    /// Holds a subagent open until `session/cancel` arrives for it.
    SubagentStall {
        subagent: String,
        #[serde(default = "default_subagent_protocol")]
        protocol: String,
        /// Takes this long to stop once told to.
        #[serde(default)]
        linger_ms: u64,
    },
}

fn default_subagent_protocol() -> String {
    "opencode".to_string()
}

fn default_subagent_end() -> String {
    "completed".to_string()
}

#[derive(Debug, Clone, Deserialize)]
struct AskStep {
    id: String,
    #[serde(default = "default_tool_kind")]
    tool_kind: String,
    title: String,
    #[serde(default)]
    raw_input: Option<Value>,
    /// For an edit: the file, what it held, and what it is to hold.
    #[serde(default)]
    diff: Option<StubDiff>,
    /// What it prints when it runs.
    #[serde(default)]
    output: Option<String>,
    /// Withdraws the request with `$/cancel_request` if it is not answered
    /// in this long, as a harness does that no longer needs it.
    #[serde(default)]
    withdraw_after_ms: Option<u64>,
    /// The request does not say what kind of act it asks about.
    #[serde(default)]
    request_without_kind: bool,
    /// The request is the first word of the act: no tool call comes before it.
    #[serde(default)]
    request_without_call: bool,
    /// Once allowed, the act runs this long before it ends.
    #[serde(default)]
    run_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct StubDiff {
    path: String,
    #[serde(default)]
    old: Option<String>,
    new: String,
}

fn default_tool_kind() -> String {
    "execute".to_string()
}

fn default_tool_status() -> String {
    "in_progress".to_string()
}

fn parse_tool_kind(s: &str) -> ToolKind {
    match s {
        "read" => ToolKind::Read,
        "edit" => ToolKind::Edit,
        "delete" => ToolKind::Delete,
        "move" => ToolKind::Move,
        "search" => ToolKind::Search,
        "execute" => ToolKind::Execute,
        "think" => ToolKind::Think,
        "fetch" => ToolKind::Fetch,
        "switch_mode" => ToolKind::SwitchMode,
        _ => ToolKind::Other,
    }
}

fn parse_tool_status(s: &str) -> ToolCallStatus {
    match s {
        "pending" => ToolCallStatus::Pending,
        "in_progress" => ToolCallStatus::InProgress,
        "completed" => ToolCallStatus::Completed,
        "failed" => ToolCallStatus::Failed,
        _ => ToolCallStatus::Pending,
    }
}

fn parse_stop_reason(s: &str) -> StopReason {
    match s {
        "cancelled" => StopReason::Cancelled,
        "refusal" => StopReason::Refusal,
        "max_tokens" => StopReason::MaxTokens,
        "max_turn_requests" => StopReason::MaxTurnRequests,
        _ => StopReason::EndTurn,
    }
}

struct Options {
    script: PathBuf,
    log: Option<PathBuf>,
    env_out: Option<PathBuf>,
}

fn parse_options(args: &[String]) -> Option<Options> {
    let mut script = None;
    let mut log = None;
    let mut env_out = None;
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let value = iter.next()?;
        match flag.as_str() {
            "--script" if script.is_none() => script = Some(PathBuf::from(value)),
            "--log" if log.is_none() => log = Some(PathBuf::from(value)),
            "--env-out" if env_out.is_none() => env_out = Some(PathBuf::from(value)),
            _ => return None,
        }
    }
    Some(Options {
        script: script?,
        log,
        env_out,
    })
}

struct Writer {
    stdout: Mutex<io::Stdout>,
    explicit_nulls: bool,
}

impl Writer {
    fn new(explicit_nulls: bool) -> Self {
        Self {
            stdout: Mutex::new(io::stdout()),
            explicit_nulls,
        }
    }

    /// Writes bytes with no line end after them.
    fn send_unfinished(&self, bytes: &[u8]) {
        let mut out = self.stdout.lock().unwrap();
        let _ = out.write_all(bytes);
        let _ = out.flush();
    }

    /// Writes messages in one write, so the reader takes them in one read.
    fn send_together(&self, values: &[Value]) {
        let text: String = values.iter().map(|v| format!("{v}\n")).collect();
        self.send_unfinished(text.as_bytes());
    }

    fn send(&self, value: &Value) {
        let line = value.to_string();
        let mut out = self.stdout.lock().unwrap();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }

    fn send_result<R: serde::Serialize>(&self, id: RequestId, result: R) {
        let msg = JsonRpcMessage::wrap(Response::<R, AcpError>::new(id, Ok(result)));
        let mut value = serde_json::to_value(msg).expect("serialize response");
        if self.explicit_nulls {
            value["error"] = Value::Null;
        }
        self.send(&value);
    }

    fn send_error(&self, id: RequestId, err: AcpError) {
        let msg = JsonRpcMessage::wrap(Response::<Value, AcpError>::new(id, Err(err)));
        let value = serde_json::to_value(msg).expect("serialize error response");
        self.send(&value);
    }

    fn send_update(&self, session_id: &SessionId, update: SessionUpdate) {
        let notif = JsonRpcMessage::wrap(Notification {
            method: CLIENT_METHOD_NAMES.session_update.into(),
            params: Some(SessionNotification::new(session_id.clone(), update)),
        });
        let mut value = serde_json::to_value(notif).expect("serialize session notification");
        if self.explicit_nulls {
            value["id"] = Value::Null;
        }
        self.send(&value);
    }

    fn send_raw_update(&self, session_id: &SessionId, raw_update: Value) {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": CLIENT_METHOD_NAMES.session_update,
            "params": {
                "sessionId": session_id.0.as_ref(),
                "update": raw_update,
            }
        });
        self.send(&msg);
    }
}

struct CancelFlag {
    cancelled: Mutex<bool>,
    cv: Condvar,
}

impl CancelFlag {
    fn new() -> Self {
        Self {
            cancelled: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    fn cancel(&self) {
        let mut guard = self.cancelled.lock().unwrap();
        *guard = true;
        self.cv.notify_all();
    }

    fn is_cancelled(&self) -> bool {
        *self.cancelled.lock().unwrap()
    }

    /// Sleeps for `dur`, waking early if cancelled. Returns `true` if cancelled.
    fn sleep_or_cancel(&self, dur: Duration) -> bool {
        let guard = self.cancelled.lock().unwrap();
        if *guard {
            return true;
        }
        let (guard, _) = self.cv.wait_timeout(guard, dur).unwrap();
        *guard
    }

    /// Blocks until cancelled.
    fn wait_cancelled(&self) {
        let mut guard = self.cancelled.lock().unwrap();
        while !*guard {
            guard = self.cv.wait(guard).unwrap();
        }
    }
}

/// Messages steered into each session's running prompt, until a step takes them.
type Steers = Mutex<HashMap<String, std::collections::VecDeque<String>>>;

/// Waits until a message is steered into the session, or the turn is
/// cancelled, and returns the message, or `None` when cancelled.
fn await_steer(steers: &Steers, session: &str, cancel: &CancelFlag) -> Option<String> {
    loop {
        if let Some(text) = steers.lock().unwrap().get_mut(session).and_then(|q| q.pop_front()) {
            return Some(text);
        }
        if cancel.sleep_or_cancel(Duration::from_millis(10)) {
            return None;
        }
    }
}

struct ScriptPicker {
    turns: Vec<TurnScript>,
    next_unmatched: usize,
}

impl ScriptPicker {
    fn pick(&mut self, prompt_text: &str) -> TurnScript {
        // Check for explicit substring matches first.
        if let Some(found) = self
            .turns
            .iter()
            .find(|t| t.r#match.as_deref().is_some_and(|m| prompt_text.contains(m)))
        {
            return found.clone();
        }
        // Otherwise take the next sequential unmatched entry.
        while self.next_unmatched < self.turns.len() {
            let idx = self.next_unmatched;
            self.next_unmatched += 1;
            if self.turns[idx].r#match.is_none() {
                return self.turns[idx].clone();
            }
        }
        TurnScript {
            r#match: None,
            steps: vec![Step::Text {
                text: format!("stub echo: {prompt_text}"),
            }],
            stall: false,
            stop_reason: "end_turn".to_string(),
            error: None,
            cancel_with_error: false,
            after: Vec::new(),
            crash: false,
            ignore_cancel: false,
            exit_on_cancel: false,
        }
    }
}

/// What an `ask` step needs from the stub beyond its session.
struct Asking<'a> {
    modes: &'a Modes,
    session_modes: &'a Mutex<HashMap<String, String>>,
    always: &'a Mutex<HashMap<String, Vec<String>>>,
    replies: &'a Replies,
    next_request: &'a AtomicU64,
}

/// What an `ask` step came to.
#[derive(PartialEq)]
enum Asked {
    /// The turn goes on.
    Went,
    /// The person said no, or the turn was cancelled while it asked: the
    /// turn ends as cancelled.
    Ended,
}

/// Answers Lever sends to the stub's own requests, by request ID.
type Replies = Mutex<HashMap<String, std::sync::mpsc::Sender<Value>>>;

/// The modes the stub offers: each primary agent in `OPENCODE_CONFIG_CONTENT`
/// with what it asks for, then `build` and `plan`, and the one a session
/// starts in.
struct Modes {
    offered: bool,
    agents: Vec<(String, HashMap<String, String>)>,
    default: String,
}

impl Modes {
    fn read(offered: bool, values: Option<&[String]>) -> Modes {
        if let Some(values) = values {
            return Modes {
                offered,
                agents: values.iter().map(|v| (v.clone(), HashMap::new())).collect(),
                default: values.first().cloned().unwrap_or_default(),
            };
        }
        let config: Value = std::env::var("OPENCODE_CONFIG_CONTENT")
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or(Value::Null);
        let mut agents = Vec::new();
        if let Some(defined) = config.get("agent").and_then(Value::as_object) {
            for (name, agent) in defined {
                if agent.get("mode").and_then(Value::as_str) != Some("primary") {
                    continue;
                }
                // A rule for a permission, or rules by pattern, of which
                // the stub takes the one for every pattern.
                let permission = agent
                    .get("permission")
                    .and_then(Value::as_object)
                    .map(|p| {
                        p.iter()
                            .filter_map(|(k, v)| {
                                let rule = v.as_str().or_else(|| v.get("*").and_then(Value::as_str))?;
                                Some((k.clone(), rule.to_string()))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                agents.push((name.clone(), permission));
            }
        }
        agents.push(("build".to_string(), HashMap::new()));
        agents.push((
            "plan".to_string(),
            HashMap::from([("edit".to_string(), "deny".to_string())]),
        ));
        let default = config
            .get("default_agent")
            .and_then(Value::as_str)
            .filter(|d| agents.iter().any(|(name, _)| name == d))
            .unwrap_or("build")
            .to_string();
        Modes {
            offered,
            agents,
            default,
        }
    }

    fn has(&self, mode: &str) -> bool {
        self.agents.iter().any(|(name, _)| name == mode)
    }

    /// What `mode` does for `permission`: `ask`, `deny`, or `allow`.
    fn rule(&self, mode: &str, permission: &str) -> String {
        self.agents
            .iter()
            .find(|(name, _)| name == mode)
            .and_then(|(_, rules)| rules.get(permission).cloned())
            .unwrap_or_else(|| "allow".to_string())
    }
}

fn act_update(writer: &Writer, session_id: &SessionId, id: &str, status: ToolCallStatus, text: &str) {
    let fields = ToolCallUpdateFields::new()
        .status(status)
        .content(vec![ToolCallContent::from(ContentBlock::from(text.to_string()))]);
    writer.send_update(
        session_id,
        SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(ToolCallId::new(id.to_string()), fields)),
    );
}

#[allow(clippy::too_many_arguments)]
fn ask(
    writer: &Writer,
    session_id: &SessionId,
    asking: &Asking<'_>,
    cancel: Option<&CancelFlag>,
    step: &AskStep,
) -> Asked {
    let AskStep {
        id,
        tool_kind,
        title,
        raw_input,
        diff,
        output,
        withdraw_after_ms,
        request_without_kind,
        request_without_call,
        run_ms,
    } = step;
    let (id, title, raw_input, diff, output) = (
        id.as_str(),
        title.as_str(),
        raw_input.as_ref(),
        diff.as_ref(),
        output.as_deref(),
    );
    let kind = parse_tool_kind(tool_kind);
    let permission = if kind == ToolKind::Execute { "bash" } else { "edit" };
    let mut fields = ToolCallUpdateFields::new().title(title.to_string());
    if !request_without_kind {
        fields = fields.kind(kind);
    }
    if let Some(input) = raw_input {
        fields = fields.raw_input(input.clone());
    }
    if let Some(d) = diff {
        let mut change = Diff::new(PathBuf::from(&d.path), d.new.clone());
        if let Some(old) = &d.old {
            change = change.old_text(old.clone());
        }
        fields = fields.content(vec![ToolCallContent::Diff(change)]);
    }
    let mut call = ToolCall::new(ToolCallId::new(id.to_string()), title.to_string())
        .kind(kind)
        .status(ToolCallStatus::InProgress);
    if let Some(input) = raw_input {
        call = call.raw_input(input.clone());
    }
    if !request_without_call {
        writer.send_update(session_id, SessionUpdate::ToolCall(call));
    }

    let sid = session_id.0.as_ref().to_string();
    let mode = asking
        .session_modes
        .lock()
        .unwrap()
        .get(&sid)
        .cloned()
        .unwrap_or_else(|| asking.modes.default.clone());
    let rule = if asking.modes.offered {
        asking.modes.rule(&mode, permission)
    } else {
        "allow".to_string()
    };
    let allowed_always = asking
        .always
        .lock()
        .unwrap()
        .get(&sid)
        .is_some_and(|granted| granted.iter().any(|p| p == permission));
    if rule == "deny" {
        act_update(
            writer,
            session_id,
            id,
            ToolCallStatus::Failed,
            "Not allowed in this mode",
        );
        return Asked::Went;
    }
    if rule == "ask" && !allowed_always {
        let n = asking.next_request.fetch_add(1, Ordering::Relaxed);
        let request_id = RequestId::Number(n as i64);
        let (tx, rx) = std::sync::mpsc::channel();
        asking.replies.lock().unwrap().insert(n.to_string(), tx);
        let options = vec![
            PermissionOption::new("once", "Allow once", PermissionOptionKind::AllowOnce),
            PermissionOption::new("always", "Always allow", PermissionOptionKind::AllowAlways),
            PermissionOption::new("reject", "Reject", PermissionOptionKind::RejectOnce),
        ];
        let request = RequestPermissionRequest::new(
            session_id.clone(),
            ToolCallUpdate::new(ToolCallId::new(id.to_string()), fields),
            options,
        );
        writer.send(
            &serde_json::to_value(JsonRpcMessage::wrap(Request {
                id: request_id.clone(),
                method: CLIENT_METHOD_NAMES.session_request_permission.into(),
                params: Some(request),
            }))
            .expect("serialize permission request"),
        );
        let asked_at = std::time::Instant::now();
        let answer = loop {
            let given_up = withdraw_after_ms.is_some_and(|ms| asked_at.elapsed() >= Duration::from_millis(ms));
            if given_up || cancel.is_some_and(CancelFlag::is_cancelled) {
                asking.replies.lock().unwrap().remove(&n.to_string());
                writer.send(&json!({
                    "jsonrpc": "2.0",
                    "method": PROTOCOL_LEVEL_METHOD_NAMES.cancel_request,
                    "params": { "requestId": n },
                }));
                break None;
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(reply) => break Some(reply),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
            }
        };
        let chosen = answer
            .as_ref()
            .and_then(|r| r.pointer("/result/outcome"))
            .filter(|o| o.get("outcome").and_then(Value::as_str) == Some("selected"))
            .and_then(|o| o.get("optionId").and_then(Value::as_str))
            .map(str::to_string);
        match chosen.as_deref() {
            Some("once") => {}
            Some("always") => {
                asking
                    .always
                    .lock()
                    .unwrap()
                    .entry(sid)
                    .or_default()
                    .push(permission.to_string());
            }
            _ => {
                act_update(
                    writer,
                    session_id,
                    id,
                    ToolCallStatus::Failed,
                    "The user declined this tool call",
                );
                return Asked::Ended;
            }
        }
    }
    thread::sleep(Duration::from_millis(*run_ms));
    act_update(
        writer,
        session_id,
        id,
        ToolCallStatus::Completed,
        output.unwrap_or("ran"),
    );
    Asked::Went
}

fn send_opencode_child_status(
    writer: &Writer,
    root_sid: &SessionId,
    child_sid: &str,
    title: Option<&str>,
    status: &str,
    error: Option<&str>,
) {
    send_opencode_nested_status(writer, root_sid, None, child_sid, title, status, error);
}

/// A child's status, as OpenCode says it of a child of `parent_sid` (a
/// subagent) or, with none, of the root.
fn send_opencode_nested_status(
    writer: &Writer,
    root_sid: &SessionId,
    parent_sid: Option<&str>,
    child_sid: &str,
    title: Option<&str>,
    status: &str,
    error: Option<&str>,
) {
    let mut params = json!({
        "rootSessionId": root_sid.0.as_ref(),
        "childSessionId": child_sid,
        "parentSessionId": parent_sid.unwrap_or(root_sid.0.as_ref()),
        "depth": if parent_sid.is_some() { 2 } else { 1 },
        "type": "status",
        "status": status,
    });
    if let Some(t) = title {
        params["title"] = json!(t);
    }
    if let Some(err) = error {
        params["error"] = json!({ "message": err });
    }
    writer.send(&json!({
        "jsonrpc": "2.0",
        "method": "opencode/session/child_update",
        "params": params,
    }));
}

fn send_opencode_child_update(
    writer: &Writer,
    root_sid: &SessionId,
    child_sid: &str,
    title: Option<&str>,
    update: Value,
) {
    let mut params = json!({
        "rootSessionId": root_sid.0.as_ref(),
        "childSessionId": child_sid,
        "parentSessionId": root_sid.0.as_ref(),
        "depth": 1,
        "type": "update",
        "update": update,
    });
    if let Some(t) = title {
        params["title"] = json!(t);
    }
    writer.send(&json!({
        "jsonrpc": "2.0",
        "method": "opencode/session/child_update",
        "params": params,
    }));
}

#[allow(clippy::too_many_arguments)]
fn run_subagent_step(
    writer: &Writer,
    session_id: &SessionId,
    asking: &Asking<'_>,
    active_cancels: &Mutex<HashMap<String, Arc<CancelFlag>>>,
    subagent_titles: &mut HashMap<String, String>,
    step: &Step,
) {
    match step {
        Step::SubagentStart {
            id,
            title,
            prompt,
            protocol,
            launched_by: Some(parent),
            name_after_ms,
            ..
        } => {
            // OpenCode 2.4.1 reports a subagent's own launcher call as an
            // update of that subagent's session: its ID and title carry the
            // subagent's, and its output names the new child's session.
            subagent_titles.insert(id.clone(), title.clone());
            active_cancels
                .lock()
                .unwrap()
                .entry(id.clone())
                .or_insert_with(|| Arc::new(CancelFlag::new()));
            let parent_title = subagent_titles.get(parent).cloned();
            let wrapper_id = format!("{parent}:call_subagent_{id}");
            let wrapper_title = match &parent_title {
                Some(t) => format!("{t}: subagent"),
                None => "subagent".to_string(),
            };
            let mut input = json!({ "description": title, "agent": "general" });
            if let Some(p) = prompt {
                input["prompt"] = json!(p);
            }
            let meta =
                json!({ "opencode/child-session": { "id": parent, "parentID": session_id.0.as_ref(), "depth": 1 } });
            let _ = protocol;
            send_opencode_child_update(
                writer,
                session_id,
                parent,
                parent_title.as_deref(),
                json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": wrapper_id,
                    "title": wrapper_title,
                    "kind": "think",
                    "status": "pending",
                    "rawInput": input,
                    "_meta": meta,
                }),
            );
            send_opencode_nested_status(writer, session_id, Some(parent), id, Some(title), "created", None);
            thread::sleep(Duration::from_millis(*name_after_ms));
            send_opencode_child_update(
                writer,
                session_id,
                parent,
                parent_title.as_deref(),
                json!({
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": wrapper_id,
                    "status": "completed",
                    "content": [{ "type": "content", "content": { "type": "text", "text": format!("The subagent is working in the background (sessionID: {id}).") } }],
                    "rawOutput": { "metadata": { "sessionID": id, "status": "running" } },
                    "_meta": meta,
                }),
            );
            send_opencode_nested_status(writer, session_id, Some(parent), id, Some(title), "running", None);
        }
        Step::SubagentStart {
            id,
            title,
            prompt,
            protocol,
            launched_by: None,
            hold_launcher,
            launch_fails,
            unfollowed,
            ..
        } => {
            subagent_titles.insert(id.clone(), title.clone());
            active_cancels
                .lock()
                .unwrap()
                .entry(id.clone())
                .or_insert_with(|| Arc::new(CancelFlag::new()));
            if protocol == "acp" {
                writer.send_raw_update(
                    session_id,
                    json!({
                        "sessionUpdate": "subagent_update",
                        "sessionId": id,
                        "title": title,
                        "state": { "state": "running" }
                    }),
                );
                if let Some(p) = prompt {
                    writer.send_raw_update(
                        session_id,
                        json!({
                            "sessionUpdate": "session_message",
                            "messageId": format!("msg-{id}"),
                            "senderSessionId": session_id.0.as_ref(),
                            "recipientSessionId": id,
                            "content": [{ "type": "text", "text": p }]
                        }),
                    );
                }
            } else {
                // Mirror real OpenCode 2.4.1: parent `subagent` wrapper tool
                // call + `opencode/session/child_update` status notifications.
                let wrapper_id = format!("call_subagent_{id}");
                let mut input = json!({ "description": title, "agent": "general" });
                if let Some(p) = prompt {
                    input["prompt"] = json!(p);
                }
                let tc = ToolCall::new(ToolCallId::new(wrapper_id.clone()), "subagent".to_string())
                    .kind(ToolKind::Think)
                    .status(ToolCallStatus::Pending);
                writer.send_update(session_id, SessionUpdate::ToolCall(tc));
                let in_prog = ToolCallUpdateFields::new()
                    .status(ToolCallStatus::InProgress)
                    .raw_input(input);
                writer.send_update(
                    session_id,
                    SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(ToolCallId::new(wrapper_id.clone()), in_prog)),
                );
                if *hold_launcher {
                    return;
                }
                // A launch that fails says nothing of its child before it does.
                if !*launch_fails && !*unfollowed {
                    send_opencode_child_status(writer, session_id, id, Some(title), "created", None);
                }
                let (end, said) = if *launch_fails {
                    (
                        ToolCallStatus::Failed,
                        format!("Could not start the subagent (sessionID: {id})."),
                    )
                } else {
                    (
                        ToolCallStatus::Completed,
                        format!("The subagent is working in the background (sessionID: {id})."),
                    )
                };
                let completed = ToolCallUpdateFields::new()
                    .status(end)
                    .content(vec![ToolCallContent::from(ContentBlock::from(said))])
                    .raw_output(json!({
                        "metadata": { "sessionID": id, "status": "running", "truncated": false }
                    }));
                writer.send_update(
                    session_id,
                    SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(ToolCallId::new(wrapper_id), completed)),
                );
                if !*launch_fails && !*unfollowed {
                    send_opencode_child_status(writer, session_id, id, Some(title), "running", None);
                }
            }
        }
        Step::SubagentText {
            subagent,
            text,
            protocol,
        } => {
            let title = subagent_titles.get(subagent).map(String::as_str);
            if protocol == "acp" {
                writer.send_update(
                    &SessionId::new(subagent.clone()),
                    SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
                        text.clone(),
                    )))),
                );
            } else {
                send_opencode_child_update(
                    writer,
                    session_id,
                    subagent,
                    title,
                    json!({
                        "sessionUpdate": "agent_message_chunk",
                        "content": { "type": "text", "text": text },
                        "_meta": {
                            "opencode/child-session": {
                                "id": subagent,
                                "parentID": session_id.0.as_ref(),
                                "depth": 1,
                                "title": title.unwrap_or("Subagent"),
                            }
                        }
                    }),
                );
            }
        }
        Step::SubagentAct {
            subagent,
            subagent_title,
            id,
            tool_kind,
            title,
            status,
            content,
            raw_input,
            raw_output,
            protocol,
        } => {
            let child_title = subagent_title
                .as_deref()
                .or_else(|| subagent_titles.get(subagent).map(String::as_str));
            if protocol == "acp" {
                let mut tc = ToolCall::new(ToolCallId::new(id.clone()), title.clone())
                    .kind(parse_tool_kind(tool_kind))
                    .status(parse_tool_status(status));
                if let Some(c) = content {
                    tc = tc.content(vec![ToolCallContent::from(ContentBlock::from(c.clone()))]);
                }
                if let Some(inp) = raw_input {
                    tc = tc.raw_input(inp.clone());
                }
                if let Some(out) = raw_output {
                    tc = tc.raw_output(out.clone());
                }
                writer.send_update(&SessionId::new(subagent.clone()), SessionUpdate::ToolCall(tc));
            } else {
                let prefixed_title = match child_title {
                    Some(ct) => format!("{ct}: {title}"),
                    None => title.clone(),
                };
                let mut upd = json!({
                    "sessionUpdate": "tool_call",
                    "toolCallId": format!("{subagent}:{id}"),
                    "title": prefixed_title,
                    "kind": tool_kind,
                    "status": status,
                    "_meta": {
                        "opencode/child-session": {
                            "id": subagent,
                            "parentID": session_id.0.as_ref(),
                            "depth": 1,
                            "title": child_title.unwrap_or("Subagent"),
                        }
                    }
                });
                if let Some(c) = content {
                    upd["content"] = json!([{ "type": "content", "content": { "type": "text", "text": c } }]);
                }
                if let Some(inp) = raw_input {
                    upd["rawInput"] = inp.clone();
                }
                if let Some(out) = raw_output {
                    upd["rawOutput"] = out.clone();
                }
                send_opencode_child_update(writer, session_id, subagent, child_title, upd);
            }
        }
        Step::SubagentAsk {
            subagent,
            subagent_title,
            protocol,
            wait_for_answer,
            ask: step,
        } => {
            let child_title = subagent_title
                .as_deref()
                .or_else(|| subagent_titles.get(subagent).map(String::as_str));
            let cancel = active_cancels.lock().unwrap().get(subagent).cloned();
            let call_id = if protocol == "acp" {
                step.id.clone()
            } else {
                format!("{subagent}:{}", step.id)
            };
            let req_title = if protocol == "acp" {
                step.title.clone()
            } else if let Some(ct) = child_title {
                format!("{ct}: {}", step.title)
            } else {
                step.title.clone()
            };
            let n = asking.next_request.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = std::sync::mpsc::channel();
            asking.replies.lock().unwrap().insert(n.to_string(), tx);
            let mut tool_call = json!({
                "toolCallId": call_id,
                "title": req_title,
                "status": "pending",
            });
            if !step.request_without_kind {
                tool_call["kind"] = json!(step.tool_kind);
            }
            if let Some(inp) = &step.raw_input {
                tool_call["rawInput"] = inp.clone();
            }
            if let Some(d) = &step.diff {
                let mut diff = json!({ "type": "diff", "path": d.path, "newText": d.new });
                if let Some(old) = &d.old {
                    diff["oldText"] = json!(old);
                }
                tool_call["content"] = json!([diff]);
            }
            let target_sid = if protocol == "acp" {
                subagent.as_str()
            } else {
                tool_call["_meta"] = json!({
                    "opencode/child-session": {
                        "id": subagent,
                        "parentID": session_id.0.as_ref(),
                        "depth": 1,
                        "title": child_title.unwrap_or("Subagent"),
                    }
                });
                session_id.0.as_ref()
            };
            writer.send(&json!({
                "jsonrpc": "2.0",
                "id": n,
                "method": CLIENT_METHOD_NAMES.session_request_permission,
                "params": {
                    "sessionId": target_sid,
                    "toolCall": tool_call,
                    "options": [
                        { "optionId": "once", "name": "Allow once", "kind": "allow_once" },
                        { "optionId": "always", "name": "Always allow", "kind": "allow_always" },
                        { "optionId": "reject", "name": "Reject", "kind": "reject_once" }
                    ]
                }
            }));
            if !*wait_for_answer {
                return;
            }
            let asked_at = std::time::Instant::now();
            let answer = loop {
                let given_up = step
                    .withdraw_after_ms
                    .is_some_and(|ms| asked_at.elapsed() >= Duration::from_millis(ms));
                if given_up || cancel.as_deref().is_some_and(CancelFlag::is_cancelled) {
                    asking.replies.lock().unwrap().remove(&n.to_string());
                    writer.send(&json!({
                        "jsonrpc": "2.0",
                        "method": PROTOCOL_LEVEL_METHOD_NAMES.cancel_request,
                        "params": { "requestId": n },
                    }));
                    break None;
                }
                match rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(reply) => break Some(reply),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break None,
                }
            };
            let chosen = answer
                .as_ref()
                .and_then(|r| r.pointer("/result/outcome"))
                .filter(|o| o.get("outcome").and_then(Value::as_str) == Some("selected"))
                .and_then(|o| o.get("optionId").and_then(Value::as_str))
                .map(str::to_string);
            let (status_str, out_text) = match chosen.as_deref() {
                Some("once" | "always") => ("completed", step.output.as_deref().unwrap_or("ran")),
                _ => ("failed", "The user declined this tool call"),
            };
            if protocol == "acp" {
                act_update(
                    writer,
                    &SessionId::new(subagent.clone()),
                    &step.id,
                    parse_tool_status(status_str),
                    out_text,
                );
            } else {
                send_opencode_child_update(
                    writer,
                    session_id,
                    subagent,
                    child_title,
                    json!({
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": call_id,
                        "status": status_str,
                        "content": [{ "type": "content", "content": { "type": "text", "text": out_text } }],
                        "_meta": {
                            "opencode/child-session": {
                                "id": subagent,
                                "parentID": session_id.0.as_ref(),
                                "depth": 1,
                                "title": child_title.unwrap_or("Subagent"),
                            }
                        }
                    }),
                );
            }
        }
        Step::SubagentEnd {
            subagent,
            status,
            error,
            protocol,
        } => {
            let title = subagent_titles.get(subagent).map(String::as_str);
            active_cancels.lock().unwrap().remove(subagent);
            if protocol == "acp" {
                let state = match status.as_str() {
                    "cancelled" | "interrupted" => json!({ "state": "idle", "stopReason": "cancelled" }),
                    "failed" => json!({
                        "state": "idle",
                        "stopReason": "error",
                        "error": { "code": -32000, "message": error.as_deref().unwrap_or("The subagent failed.") }
                    }),
                    _ => json!({ "state": "idle", "stopReason": "end_turn" }),
                };
                writer.send_raw_update(
                    session_id,
                    json!({
                        "sessionUpdate": "subagent_update",
                        "sessionId": subagent,
                        "state": state,
                    }),
                );
            } else {
                send_opencode_child_status(writer, session_id, subagent, title, status, error.as_deref());
            }
        }
        Step::SubagentStall {
            subagent,
            protocol,
            linger_ms,
        } => {
            let flag = active_cancels
                .lock()
                .unwrap()
                .entry(subagent.clone())
                .or_insert_with(|| Arc::new(CancelFlag::new()))
                .clone();
            flag.wait_cancelled();
            thread::sleep(Duration::from_millis(*linger_ms));
            let title = subagent_titles.get(subagent).map(String::as_str);
            active_cancels.lock().unwrap().remove(subagent);
            if protocol == "acp" {
                writer.send_raw_update(
                    session_id,
                    json!({
                        "sessionUpdate": "subagent_update",
                        "sessionId": subagent,
                        "state": { "state": "idle", "stopReason": "cancelled" }
                    }),
                );
            } else {
                send_opencode_child_status(writer, session_id, subagent, title, "interrupted", None);
            }
        }
        _ => {}
    }
}

fn emit_step(
    writer: &Writer,
    session_id: &SessionId,
    step: &Step,
    cancel: Option<&CancelFlag>,
    model: Option<&str>,
) -> bool {
    match step {
        Step::PauseMs { ms } => {
            let dur = Duration::from_millis(*ms);
            if let Some(flag) = cancel {
                return flag.sleep_or_cancel(dur);
            }
            thread::sleep(dur);
        }
        Step::Text { text } => {
            let rendered = match model {
                Some(m) => text.replace("{model}", m),
                None => text.clone(),
            };
            writer.send_update(
                session_id,
                SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(rendered)))),
            );
        }
        Step::Act {
            id,
            tool_kind,
            title,
            status,
            content,
            raw_input,
            raw_output,
        } => {
            let mut tc = ToolCall::new(ToolCallId::new(id.clone()), title.clone())
                .kind(parse_tool_kind(tool_kind))
                .status(parse_tool_status(status));
            if let Some(text) = content {
                tc = tc.content(vec![ToolCallContent::from(ContentBlock::from(text.clone()))]);
            }
            if let Some(input) = raw_input {
                tc = tc.raw_input(input.clone());
            }
            if let Some(output) = raw_output {
                tc = tc.raw_output(output.clone());
            }
            writer.send_update(session_id, SessionUpdate::ToolCall(tc));
        }
        Step::ActUpdate {
            id,
            tool_kind,
            title,
            status,
            content,
            raw_output,
        } => {
            let mut fields = ToolCallUpdateFields::new();
            if let Some(kind) = tool_kind {
                fields = fields.kind(parse_tool_kind(kind));
            }
            if let Some(title) = title {
                fields = fields.title(title.clone());
            }
            if let Some(status) = status {
                fields = fields.status(parse_tool_status(status));
            }
            if let Some(text) = content {
                fields = fields.content(vec![ToolCallContent::from(ContentBlock::from(text.clone()))]);
            }
            if let Some(output) = raw_output {
                fields = fields.raw_output(output.clone());
            }
            writer.send_update(
                session_id,
                SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(ToolCallId::new(id.clone()), fields)),
            );
        }
        Step::Title { title } => {
            writer.send_update(
                session_id,
                SessionUpdate::SessionInfoUpdate(SessionInfoUpdate::new().title(title.clone())),
            );
        }
        Step::Thought { text } => {
            writer.send_update(
                session_id,
                SessionUpdate::AgentThoughtChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(text.clone())))),
            );
        }
        Step::AvailableCommands => {
            writer.send_update(
                session_id,
                SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(Vec::new())),
            );
        }
        Step::Unknown { update } => {
            writer.send_raw_update(session_id, update.clone());
        }
        Step::Flood { bytes } => writer.send_unfinished(&vec![b'x'; *bytes]),
        Step::Spawn { pid_file, detach: true } => {
            let _ = std::process::Command::new("sh")
                .arg("-c")
                .arg("sleep 600 >/dev/null 2>&1 & echo $! > \"$1\"")
                .arg("sh")
                .arg(pid_file)
                .status();
        }
        Step::Spawn {
            pid_file,
            detach: false,
        } => {
            if let Ok(child) = std::process::Command::new("sleep").arg("600").spawn() {
                let _ = std::fs::write(pid_file, child.id().to_string());
                // Left running: the stub's own end must take it, or Lever.
                std::mem::forget(child);
            }
        }
        // Carried out by the turn, which has what they need.
        Step::Ask(_)
        | Step::AwaitSteer { .. }
        | Step::ModeChange { .. }
        | Step::SubagentStart { .. }
        | Step::SubagentText { .. }
        | Step::SubagentAct { .. }
        | Step::SubagentAsk { .. }
        | Step::SubagentEnd { .. }
        | Step::SubagentStall { .. } => {}
    }
    false
}

fn prompt_text(req: &PromptRequest) -> String {
    req.prompt
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn append_log(log_file: &mut Option<File>, line: &str) {
    if let Some(file) = log_file.as_mut() {
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }
}

/// The effort levels a session at `model` offers: the Model's own, then
/// `default`. None when the Model has no levels.
fn effort_levels(models: Option<&[StubModel]>, model: Option<&str>) -> Vec<String> {
    let model = model.or_else(|| models.and_then(|m| m.first()).map(|m| m.id.as_str()));
    let mut levels = models
        .and_then(|list| list.iter().find(|m| Some(m.id.as_str()) == model))
        .map(|m| m.efforts.clone())
        .unwrap_or_default();
    if !levels.is_empty() {
        levels.push(DEFAULT_EFFORT.to_string());
    }
    levels
}

/// A level's name, as OpenCode makes it: its ID with a capital.
fn effort_name(id: &str) -> String {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn build_config_options(
    models: Option<&[StubModel]>,
    current: Option<&str>,
    modes: &Modes,
    current_mode: Option<&str>,
    effort: Option<&str>,
) -> Option<Vec<SessionConfigOption>> {
    let mut options = Vec::new();
    if let Some(models) = models {
        let levels = effort_levels(Some(models), current);
        let current = current
            .or_else(|| models.first().map(|m| m.id.as_str()))
            .unwrap_or("")
            .to_string();
        let opts: Vec<SessionConfigSelectOption> = models
            .iter()
            .map(|m| SessionConfigSelectOption::new(m.id.clone(), m.name.clone()))
            .collect();
        options.push(
            SessionConfigOption::select("model", "Model", current, opts).category(SessionConfigOptionCategory::Model),
        );
        if !levels.is_empty() {
            let at = effort
                .filter(|e| levels.iter().any(|level| level == e))
                .unwrap_or(DEFAULT_EFFORT)
                .to_string();
            let opts: Vec<SessionConfigSelectOption> = levels
                .iter()
                .map(|level| SessionConfigSelectOption::new(level.clone(), effort_name(level)))
                .collect();
            options.push(
                SessionConfigOption::select("effort", "Effort", at, opts)
                    .category(SessionConfigOptionCategory::ThoughtLevel),
            );
        }
    }
    if modes.offered {
        let opts: Vec<SessionConfigSelectOption> = modes
            .agents
            .iter()
            .map(|(name, _)| SessionConfigSelectOption::new(name.clone(), name.clone()))
            .collect();
        let current = current_mode.unwrap_or(&modes.default).to_string();
        options.push(
            SessionConfigOption::select("mode", "Session Mode", current, opts)
                .category(SessionConfigOptionCategory::Mode),
        );
    }
    (!options.is_empty()).then_some(options)
}

fn run(options: Options) -> Result<(), String> {
    let script_text = std::fs::read_to_string(&options.script).map_err(|e| format!("could not read script: {e}"))?;
    let script: Script = serde_json::from_str(&script_text).map_err(|e| format!("invalid script JSON: {e}"))?;
    if let Some(env_path) = &options.env_out {
        let snapshot = json!({
            "OPENCODE_CONFIG_CONTENT": std::env::var("OPENCODE_CONFIG_CONTENT").ok(),
            "OPENCODE_DISABLE_FREE_MODELS": std::env::var("OPENCODE_DISABLE_FREE_MODELS").ok(),
            "OPENCODE_DISABLE_MODELS_FETCH": std::env::var("OPENCODE_DISABLE_MODELS_FETCH").ok(),
        });
        std::fs::write(env_path, snapshot.to_string())
            .map_err(|e| format!("could not write env snapshot {}: {e}", env_path.display()))?;
    }
    let mut log_file = match &options.log {
        Some(path) => Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|e| format!("could not open log {}: {e}", path.display()))?,
        ),
        None => None,
    };

    let writer = Arc::new(Writer::new(script.explicit_nulls));
    let picker = Arc::new(Mutex::new(ScriptPicker {
        turns: script.turns.clone(),
        next_unmatched: 0,
    }));
    let active_cancels: Arc<Mutex<HashMap<String, Arc<CancelFlag>>>> = Arc::new(Mutex::new(HashMap::new()));
    let session_models: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let session_modes: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let session_efforts: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let commands_update = (!script.commands.is_empty()).then(|| {
        AvailableCommandsUpdate::new(
            script
                .commands
                .iter()
                .map(|command| {
                    AvailableCommand::new(command.name.clone(), command.description.clone()).input(
                        command
                            .hint
                            .clone()
                            .map(|hint| AvailableCommandInput::Unstructured(UnstructuredCommandInput::new(hint))),
                    )
                })
                .collect(),
        )
    });
    let always: Arc<Mutex<HashMap<String, Vec<String>>>> = Arc::new(Mutex::new(HashMap::new()));
    let steers: Arc<Steers> = Arc::new(Mutex::new(HashMap::new()));
    let replies: Arc<Replies> = Arc::new(Mutex::new(HashMap::new()));
    let next_request = Arc::new(AtomicU64::new(0));
    let modes = Arc::new(Modes::read(script.modes, script.mode_values.as_deref()));
    let next_session = AtomicU64::new(1);
    let free_disabled = std::env::var("OPENCODE_DISABLE_FREE_MODELS").ok().as_deref() == Some("1");
    let default_model = script
        .default_model
        .clone()
        .or_else(|| script.models.as_ref().and_then(|m| m.first().map(|x| x.id.clone())));

    let stdin = io::stdin();
    let reader = BufReader::new(stdin.lock());
    let mut workers = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        append_log(&mut log_file, trimmed);

        let Ok(raw) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        let Some(method) = raw.get("method").and_then(Value::as_str) else {
            // An answer to one of the stub's own requests.
            if let Some(id) = raw.get("id")
                && let Some(waiting) = replies
                    .lock()
                    .unwrap()
                    .remove(&id.to_string().trim_matches('"').to_string())
            {
                let _ = waiting.send(raw.clone());
            }
            continue;
        };

        // Notification (no "id")
        if raw.get("id").is_none() {
            if method == AGENT_METHOD_NAMES.session_cancel
                && let Some(params) = raw.get("params")
                && let Ok(cancel) = serde_json::from_value::<CancelNotification>(params.clone())
                && let Some(flag) = active_cancels
                    .lock()
                    .unwrap()
                    .get(cancel.session_id.0.as_ref())
                    .cloned()
            {
                flag.cancel();
            }
            continue;
        }

        let Ok(req) = serde_json::from_value::<Request<Value>>(raw) else {
            continue;
        };
        let id = req.id;
        let params = req.params.unwrap_or(Value::Null);

        if req.method.as_ref() == AGENT_METHOD_NAMES.initialize {
            if let Some(err) = &script.initialize_error {
                writer.send_error(id, AcpError::new(err.code, err.message.clone()));
                continue;
            }
            match serde_json::from_value::<InitializeRequest>(params) {
                Ok(_init) => {
                    let mut session_caps = SessionCapabilities::new().close(SessionCloseCapabilities::new());
                    if script.delete_session {
                        session_caps =
                            session_caps.delete(agent_client_protocol_schema::v1::SessionDeleteCapabilities::new());
                    }
                    let caps = AgentCapabilities::new()
                        .load_session(script.load_session)
                        .session_capabilities(session_caps);
                    let resp = InitializeResponse::new(ProtocolVersion::V1)
                        .agent_capabilities(caps)
                        .agent_info(Implementation::new("crowbar-stub-harness", "0.1.0"));
                    let mut resp = serde_json::to_value(resp).expect("serialize initialize response");
                    if script.steering {
                        // Where OpenCode 2.4.1 says it, beside the
                        // capabilities ACP's first version names.
                        resp["_meta"] = json!({ "steering": { "supported": true } });
                    }
                    writer.send_result(id, resp);
                }
                Err(e) => writer.send_error(id, AcpError::invalid_params().data(json!(e.to_string()))),
            }
        } else if req.method.as_ref() == AGENT_METHOD_NAMES.session_new {
            thread::sleep(Duration::from_millis(script.new_session_delay_ms));
            if script.no_models_when_free_disabled && free_disabled {
                writer.send_error(
                    id,
                    AcpError::new(-32603, "Internal error: No models are available")
                        .data(json!({ "errorName": "CatalogNotReady" })),
                );
                continue;
            }
            if let Some(err) = &script.new_session_error {
                writer.send_error(id, AcpError::new(err.code, err.message.clone()));
                continue;
            }
            match serde_json::from_value::<NewSessionRequest>(params) {
                Ok(_new_req) => {
                    let n = next_session.fetch_add(1, Ordering::Relaxed);
                    let sid = script
                        .session_id
                        .clone()
                        .unwrap_or_else(|| format!("{}{n}", script.session_prefix));
                    if script.emit_catalog_update_on_new_session {
                        let existing: Vec<_> = {
                            let models_map = session_models.lock().unwrap();
                            let modes_map = session_modes.lock().unwrap();
                            let efforts_map = session_efforts.lock().unwrap();
                            models_map
                                .keys()
                                .filter(|existing_sid| existing_sid.as_str() != sid.as_str())
                                .map(|existing_sid| {
                                    (
                                        existing_sid.clone(),
                                        models_map.get(existing_sid).cloned(),
                                        modes_map.get(existing_sid).cloned(),
                                        efforts_map.get(existing_sid).cloned(),
                                    )
                                })
                                .collect()
                        };
                        for (existing_sid, m, mo, ef) in existing {
                            if let Some(opts) = build_config_options(
                                script.models.as_deref(),
                                m.as_deref(),
                                &modes,
                                mo.as_deref(),
                                ef.as_deref(),
                            ) {
                                writer.send_update(
                                    &SessionId::new(existing_sid),
                                    SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(opts)),
                                );
                            }
                        }
                    }
                    if let Some(ref dm) = default_model {
                        session_models.lock().unwrap().insert(sid.clone(), dm.clone());
                    }
                    session_modes.lock().unwrap().insert(sid.clone(), modes.default.clone());
                    let mut resp = NewSessionResponse::new(SessionId::new(sid.clone()));
                    if let Some(opts) =
                        build_config_options(script.models.as_deref(), default_model.as_deref(), &modes, None, None)
                    {
                        resp = resp.config_options(opts);
                    }
                    writer.send_result(id, resp);
                    if let Some(update) = &commands_update {
                        writer.send_update(
                            &SessionId::new(sid),
                            SessionUpdate::AvailableCommandsUpdate(update.clone()),
                        );
                    } else if script.emit_available_commands {
                        writer.send_update(
                            &SessionId::new(sid),
                            SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(Vec::new())),
                        );
                    }
                }
                Err(e) => writer.send_error(id, AcpError::invalid_params().data(json!(e.to_string()))),
            }
        } else if req.method.as_ref() == AGENT_METHOD_NAMES.session_delete {
            if let Some(err) = &script.delete_session_error {
                writer.send_error(id, AcpError::new(err.code, err.message.clone()));
            } else if script.delete_session {
                match serde_json::from_value::<agent_client_protocol_schema::v1::DeleteSessionRequest>(params) {
                    Ok(req) => {
                        session_models.lock().unwrap().remove(req.session_id.0.as_ref());
                        let delay = Duration::from_millis(script.delete_delay_ms);
                        if delay.is_zero() {
                            writer.send_result(id, json!({}));
                        } else {
                            let writer = writer.clone();
                            workers.push(thread::spawn(move || {
                                thread::sleep(delay);
                                writer.send_result(id, json!({}));
                            }));
                        }
                    }
                    Err(e) => writer.send_error(id, AcpError::invalid_params().data(json!(e.to_string()))),
                }
            } else {
                writer.send_error(id, AcpError::method_not_found());
            }
        } else if req.method.as_ref() == AGENT_METHOD_NAMES.session_close {
            match serde_json::from_value::<CloseSessionRequest>(params) {
                Ok(close_req) => {
                    session_models.lock().unwrap().remove(close_req.session_id.0.as_ref());
                    session_efforts.lock().unwrap().remove(close_req.session_id.0.as_ref());
                    writer.send_result(id, CloseSessionResponse::new());
                }
                Err(e) => writer.send_error(id, AcpError::invalid_params().data(json!(e.to_string()))),
            }
        } else if req.method.as_ref() == AGENT_METHOD_NAMES.session_set_config_option {
            match serde_json::from_value::<SetSessionConfigOptionRequest>(params) {
                Ok(set_req) => {
                    let sid = set_req.session_id.0.as_ref().to_string();
                    let val = set_req
                        .value
                        .as_value_id()
                        .map(|v| v.0.as_ref().to_string())
                        .unwrap_or_default();
                    if set_req.config_id.0.as_ref() == "mode" {
                        let refused = script.refuse_mode.as_deref() == Some(val.as_str());
                        if refused && let Some(child) = &script.refusal_rides_with {
                            let error = JsonRpcMessage::wrap(Response::<Value, AcpError>::new(
                                id,
                                Err(AcpError::invalid_params().data(json!("unknown mode"))),
                            ));
                            writer.send_together(&[
                                serde_json::to_value(error).expect("serialize error response"),
                                json!({
                                    "jsonrpc": "2.0",
                                    "method": "opencode/session/child_update",
                                    "params": {
                                        "rootSessionId": sid,
                                        "childSessionId": child,
                                        "parentSessionId": sid,
                                        "depth": 1,
                                        "type": "status",
                                        "status": "running",
                                    },
                                }),
                            ]);
                            continue;
                        }
                        if !modes.offered || !modes.has(&val) || refused {
                            writer.send_error(id, AcpError::invalid_params().data(json!("unknown mode")));
                            continue;
                        }
                        // The mode takes at once; the answer may come later.
                        session_modes.lock().unwrap().insert(sid.clone(), val.clone());
                        let model = session_models.lock().unwrap().get(&sid).cloned();
                        let effort = session_efforts.lock().unwrap().get(&sid).cloned();
                        let mut opts = build_config_options(
                            script.models.as_deref(),
                            model.as_deref(),
                            &modes,
                            Some(&val),
                            effort.as_deref(),
                        )
                        .unwrap_or_default();
                        if script.partial_config_replies {
                            opts.retain(|o| o.id.0.as_ref() == "mode");
                        }
                        if script.mode_update_first {
                            writer.send_update(
                                &set_req.session_id,
                                SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(opts.clone())),
                            );
                        }
                        let delay = Duration::from_millis(script.set_config_delay_ms);
                        let writer = writer.clone();
                        let rides = script.mode_reply_rides_with_ask.clone();
                        let rides_text = script.mode_reply_rides_with_text.clone();
                        let parent = sid.clone();
                        let respond = move || {
                            thread::sleep(delay);
                            if let Some(said) = rides_text {
                                let reply =
                                    JsonRpcMessage::wrap(Response::<SetSessionConfigOptionResponse, AcpError>::new(
                                        id,
                                        Ok(SetSessionConfigOptionResponse::new(opts)),
                                    ));
                                writer.send_together(&[
                                    serde_json::to_value(reply).expect("serialize response"),
                                    json!({
                                        "jsonrpc": "2.0",
                                        "method": CLIENT_METHOD_NAMES.session_update,
                                        "params": {
                                            "sessionId": parent,
                                            "update": {
                                                "sessionUpdate": "agent_message_chunk",
                                                "content": { "type": "text", "text": said }
                                            }
                                        }
                                    }),
                                ]);
                                return;
                            }
                            let Some(child) = rides else {
                                writer.send_result(id, SetSessionConfigOptionResponse::new(opts));
                                return;
                            };
                            let reply =
                                JsonRpcMessage::wrap(Response::<SetSessionConfigOptionResponse, AcpError>::new(
                                    id,
                                    Ok(SetSessionConfigOptionResponse::new(opts)),
                                ));
                            writer.send_together(&[
                                serde_json::to_value(reply).expect("serialize response"),
                                json!({
                                    "jsonrpc": "2.0",
                                    "id": 9000,
                                    "method": CLIENT_METHOD_NAMES.session_request_permission,
                                    "params": {
                                        "sessionId": parent,
                                        "toolCall": {
                                            "toolCallId": format!("{child}:call_rides"),
                                            "title": "Rides: Read /tmp/rides",
                                            "kind": "read",
                                            "status": "pending",
                                            "rawInput": { "path": "/tmp/rides" },
                                            "_meta": { "opencode/child-session": {
                                                "id": child, "parentID": parent, "depth": 1, "title": "Rides"
                                            } }
                                        },
                                        "options": [
                                            { "optionId": "once", "name": "Allow once", "kind": "allow_once" },
                                            { "optionId": "reject", "name": "Reject", "kind": "reject_once" }
                                        ]
                                    }
                                }),
                            ]);
                        };
                        if delay.is_zero() {
                            respond();
                        } else {
                            workers.push(thread::spawn(respond));
                        }
                        continue;
                    }
                    if set_req.config_id.0.as_ref() == "effort" {
                        let model = session_models.lock().unwrap().get(&sid).cloned();
                        let offered = effort_levels(script.models.as_deref(), model.as_deref()).contains(&val);
                        if !offered || script.refuse_effort.as_deref() == Some(val.as_str()) {
                            writer.send_error(id, AcpError::invalid_params().data(json!("unknown effort")));
                            continue;
                        }
                        let before = session_efforts.lock().unwrap().insert(sid.clone(), val.clone());
                        let mode = session_modes.lock().unwrap().get(&sid).cloned();
                        let options = |effort: Option<&str>| {
                            build_config_options(
                                script.models.as_deref(),
                                model.as_deref(),
                                &modes,
                                mode.as_deref(),
                                effort,
                            )
                            .unwrap_or_default()
                        };
                        let mut opts = options(Some(&val));
                        if script.partial_config_replies {
                            opts.retain(|o| o.id.0.as_ref() == "effort");
                        }
                        // The answer that lags says the effort before, and
                        // the update after it the effort now.
                        let lagging = script.effort_answer_lags.then(|| options(before.as_deref()));
                        let delay = Duration::from_millis(script.set_config_delay_ms);
                        let writer = writer.clone();
                        let session = SessionId::new(sid.clone());
                        let respond = move || {
                            thread::sleep(delay);
                            match lagging {
                                Some(old) => {
                                    writer.send_result(id, SetSessionConfigOptionResponse::new(old));
                                    writer.send_update(
                                        &session,
                                        SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(opts)),
                                    );
                                }
                                None => writer.send_result(id, SetSessionConfigOptionResponse::new(opts)),
                            }
                        };
                        if delay.is_zero() {
                            respond();
                        } else {
                            workers.push(thread::spawn(respond));
                        }
                        continue;
                    }
                    let valid = script
                        .models
                        .as_ref()
                        .is_some_and(|list| list.iter().any(|m| m.id == val));
                    if set_req.config_id.0.as_ref() != "model" || !valid {
                        writer.send_error(id, AcpError::invalid_params().data(json!("unknown model")));
                    } else {
                        let delay = Duration::from_millis(script.set_config_delay_ms);
                        let writer = writer.clone();
                        let models = script.models.clone();
                        let session_models = Arc::clone(&session_models);
                        let mode = session_modes.lock().unwrap().get(&sid).cloned();
                        let effort = session_efforts.lock().unwrap().get(&sid).cloned();
                        let modes = Arc::clone(&modes);
                        let partial = script.partial_config_replies;
                        let respond = move || {
                            if !delay.is_zero() {
                                thread::sleep(delay);
                            }
                            session_models.lock().unwrap().insert(sid, val.clone());
                            let mut opts = build_config_options(
                                models.as_deref(),
                                Some(&val),
                                &modes,
                                mode.as_deref(),
                                effort.as_deref(),
                            )
                            .unwrap_or_default();
                            if partial {
                                opts.retain(|o| o.id.0.as_ref() == "model");
                            }
                            writer.send_result(id, SetSessionConfigOptionResponse::new(opts));
                        };
                        if delay.is_zero() {
                            respond();
                        } else {
                            workers.push(thread::spawn(respond));
                        }
                    }
                }
                Err(e) => writer.send_error(id, AcpError::invalid_params().data(json!(e.to_string()))),
            }
        } else if req.method.as_ref() == AGENT_METHOD_NAMES.session_load {
            match serde_json::from_value::<LoadSessionRequest>(params) {
                Ok(load_req) => {
                    if !script.load_session {
                        writer.send_error(id, AcpError::method_not_found());
                    } else {
                        let (writer, replay) = (writer.clone(), script.load_replay.clone());
                        let delay = Duration::from_millis(script.load_delay_ms);
                        let sid = load_req.session_id.0.as_ref().to_string();
                        let cur = session_models
                            .lock()
                            .unwrap()
                            .get(&sid)
                            .cloned()
                            .or_else(|| default_model.clone());
                        // A session this process did not begin is at the
                        // default Model, as a new one is.
                        if let Some(model) = &cur {
                            session_models
                                .lock()
                                .unwrap()
                                .entry(sid.clone())
                                .or_insert_with(|| model.clone());
                        }
                        let mode = session_modes.lock().unwrap().get(&sid).cloned();
                        let effort = session_efforts.lock().unwrap().get(&sid).cloned();
                        let opts = build_config_options(
                            script.models.as_deref(),
                            cur.as_deref(),
                            &modes,
                            mode.as_deref(),
                            effort.as_deref(),
                        );
                        let commands = commands_update.clone();
                        workers.push(thread::spawn(move || {
                            thread::sleep(delay);
                            for step in &replay {
                                let _ = emit_step(&writer, &load_req.session_id, step, None, cur.as_deref());
                            }
                            let mut resp = LoadSessionResponse::new();
                            if let Some(opts) = opts {
                                resp = resp.config_options(opts);
                            }
                            writer.send_result(id, resp);
                            if let Some(update) = commands {
                                writer
                                    .send_update(&load_req.session_id, SessionUpdate::AvailableCommandsUpdate(update));
                            }
                        }));
                    }
                }
                Err(e) => writer.send_error(id, AcpError::invalid_params().data(json!(e.to_string()))),
            }
        } else if script.steering && req.method.as_ref() == "_session/steering" {
            // OpenCode 2.4.1's own extension: the message goes into the
            // session's running prompt, which takes it at its next step.
            let session = params
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let text: String = params
                .get("prompt")
                .and_then(Value::as_array)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if script.steer_exits {
                std::process::exit(1);
            }
            let running = active_cancels.lock().unwrap().contains_key(&session);
            let refused = script.steer_refused || script.steer_refused_for.as_ref().is_some_and(|t| text.contains(t));
            let answer = |running: bool| {
                if running && !refused {
                    json!({ "outcome": "injected" })
                } else {
                    json!({ "outcome": "promptRequired", "reason": "noRunningTurn" })
                }
            };
            if script.steer_delay_ms > 0 {
                // Answered later, by whether a prompt runs then.
                let (writer, active_cancels, delay) = (writer.clone(), active_cancels.clone(), script.steer_delay_ms);
                let outcome = answer(true);
                workers.push(thread::spawn(move || {
                    thread::sleep(Duration::from_millis(delay));
                    let running = active_cancels.lock().unwrap().contains_key(&session);
                    let outcome = if running {
                        outcome
                    } else {
                        json!({ "outcome": "promptRequired", "reason": "noRunningTurn" })
                    };
                    writer.send_result(id, outcome);
                }));
            } else {
                if running && !refused {
                    steers.lock().unwrap().entry(session).or_default().push_back(text);
                }
                writer.send_result(id, answer(running));
            }
        } else if req.method.as_ref() == AGENT_METHOD_NAMES.session_prompt {
            let prompt_req = match serde_json::from_value::<PromptRequest>(params) {
                Ok(p) => p,
                Err(e) => {
                    writer.send_error(id, AcpError::invalid_params().data(json!(e.to_string())));
                    continue;
                }
            };
            let text = prompt_text(&prompt_req);
            let turn_script = picker.lock().unwrap().pick(&text);
            let cancel = Arc::new(CancelFlag::new());
            let sid_key = prompt_req.session_id.0.as_ref().to_string();
            let model = session_models.lock().unwrap().get(&sid_key).cloned();
            let effort = session_efforts.lock().unwrap().get(&sid_key).cloned();
            active_cancels.lock().unwrap().insert(sid_key.clone(), cancel.clone());

            let writer = writer.clone();
            let active_cancels = active_cancels.clone();
            let session_id = prompt_req.session_id;
            let steers = Arc::clone(&steers);
            let (modes, session_modes, always, replies, next_request) = (
                Arc::clone(&modes),
                Arc::clone(&session_modes),
                Arc::clone(&always),
                Arc::clone(&replies),
                Arc::clone(&next_request),
            );
            let script_models = script.models.clone();
            workers.push(thread::spawn(move || {
                let asking = Asking {
                    modes: &modes,
                    session_modes: &session_modes,
                    always: &always,
                    replies: &replies,
                    next_request: &next_request,
                };
                // A harness that takes no notice of a cancel never sees one.
                let unheard = CancelFlag::new();
                let heard = if turn_script.ignore_cancel { &unheard } else { &*cancel };
                let mut was_cancelled = false;
                // `{effort}` in a text step is the effort the session is at.
                let levels = effort_levels(script_models.as_deref(), model.as_deref());
                let at_effort = match &effort {
                    Some(effort) if levels.contains(effort) => effort.clone(),
                    _ if levels.is_empty() => "none".to_string(),
                    _ => DEFAULT_EFFORT.to_string(),
                };
                let steps: Vec<Step> = turn_script
                    .steps
                    .iter()
                    .map(|step| match step {
                        Step::Text { text } => Step::Text {
                            text: text.replace("{effort}", &at_effort),
                        },
                        other => other.clone(),
                    })
                    .collect();
                let mut subagent_titles: HashMap<String, String> = HashMap::new();
                for step in &steps {
                    if heard.is_cancelled() {
                        was_cancelled = true;
                        break;
                    }
                    match step {
                        Step::Ask(step) => {
                            let asked = ask(&writer, &session_id, &asking, Some(heard), step);
                            if asked == Asked::Ended {
                                was_cancelled = true;
                                break;
                            }
                        }
                        Step::SubagentStart { .. }
                        | Step::SubagentText { .. }
                        | Step::SubagentAct { .. }
                        | Step::SubagentAsk { .. }
                        | Step::SubagentEnd { .. }
                        | Step::SubagentStall { .. } => {
                            run_subagent_step(
                                &writer,
                                &session_id,
                                &asking,
                                &active_cancels,
                                &mut subagent_titles,
                                step,
                            );
                        }
                        Step::AwaitSteer { text } => match await_steer(&steers, &sid_key, heard) {
                            Some(steer) => {
                                let step = Step::Text {
                                    text: text.replace("{steer}", &steer),
                                };
                                let _ = emit_step(&writer, &session_id, &step, Some(heard), model.as_deref());
                            }
                            None => {
                                was_cancelled = true;
                                break;
                            }
                        },
                        Step::ModeChange { mode } => {
                            let sid = session_id.0.as_ref().to_string();
                            session_modes.lock().unwrap().insert(sid, mode.clone());
                            let opts = build_config_options(
                                script_models.as_deref(),
                                model.as_deref(),
                                &modes,
                                Some(mode),
                                effort.as_deref(),
                            )
                            .unwrap_or_default();
                            writer.send_update(
                                &session_id,
                                SessionUpdate::ConfigOptionUpdate(ConfigOptionUpdate::new(opts)),
                            );
                        }
                        _ => {
                            if emit_step(&writer, &session_id, step, Some(heard), model.as_deref()) {
                                was_cancelled = true;
                                break;
                            }
                        }
                    }
                }
                if turn_script.crash {
                    std::process::exit(1);
                }
                if !was_cancelled && turn_script.stall {
                    heard.wait_cancelled();
                    was_cancelled = true;
                }
                if was_cancelled && turn_script.exit_on_cancel {
                    std::process::exit(0);
                }
                active_cancels.lock().unwrap().remove(&sid_key);
                // What no step took goes with the prompt.
                steers.lock().unwrap().remove(&sid_key);

                if was_cancelled && turn_script.cancel_with_error {
                    writer.send_error(id, AcpError::request_cancelled());
                } else if was_cancelled && turn_script.error.is_none() {
                    writer.send_result(id, PromptResponse::new(StopReason::Cancelled));
                } else if let Some(err) = &turn_script.error {
                    writer.send_error(id, AcpError::new(err.code, err.message.clone()));
                } else {
                    writer.send_result(id, PromptResponse::new(parse_stop_reason(&turn_script.stop_reason)));
                }

                // After the turn, as a subagent of OpenCode's goes on: an
                // `ask` or subagent step here waits for its answer or cancel as
                // one in the turn does.
                for step in &turn_script.after {
                    match step {
                        Step::Ask(step) => {
                            let _ = ask(&writer, &session_id, &asking, None, step);
                        }
                        Step::SubagentStart { .. }
                        | Step::SubagentText { .. }
                        | Step::SubagentAct { .. }
                        | Step::SubagentAsk { .. }
                        | Step::SubagentEnd { .. }
                        | Step::SubagentStall { .. } => {
                            run_subagent_step(
                                &writer,
                                &session_id,
                                &asking,
                                &active_cancels,
                                &mut subagent_titles,
                                step,
                            );
                        }
                        _ => {
                            let _ = emit_step(&writer, &session_id, step, None, model.as_deref());
                        }
                    }
                }
            }));
        } else {
            writer.send_error(id, AcpError::method_not_found());
        }
    }

    for worker in workers {
        let _ = worker.join();
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(options) = parse_options(&args) else {
        eprintln!("usage: crowbar-stub-harness --script <path> [--log <path>]");
        return ExitCode::from(2);
    };
    if let Err(err) = run(options) {
        eprintln!("crowbar-stub-harness: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
