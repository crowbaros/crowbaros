//! Lever's Agent Client Protocol (ACP) v1 adapter implementing [`Harness`].
//!
//! Organized internally into:
//! - [`transport`]: non-blocking stdio pipes, line buffering, JSON-RPC 2.0
//!   framing, and process-tree teardown
//! - [`dialect`]: `AcpDialect` seam isolating OpenCode-specific ACP extensions
//!   from generic ACP v1 harnesses
//! - [`responses`]: per-RPC response handlers
//! - [`updates`]: incoming notifications, permission requests, subagent routing,
//!   and ID redaction
//! - [`history`]: `session/load` history replay accumulator
//! - [`format`]: model/mode/effort parsing, request diff rendering, redaction
//! - [`discovery`]: one-shot ACP `session/list` discovery

mod dialect;
mod discovery;
mod format;
mod history;
mod responses;
mod transport;
mod updates;

#[cfg(test)]
mod tests;

pub use discovery::{AcpSessionDiscoverer, discoverer_for_harness};

use crate::harness::{
    EffortSelector, ExternalSession, Harness, HarnessCapabilities, HarnessConfig, HarnessEvent, HarnessFactory,
    HarnessLaunchOptions,
};
use crate::mcp::McpServerSpec;
use crate::version::LEVER_VERSION;
use agent_client_protocol_schema::ProtocolVersion;
use agent_client_protocol_schema::v1::{
    AGENT_METHOD_NAMES, CancelNotification, ClientCapabilities, CloseSessionRequest, DeleteSessionRequest,
    Implementation, InitializeRequest, ListSessionsRequest, LoadSessionRequest, NewSessionRequest, PromptRequest,
    RequestId, RequestPermissionOutcome, RequestPermissionResponse, SelectedPermissionOutcome, SessionConfigId,
    SessionConfigOption, SessionId, SetSessionConfigOptionRequest, SubagentCapabilities,
};
use dialect::{AcpDialect, dialect_for_config};
use format::{ModeSelector, extract_effort, extract_modes, model_option, prompt_blocks, to_acp_mcp_servers};
use history::HistoryBuilder;
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::os::fd::RawFd;
use std::path::PathBuf;
use transport::AcpTransport;

/// Factory that spawns [`AcpHarness`] instances behind the [`HarnessFactory`] port.
#[derive(Debug, Default, Clone, Copy)]
pub struct AcpHarnessFactory;

impl HarnessFactory for AcpHarnessFactory {
    fn spawn(&mut self, config: &HarnessConfig, options: HarnessLaunchOptions) -> Result<Box<dyn Harness>, String> {
        Ok(Box::new(AcpHarness::spawn_with_options(config, options)?))
    }
}

pub(super) enum PendingRpc {
    DeleteSession {
        thread_id: String,
        session_id: String,
    },
    Initialize,
    ProbeModels,
    CloseProbe,
    NewSession {
        thread_id: String,
        turn: u64,
        afresh: bool,
        epoch: u64,
    },
    LoadSession {
        thread_id: String,
        turn: u64,
        session_id: String,
        cwd: String,
        mcp_servers: Vec<McpServerSpec>,
        epoch: u64,
    },
    /// Loads an externally-started session via `session/load` and captures its
    /// replayed updates into a [`SessionHistory`](crate::harness::SessionHistory).
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

pub(super) struct PendingSubagentCall {
    pub(super) thread_id: String,
    pub(super) title: Option<String>,
    pub(super) prompt: Option<String>,
    pub(super) child_session_id: Option<String>,
}

/// Where a subagent's prompt came from: its launcher call, or its own
/// session's stream of the message it was given.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PromptFrom {
    Launcher,
    Stream,
}

struct Permission {
    rpc_id: RequestId,
    options: Vec<(String, String)>,
}

