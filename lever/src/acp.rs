//! Lever's Agent Client Protocol (ACP) v1 client over a harness child's
//! standard input and output, using a non-blocking JSON-RPC 2.0 line loop and
//! the pinned `agent-client-protocol-schema` v1 types.

use crate::channel::LEVER_VERSION;
use crate::opencode;
use crate::sys;
use agent_client_protocol_schema::v1::{
    AGENT_METHOD_NAMES, CLIENT_METHOD_NAMES, CancelNotification, CancelRequestNotification, CloseSessionRequest,
    ContentBlock, Error as AcpError, ErrorCode, Implementation, InitializeRequest, InitializeResponse, JsonRpcMessage,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, Notification,
    PROTOCOL_LEVEL_METHOD_NAMES, PermissionOptionKind, PromptRequest, PromptResponse, Request, RequestId,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse, Response, SelectedPermissionOutcome,
    SessionConfigId, SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOptions,
    SessionId, SessionUpdate, SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, StopReason,
    ToolCallContent, ToolCallStatus, ToolKind,
};
use agent_client_protocol_schema::{MaybeUndefined, ProtocolVersion};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
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
}

/// One Model option offered by a harness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOption {
    pub id: String,
    pub name: String,
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
    },
    ModeSet {
        thread_id: String,
        turn: u64,
    },
    ModeSetFailed {
        thread_id: String,
        turn: u64,
        message: String,
    },
    /// The harness asks the person before it acts.
    PermissionRequested {
        thread_id: String,
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
    Exited,
}

enum PendingRpc {
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
    SetModel {
        thread_id: String,
        turn: Option<u64>,
        session_id: String,
        model: String,
    },
    SetMode {
        thread_id: String,
        turn: u64,
        session_id: String,
        mode: String,
    },
    Prompt {
        thread_id: String,
        turn: u64,
    },
}

pub struct AcpHarness {
    child: Child,
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
    supports_close_session: bool,
    probing_models: bool,
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
    loading_sessions: HashSet<String>,
    /// Why Lever is ending this harness, when the harness did not end itself.
    fault: Option<&'static str>,
    /// Tool call IDs seen per thread, used only to redact harness error messages
    /// and unrecognized updates without altering normal model output.
    thread_tool_ids: HashMap<String, HashSet<String>>,
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

        let mut harness = Self {
            child,
            stdin: Some(stdin),
            stdout,
            write_buf: Vec::new(),
            read_buf: Vec::new(),
            scanned: 0,
            next_id: 1,
            initialized: false,
            supports_load_session: false,
            supports_close_session: false,
            probing_models: false,
            model_config_id: None,
            mode_config_id: None,
            pending: HashMap::new(),
            permissions: HashMap::new(),
            next_permission: 1,
            session_modes: HashMap::new(),
            thread_to_session: HashMap::new(),
            session_to_thread: HashMap::new(),
            session_models: HashMap::new(),
            loading_sessions: HashSet::new(),
            fault: None,
            thread_tool_ids: HashMap::new(),
        };

        let init = InitializeRequest::new(ProtocolVersion::V1)
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

    /// Lets go of a thread's session, which Lever will not use.
    pub fn forget_session(&mut self, thread_id: &str) {
        if let Some(session_id) = self.thread_to_session.remove(thread_id) {
            self.session_to_thread.remove(&session_id);
            self.session_models.remove(&session_id);
            self.session_modes.remove(&session_id);
        }
    }

    pub fn session_current_model(&self, session_id: &str) -> Option<&str> {
        self.session_models.get(session_id).map(String::as_str)
    }

    /// A session's mode, as Lever's mode ID, when it is one of Lever's.
    pub fn session_current_mode(&self, session_id: &str) -> Option<&str> {
        self.session_modes.get(session_id).map(String::as_str)
    }

    /// Whether the harness has a mode selector Lever can set.
    pub fn has_modes(&self) -> bool {
        self.mode_config_id.is_some()
    }