/// Live ACP v1 harness adapter.
pub struct AcpHarness {
    transport: AcpTransport,
    dialect: Box<dyn AcpDialect>,
    initialized: bool,
    supports_load_session: bool,
    supports_list_sessions: bool,
    supports_close_session: bool,
    supports_delete_session: bool,
    /// The harness says it can steer a message into a running prompt.
    supports_steering: bool,
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
    /// Home directory of the harness process, used to resolve `~` in request
    /// details identically to `project::resolve_project_path`.
    home: PathBuf,
    /// Per-thread session generation bumped on `forget_session`/`detach_session`
    /// so stale in-flight `session/new` or `session/load` replies from before a
    /// project move are discarded and closed instead of re-binding an old folder.
    session_gen: HashMap<String, u64>,
    thread_to_session: HashMap<String, String>,
    session_to_thread: HashMap<String, String>,
    /// Pre-move root session IDs detached from a thread, kept with the turn
    /// number that last ran on that session until the thread is forgotten or
    /// the session ID is deleted/reassigned, so late subagent updates from the
    /// old session still route to the thread (direct children attach to this
    /// turn; nested children resolve their parent subagent's recorded turn).
    retired_root_to_thread: HashMap<String, (String, Option<u64>)>,
    /// Original turn number recorded for a subagent routed from a retired
    /// pre-move session.
    child_retired_turn: HashMap<String, u64>,
    session_models: HashMap<String, String>,
    /// Each session's effort selector, when its Model has effort levels.
    session_efforts: HashMap<String, EffortSelector>,
    /// ACP configuration option ID for each session's effort selector.
    session_effort_config_ids: HashMap<String, String>,
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
    /// Parent tool calls that launch a subagent, kept so their prompt and
    /// description attach to the spawned child session without exposing the
    /// wrapper tool call or session ID. Keyed by the session that made the
    /// call and the call's ID: a call ID is unique only within its session.
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

impl AcpHarness {
    /// Spawns the configured ACP harness child using the dialect selected for `config`.
    pub fn spawn(config: &HarnessConfig, free_hosted_models: bool) -> Result<Self, String> {
        Self::spawn_with_options(config, HarnessLaunchOptions { free_hosted_models })
    }

    /// Spawns the configured ACP harness child with explicit [`HarnessLaunchOptions`].
    pub fn spawn_with_options(config: &HarnessConfig, options: HarnessLaunchOptions) -> Result<Self, String> {
        let dialect = dialect_for_config(config);
        Self::spawn_with_dialect(config, options, dialect)
    }

    fn spawn_with_dialect(
        config: &HarnessConfig,
        options: HarnessLaunchOptions,
        dialect: Box<dyn AcpDialect>,
    ) -> Result<Self, String> {
        let (env_set, env_remove) = dialect.prepare_spawn_env(config, options);
        let transport = AcpTransport::spawn(&config.name, &config.program, &config.args, &env_set, &env_remove)?;
        let home = config
            .env
            .iter()
            .rev()
            .find(|(k, _)| k == "HOME")
            .map(|(_, v)| PathBuf::from(v))
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| std::env::var("HOME").ok().filter(|s| !s.is_empty()).map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"));

        let mut capabilities = ClientCapabilities::new().subagents(SubagentCapabilities::new());
        if let Some(meta) = dialect.initialize_client_meta() {
            capabilities = capabilities.meta(meta);
        }
        let init = InitializeRequest::new(ProtocolVersion::V1)
            .client_capabilities(capabilities)
            .client_info(Implementation::new("crowbar-lever", LEVER_VERSION));

        let mut harness = Self {
            transport,
            dialect,
            initialized: false,
            supports_load_session: false,
            supports_list_sessions: false,
            supports_close_session: false,
            supports_delete_session: false,
            supports_steering: false,
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
            home,
            session_gen: HashMap::new(),
            thread_to_session: HashMap::new(),
            session_to_thread: HashMap::new(),
            retired_root_to_thread: HashMap::new(),
            child_retired_turn: HashMap::new(),
            session_models: HashMap::new(),
            session_efforts: HashMap::new(),
            session_effort_config_ids: HashMap::new(),
            probe_sessions: VecDeque::new(),
            new_probe_sessions: Vec::new(),
            probing_efforts: HashSet::new(),
            loading_sessions: HashSet::new(),
            history_loads: HashMap::new(),
            thread_tool_ids: HashMap::new(),
            thread_subagent_ids: HashMap::new(),
            child_to_thread: HashMap::new(),
            child_titles: HashMap::new(),
            pending_subagent_calls: HashMap::new(),
            child_prompts: HashMap::new(),
            child_launchers: HashMap::new(),
            transport_failures: HashMap::new(),
        };

        let id = harness.transport.send_request(AGENT_METHOD_NAMES.initialize, &init);
        harness.pending.insert(id, PendingRpc::Initialize);
        let _ = harness.transport.on_writable();
        Ok(harness)
    }

    fn current_session_gen(&self, thread_id: &str) -> u64 {
        self.session_gen.get(thread_id).copied().unwrap_or(0)
    }

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

    fn close_probe(&mut self, session_id: SessionId) {
        if session_id.0.is_empty() {
            return;
        }
        self.remember_probe_session(session_id.0.as_ref().to_string());
        if self.supports_close_session {
            let close = CloseSessionRequest::new(session_id);
            let id = self.transport.send_request(AGENT_METHOD_NAMES.session_close, &close);
            self.pending.insert(id, PendingRpc::CloseProbe);
            let _ = self.transport.on_writable();
        }
    }

    fn take_effort(&mut self, session_id: &str, options: &[SessionConfigOption]) {
        match extract_effort(options) {
            Some((selector, config_id)) => {
                self.session_efforts.insert(session_id.to_string(), selector);
                self.session_effort_config_ids.insert(session_id.to_string(), config_id);
            }
            None if model_option(options).is_some() => {
                self.session_efforts.remove(session_id);
                self.session_effort_config_ids.remove(session_id);
            }
            None => {}
        }
    }

    fn take_modes(&mut self, session_id: &str, options: Option<&[SessionConfigOption]>) -> Option<ModeSelector> {
        let selector = options.and_then(|opts| extract_modes(opts, self.dialect.as_ref()))?;
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

    fn loading_thread(&self, session_id: &str) -> Option<String> {
        self.pending.values().find_map(|pending| match pending {
            PendingRpc::LoadSession {
                thread_id,
                session_id: loading,
                epoch,
                ..
            } if loading == session_id && *epoch == self.current_session_gen(thread_id) => Some(thread_id.clone()),
            _ => None,
        })
    }

    fn thread_loading(&self, thread_id: &str) -> bool {
        let cur = self.current_session_gen(thread_id);
        self.pending.values().any(|pending| {
            matches!(
                pending,
                PendingRpc::LoadSession { thread_id: loading, epoch, .. }
                    if loading == thread_id && *epoch == cur
            )
        })
    }

    fn request_new_session(
        &mut self,
        thread_id: &str,
        turn: u64,
        cwd: &str,
        afresh: bool,
        mcp_servers: &[McpServerSpec],
        epoch: u64,
    ) {
        let req = NewSessionRequest::new(PathBuf::from(cwd))
            .mcp_servers(to_acp_mcp_servers(mcp_servers, self.dialect.as_ref()));
        let id = self.transport.send_request(AGENT_METHOD_NAMES.session_new, &req);
        self.pending.insert(
            id,
            PendingRpc::NewSession {
                thread_id: thread_id.to_string(),
                turn,
                afresh,
                epoch,
            },
        );
        let _ = self.transport.on_writable();
    }

    fn bind_session(&mut self, thread_id: &str, session_id: &str) -> bool {
        if self.loading_sessions.contains(session_id)
            || self.history_loads.contains_key(session_id)
            || self
                .session_to_thread
                .get(session_id)
                .is_some_and(|owner| owner != thread_id)
            || self.child_to_thread.contains_key(session_id)
        {
            return false;
        }
        if let Some(old) = self
            .thread_to_session
            .insert(thread_id.to_string(), session_id.to_string())
            && old != session_id
        {
            self.session_to_thread.remove(&old);
            self.session_models.remove(&old);
            self.session_modes.remove(&old);
            self.session_efforts.remove(&old);
            self.session_effort_config_ids.remove(&old);
        }
        self.retired_root_to_thread.remove(session_id);
        self.session_to_thread
            .insert(session_id.to_string(), thread_id.to_string());
        true
    }
}

impl Harness for AcpHarness {
    fn pid(&self) -> Option<u32> {
        Some(self.transport.pid())
    }