    /// Sets an active session's mode, one of Lever's, through
    /// `session/set_config_option`.
    pub fn set_session_mode(&mut self, thread_id: &str, turn: u64, session_id: &str, mode: &str) {
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
            || self.loading_sessions.contains(sid);
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

    /// Makes `session_id` the session of `thread_id`, and of no other. Says
    /// no when another thread holds it or is loading it: two threads on one
    /// session would have each other's answers. A thread that loaded a
    /// session is no longer loading it when it comes here.
    fn bind_session(&mut self, thread_id: &str, session_id: &str) -> bool {
        if self.loading_sessions.contains(session_id)
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

    /// Sends a `session/prompt` request for an active session, prepending
    /// desktop context when present.
    pub fn send_prompt(&mut self, thread_id: &str, turn: u64, session_id: &str, context: Option<&str>, text: &str) {
        let mut blocks = Vec::new();
        if let Some(ctx) = context {
            blocks.push(ContentBlock::from(format!("{ctx}\n")));
        }
        blocks.push(ContentBlock::from(text.to_string()));
        let req = PromptRequest::new(SessionId::new(session_id.to_string()), blocks);
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

    /// Sends a `session/cancel` notification for an active session.
    pub fn cancel_session(&mut self, session_id: &str) {
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
        sys::kill_all_under_lever();
        let _ = self.child.kill();
        let _ = self.child.wait();
        // What it left behind ended too, and is Lever's to wait for.
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            sys::reap_orphans(None);
            if sys::descendants(std::process::id()).is_empty() || Instant::now() >= deadline {
                break;
            }
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
            if value.get("method").and_then(Value::as_str) == Some(PROTOCOL_LEVEL_METHOD_NAMES.cancel_request) {
                self.handle_cancel_request(value.get("params"), events);
                return;
            }
            self.handle_notification(&value, events);
            return;
        }

        if has_id {
            self.handle_response(value, events);
        }
    }

    /// Takes a `session/request_permission` to the person, as a request in
    /// the thread whose session asks it. One for a session Lever holds for no
    /// thread is answered `cancelled` at once: nobody would see it.
    fn handle_permission_request(&mut self, rpc_id: RequestId, params: Option<&Value>, events: &mut Vec<AcpEvent>) {
        let request = params.and_then(|p| serde_json::from_value::<RequestPermissionRequest>(p.clone()).ok());
        let thread_id = request.as_ref().and_then(|r| {
            let sid = r.session_id.0.as_ref();
            (!self.loading_sessions.contains(sid))
                .then(|| self.session_to_thread.get(sid).cloned())
                .flatten()
        });
        let (Some(request), Some(thread_id)) = (request, thread_id) else {
            self.send_result_response(
                rpc_id,
                RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
            );
            let _ = self.on_writable();
            return;
        };
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
        let title = call
            .fields
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| path.clone())
            .or_else(|| detail.as_ref().and_then(|d| d.lines().next().map(str::to_string)))
            .unwrap_or_else(|| "Act".to_string());
        let handle = self.next_permission;
        self.next_permission += 1;
        self.permissions.insert(handle, Permission { rpc_id, options });
        events.push(AcpEvent::PermissionRequested {
            thread_id,
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
                // The harness says the turn was cancelled, with an error
                // where ACP has `StopReason::Cancelled`. Any other error is
                // a failure, whether or not a stop was asked for.
                PendingRpc::Prompt { thread_id, turn } if cancelled => {
                    events.push(AcpEvent::PromptFinished {
                        thread_id,
                        turn,
                        outcome: TurnOutcome::Stopped,
                    });
                }
                PendingRpc::Prompt { thread_id, turn } => {
                    let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                    events.push(AcpEvent::PromptFinished {
                        thread_id,
                        turn,
                        outcome: TurnOutcome::Failed(message),
                    });
                }
            }
            return;
        }

        let result = value.get("result").cloned().unwrap_or(Value::Null);
        match pending {
            PendingRpc::Initialize => match serde_json::from_value::<InitializeResponse>(result) {
                Ok(resp) if resp.protocol_version == ProtocolVersion::V1 => {
                    self.initialized = true;
                    self.supports_load_session = resp.agent_capabilities.load_session;
                    self.supports_close_session = resp.agent_capabilities.session_capabilities.close.is_some();
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
                        if self.supports_close_session && !resp.session_id.0.is_empty() {
                            let close = CloseSessionRequest::new(resp.session_id);
                            let cid = self.send_request(AGENT_METHOD_NAMES.session_close, &close);
                            self.pending.insert(cid, PendingRpc::CloseProbe);
                            let _ = self.on_writable();
                        }
                        events.push(AcpEvent::ModelsProbed(Ok(catalog)));
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
            PendingRpc::Prompt { thread_id, turn } => match serde_json::from_value::<PromptResponse>(result) {
                Ok(resp) => {
                    let outcome = match resp.stop_reason {
                        StopReason::EndTurn => TurnOutcome::Done,
                        StopReason::Cancelled => TurnOutcome::Stopped,
                        StopReason::Refusal => TurnOutcome::Failed("The harness refused to continue.".to_string()),
                        StopReason::MaxTokens => TurnOutcome::Failed("The turn reached its token limit.".to_string()),
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
            },
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
        // Ignore history replayed by the harness while `session/load` is in flight:
        // Lever already owns the authoritative record in SQLite.
        if self.loading_sessions.contains(session_id) {
            return;
        }
        let Some(thread_id) = self.session_to_thread.get(session_id).cloned() else {
            return;
        };
        let Some(raw_update) = params.get("update") else {
            return;
        };

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
            Ok(SessionUpdate::SessionInfoUpdate(info)) => {
                if let MaybeUndefined::Value(title) = info.title {
                    let clean = title.trim().to_string();
                    if !clean.is_empty() {
                        events.push(AcpEvent::TitleUpdated {
                            thread_id,
                            title: clean,
                        });
                    }
                }
            }
            Ok(SessionUpdate::ConfigOptionUpdate(cfg)) => {
                let before = self.session_modes.get(session_id).cloned();
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
                if catalog.is_some() || current_mode.is_some() {
                    events.push(AcpEvent::CatalogUpdated {
                        thread_id,
                        catalog,
                        current_model,
                        current_mode,
                    });
                }
            }
            // Known ACP v1 updates that Lever does not show as thread notes:
            // thoughts stream a few tokens at a time, available commands arrive
            // on every new session, and modes, plans, usage, and notices are
            // either dropped on purpose or handled in their own slices.
            Ok(
                SessionUpdate::UserMessageChunk(_)
                | SessionUpdate::AgentThoughtChunk(_)
                | SessionUpdate::AvailableCommandsUpdate(_)
                | SessionUpdate::CurrentModeUpdate(_)
                | SessionUpdate::UsageUpdate(_)
                | SessionUpdate::Plan(_)
                | SessionUpdate::PlanUpdate(_)
                | SessionUpdate::PlanRemoved(_)
                | SessionUpdate::Notice(_)
                | SessionUpdate::CompactionUpdate(_)
                | SessionUpdate::CompactionSummaryChunk(_)
                | SessionUpdate::SubagentUpdate(_)
                | SessionUpdate::SessionMessage(_)
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
        } else {
            ids.extend(self.session_to_thread.keys().map(String::as_str));
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

/// Extracts the Model selector from ACP session configuration options.
fn extract_model_catalog(options: &[SessionConfigOption]) -> Option<ModelCatalog> {
    let opt = options
        .iter()
        .find(|o| o.category == Some(SessionConfigOptionCategory::Model))
        .or_else(|| options.iter().find(|o| o.id.0.as_ref() == "model"))?;
    let SessionConfigKind::Select(select) = &opt.kind else {
        return None;
    };
    let mut models = Vec::new();
    match &select.options {
        SessionConfigSelectOptions::Ungrouped(items) => {
            for item in items {
                models.push(ModelOption {
                    id: item.value.0.as_ref().to_string(),
                    name: item.name.clone(),
                });
            }
        }
        SessionConfigSelectOptions::Grouped(groups) => {
            for group in groups {
                for item in &group.options {
                    models.push(ModelOption {
                        id: item.value.0.as_ref().to_string(),
                        name: item.name.clone(),
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
}