    fn stdout_fd(&self) -> Option<RawFd> {
        Some(self.transport.stdout_fd())
    }

    fn stdin_fd(&self) -> Option<RawFd> {
        self.transport.stdin_fd()
    }

    fn wants_write(&self) -> bool {
        self.transport.wants_write()
    }

    fn on_writable(&mut self) -> bool {
        self.transport.on_writable()
    }

    fn on_readable(&mut self) -> Vec<HarnessEvent> {
        let mut events = Vec::new();
        let mut messages = Vec::new();
        let exited = self.transport.read_messages(|msg| messages.push(msg));
        for msg in messages {
            self.handle_message(msg, &mut events);
        }
        if exited {
            events.push(HarnessEvent::Exited);
        }
        events
    }

    fn fault(&self) -> Option<&'static str> {
        self.transport.fault()
    }

    fn kill_tree(self: Box<Self>) {
        self.transport.kill_tree();
    }

    fn stop(self: Box<Self>) {
        self.transport.stop();
    }

    fn is_initialized(&self) -> bool {
        self.initialized
    }

    fn capabilities(&self) -> HarnessCapabilities {
        HarnessCapabilities {
            supports_modes: self.mode_config_id.is_some(),
            supports_steering: self.initialized && self.supports_steering,
            supports_list_sessions: self.initialized && self.supports_list_sessions,
            supports_load_session: self.initialized && self.supports_load_session,
            supports_delete_session: self.initialized && self.supports_delete_session,
            holds_prompt_during_subagent_request: self.dialect.holds_prompt_during_subagent_request(),
        }
    }

    fn session_for_thread(&self, thread_id: &str) -> Option<&str> {
        self.thread_to_session.get(thread_id).map(String::as_str)
    }

    fn thread_for_session(&self, session_id: &str) -> Option<&str> {
        self.session_to_thread.get(session_id).map(String::as_str)
    }

    fn is_subagent_session(&self, session_id: &str) -> bool {
        self.child_to_thread.contains_key(session_id)
    }

    fn own_session_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.probe_sessions.iter().cloned().collect();
        ids.extend(self.session_to_thread.keys().cloned());
        ids.extend(self.retired_root_to_thread.keys().cloned());
        ids.extend(self.loading_sessions.iter().cloned());
        ids.extend(self.history_loads.keys().cloned());
        ids.extend(self.child_to_thread.keys().cloned());
        ids
    }

    fn launcher_of(&self, child_session_id: &str) -> Option<&str> {
        self.child_launchers.get(child_session_id).map(String::as_str)
    }

    fn retired_turn_of(&self, child_session_id: &str) -> Option<u64> {
        self.child_retired_turn.get(child_session_id).copied()
    }

    fn session_current_model(&self, session_id: &str) -> Option<&str> {
        self.session_models.get(session_id).map(String::as_str)
    }

    fn session_current_mode(&self, session_id: &str) -> Option<&str> {
        self.session_modes.get(session_id).map(String::as_str)
    }

    fn session_effort(&self, session_id: &str) -> Option<&EffortSelector> {
        self.session_efforts.get(session_id)
    }

    fn detach_session(&mut self, thread_id: &str, retired_turn: Option<u64>) {
        let entry = self.session_gen.entry(thread_id.to_string()).or_insert(0);
        *entry = entry.wrapping_add(1);
        self.transport_failures.remove(thread_id);
        let session_id = self.thread_to_session.remove(thread_id);
        if let Some(sid) = session_id {
            self.session_to_thread.remove(&sid);
            self.session_models.remove(&sid);
            self.session_modes.remove(&sid);
            self.session_efforts.remove(&sid);
            self.session_effort_config_ids.remove(&sid);
            self.retired_root_to_thread
                .insert(sid.clone(), (thread_id.to_string(), retired_turn));
            if self.supports_close_session {
                let close = CloseSessionRequest::new(SessionId::new(sid));
                let id = self.transport.send_request(AGENT_METHOD_NAMES.session_close, &close);
                self.pending.insert(id, PendingRpc::CloseProbe);
                let _ = self.transport.on_writable();
            }
        }
    }

    fn forget_session(&mut self, thread_id: &str) {
        let entry = self.session_gen.entry(thread_id.to_string()).or_insert(0);
        *entry = entry.wrapping_add(1);
        self.transport_failures.remove(thread_id);
        if let Some(session_id) = self.thread_to_session.remove(thread_id) {
            self.session_to_thread.remove(&session_id);
            self.session_models.remove(&session_id);
            self.session_modes.remove(&session_id);
            self.session_efforts.remove(&session_id);
            self.session_effort_config_ids.remove(&session_id);
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
            self.child_retired_turn.remove(&cid);
        }
        self.retired_root_to_thread.retain(|_, (owner, _)| owner != thread_id);
        self.pending_subagent_calls
            .retain(|_, call| call.thread_id != thread_id);
    }

    fn delete_session(&mut self, thread_id: &str, session_id: &str) -> bool {
        if !self.supports_delete_session {
            return false;
        }
        let id = self.transport.send_request(
            AGENT_METHOD_NAMES.session_delete,
            &DeleteSessionRequest::new(session_id.to_string()),
        );
        self.pending.insert(
            id,
            PendingRpc::DeleteSession {
                thread_id: thread_id.into(),
                session_id: session_id.into(),
            },
        );
        let _ = self.transport.on_writable();
        true
    }

    fn ensure_session(
        &mut self,
        thread_id: &str,
        turn: u64,
        cwd: &str,
        existing_session_id: Option<&str>,
        mcp_servers: &[McpServerSpec],
    ) {
        let cur_epoch = self.current_session_gen(thread_id);
        for pending in self.pending.values_mut() {
            match pending {
                PendingRpc::NewSession {
                    thread_id: waiting,
                    turn: waiting_turn,
                    epoch,
                    ..
                }
                | PendingRpc::LoadSession {
                    thread_id: waiting,
                    turn: waiting_turn,
                    epoch,
                    ..
                } if waiting == thread_id && *epoch == cur_epoch => {
                    *waiting_turn = turn;
                    return;
                }
                _ => {}
            }
        }
        let Some(sid) = existing_session_id else {
            self.request_new_session(thread_id, turn, cwd, false, mcp_servers, cur_epoch);
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
            self.request_new_session(thread_id, turn, cwd, true, mcp_servers, cur_epoch);
            return;
        }
        self.loading_sessions.insert(sid.to_string());
        let req = LoadSessionRequest::new(SessionId::new(sid.to_string()), PathBuf::from(cwd))
            .mcp_servers(to_acp_mcp_servers(mcp_servers, self.dialect.as_ref()));
        let id = self.transport.send_request(AGENT_METHOD_NAMES.session_load, &req);
        self.pending.insert(
            id,
            PendingRpc::LoadSession {
                thread_id: thread_id.to_string(),
                turn,
                session_id: sid.to_string(),
                cwd: cwd.to_string(),
                mcp_servers: mcp_servers.to_vec(),
                epoch: cur_epoch,
            },
        );
        let _ = self.transport.on_writable();
    }

    fn load_session_history(
        &mut self,
        thread_id: &str,
        session_id: &str,
        cwd: &str,
        mcp_servers: &[McpServerSpec],
    ) -> bool {
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
        self.history_loads.insert(session_id.to_string(), HistoryBuilder::new());
        let req = LoadSessionRequest::new(SessionId::new(session_id.to_string()), PathBuf::from(cwd))
            .mcp_servers(to_acp_mcp_servers(mcp_servers, self.dialect.as_ref()));
        let id = self.transport.send_request(AGENT_METHOD_NAMES.session_load, &req);
        self.pending.insert(
            id,
            PendingRpc::LoadHistory {
                thread_id: thread_id.to_string(),
                session_id: session_id.to_string(),
            },
        );
        let _ = self.transport.on_writable();
        true
    }

    fn send_prompt(&mut self, thread_id: &str, turn: u64, session_id: &str, context: Option<&str>, text: &str) {
        self.transport_failures.remove(thread_id);
        let req = PromptRequest::new(SessionId::new(session_id.to_string()), prompt_blocks(context, text));
        let id = self.transport.send_request(AGENT_METHOD_NAMES.session_prompt, &req);
        self.pending.insert(
            id,
            PendingRpc::Prompt {
                thread_id: thread_id.to_string(),
                turn,
            },
        );
        let _ = self.transport.on_writable();
    }

    fn steer(&mut self, thread_id: &str, steer: u64, session_id: &str, context: Option<&str>, text: &str) -> bool {
        if !self.initialized || !self.supports_steering {
            return false;
        }
        let Some(method) = self.dialect.steering_method() else {
            return false;
        };
        let params = json!({
            "sessionId": session_id,
            "prompt": prompt_blocks(context, text),
        });
        let id = self.transport.send_request(method, &params);
        self.pending.insert(
            id,
            PendingRpc::Steer {
                thread_id: thread_id.to_string(),
                steer,
            },
        );
        let _ = self.transport.on_writable();
        true
    }

    fn cancel_session(&mut self, session_id: &str) {
        if let Some(thread_id) = self.session_to_thread.get(session_id).cloned() {
            self.transport_failures.remove(&thread_id);
        }
        let cancel = CancelNotification::new(SessionId::new(session_id.to_string()));
        self.transport
            .send_notification(AGENT_METHOD_NAMES.session_cancel, &cancel);
        let _ = self.transport.on_writable();
    }

    fn answer_permission(&mut self, handle: u64, answer: Option<&str>) -> bool {
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
        self.transport
            .send_result_response(permission.rpc_id, RequestPermissionResponse::new(outcome));
        let _ = self.transport.on_writable();
        true
    }

    fn probing_models(&self) -> bool {
        self.probing_models
    }

    fn probe_models(&mut self, cwd: &str) {
        if self.probing_models {
            return;
        }
        self.probing_models = true;
        let req = NewSessionRequest::new(PathBuf::from(cwd));
        let id = self.transport.send_request(AGENT_METHOD_NAMES.session_new, &req);
        self.pending.insert(id, PendingRpc::ProbeModels);
        let _ = self.transport.on_writable();
    }

    fn probe_efforts(&mut self, cwd: &str, model: &str) {
        if !self.probing_efforts.insert(model.to_string()) {
            return;
        }
        let req = NewSessionRequest::new(PathBuf::from(cwd));
        let id = self.transport.send_request(AGENT_METHOD_NAMES.session_new, &req);
        self.pending.insert(
            id,
            PendingRpc::ProbeEffortSession {
                model: model.to_string(),
            },
        );
        let _ = self.transport.on_writable();
    }

    fn take_new_probe_sessions(&mut self) -> Vec<String> {
        std::mem::take(&mut self.new_probe_sessions)
    }

    fn set_session_model(&mut self, thread_id: &str, turn: Option<u64>, session_id: &str, model: &str) {
        let config_id = self.model_config_id.as_deref().unwrap_or("model");
        let req = SetSessionConfigOptionRequest::new(
            SessionId::new(session_id.to_string()),
            SessionConfigId::new(config_id.to_string()),
            model,
        );
        let id = self
            .transport
            .send_request(AGENT_METHOD_NAMES.session_set_config_option, &req);
        self.pending.insert(
            id,
            PendingRpc::SetModel {
                thread_id: thread_id.to_string(),
                turn,
                session_id: session_id.to_string(),
                model: model.to_string(),
            },
        );
        let _ = self.transport.on_writable();
    }

    fn set_session_mode(&mut self, thread_id: &str, turn: Option<u64>, session_id: &str, mode: &str) {
        let config_id = self.mode_config_id.clone().unwrap_or_else(|| "mode".to_string());
        let value = self.dialect.harness_mode(mode).unwrap_or(mode);
        let req = SetSessionConfigOptionRequest::new(
            SessionId::new(session_id.to_string()),
            SessionConfigId::new(config_id),
            value,
        );
        let id = self
            .transport
            .send_request(AGENT_METHOD_NAMES.session_set_config_option, &req);
        self.pending.insert(
            id,
            PendingRpc::SetMode {
                thread_id: thread_id.to_string(),
                turn,
                session_id: session_id.to_string(),
                mode: mode.to_string(),
            },
        );
        let _ = self.transport.on_writable();
    }

    fn set_session_effort(&mut self, thread_id: &str, turn: u64, session_id: &str, effort: &str) {
        let config_id = self
            .session_effort_config_ids
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| "effort".to_string());
        let req = SetSessionConfigOptionRequest::new(
            SessionId::new(session_id.to_string()),
            SessionConfigId::new(config_id),
            effort,
        );
        let id = self
            .transport
            .send_request(AGENT_METHOD_NAMES.session_set_config_option, &req);
        self.pending.insert(
            id,
            PendingRpc::SetEffort {
                thread_id: thread_id.to_string(),
                turn,
                session_id: session_id.to_string(),
                effort: effort.to_string(),
            },
        );
        let _ = self.transport.on_writable();
    }

    fn is_listing_sessions(&self) -> bool {
        self.listing_sessions
    }

    fn abandon_list_sessions(&mut self) {
        self.pending
            .retain(|_, pending| !matches!(pending, PendingRpc::ListSessions { .. }));
        self.listing_sessions = false;
        self.live_sessions_acc.clear();
        self.live_sessions_seen.clear();
    }

    fn list_sessions(&mut self) -> bool {
        if !self.capabilities().supports_list_sessions || self.listing_sessions {
            return false;
        }
        self.listing_sessions = true;
        self.live_sessions_acc.clear();
        self.live_sessions_seen.clear();
        let req = ListSessionsRequest::new();
        let id = self.transport.send_request(AGENT_METHOD_NAMES.session_list, &req);
        self.pending.insert(
            id,
            PendingRpc::ListSessions {
                page: 0,
                prev_cursor: None,
            },
        );
        let _ = self.transport.on_writable();
        true
    }

    fn remember_tool_ids(&mut self, thread_id: &str, ids: Vec<String>) {
        self.thread_tool_ids
            .entry(thread_id.to_string())
            .or_default()
            .extend(ids);
    }

    fn remember_subagent_ids(&mut self, thread_id: &str, ids: Vec<String>) {
        for id in ids {
            if !id.is_empty() {
                self.thread_subagent_ids
                    .entry(thread_id.to_string())
                    .or_default()
                    .insert(id);
            }
        }
    }
}
