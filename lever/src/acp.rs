//! Lever's Agent Client Protocol (ACP) v1 client over a harness child's
//! standard input and output, using a non-blocking JSON-RPC 2.0 line loop and
//! the pinned `agent-client-protocol-schema` v1 types.

use crate::channel::LEVER_VERSION;
use crate::sys;
use agent_client_protocol_schema::v1::{
    AGENT_METHOD_NAMES, CLIENT_METHOD_NAMES, CancelNotification, ContentBlock, Error as AcpError, Implementation,
    InitializeRequest, InitializeResponse, JsonRpcMessage, LoadSessionRequest, LoadSessionResponse, NewSessionRequest,
    NewSessionResponse, Notification, PromptRequest, PromptResponse, Request, RequestId, Response, SessionId,
    SessionUpdate, StopReason, ToolCallContent, ToolCallStatus, ToolKind,
};
use agent_client_protocol_schema::{MaybeUndefined, ProtocolVersion};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout};
use std::time::{Duration, Instant};

/// Configuration for a harness Lever can start on demand.
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    pub id: String,
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
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
    SessionReady {
        thread_id: String,
        turn: u64,
        session_id: String,
    },
    SessionFailed {
        thread_id: String,
        turn: u64,
        message: String,
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
    NewSession {
        thread_id: String,
        turn: u64,
    },
    LoadSession {
        thread_id: String,
        turn: u64,
        session_id: String,
        cwd: String,
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
    next_id: i64,
    initialized: bool,
    supports_load_session: bool,
    pending: HashMap<i64, PendingRpc>,
    thread_to_session: HashMap<String, String>,
    session_to_thread: HashMap<String, String>,
    loading_sessions: HashSet<String>,
    /// Tool call IDs seen per thread, used only to redact harness error messages
    /// and unrecognized updates without altering normal model output.
    thread_tool_ids: HashMap<String, HashSet<String>>,
}

impl AcpHarness {
    /// Spawns the configured harness child, configures non-blocking pipes, and
    /// queues the ACP v1 `initialize` request.
    pub fn spawn(config: &HarnessConfig) -> Result<Self, String> {
        let mut child = sys::spawn_harness(&config.program, &config.args)
            .map_err(|e| format!("could not start harness {}: {e}", config.name))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "harness child had no stdin pipe".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "harness child had no stdout pipe".to_string())?;
        sys::set_nonblocking(stdin.as_raw_fd()).map_err(|e| format!("could not set harness stdin nonblocking: {e}"))?;
        sys::set_nonblocking(stdout.as_raw_fd())
            .map_err(|e| format!("could not set harness stdout nonblocking: {e}"))?;

        let mut harness = Self {
            child,
            stdin: Some(stdin),
            stdout,
            write_buf: Vec::new(),
            read_buf: Vec::new(),
            next_id: 1,
            initialized: false,
            supports_load_session: false,
            pending: HashMap::new(),
            thread_to_session: HashMap::new(),
            session_to_thread: HashMap::new(),
            loading_sessions: HashSet::new(),
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

    pub fn session_for_thread(&self, thread_id: &str) -> Option<&str> {
        self.thread_to_session.get(thread_id).map(String::as_str)
    }

    /// Starts or reloads an ACP session for `thread_id`.
    pub fn ensure_session(&mut self, thread_id: &str, turn: u64, cwd: &str, existing_session_id: Option<&str>) {
        if self.supports_load_session
            && let Some(sid) = existing_session_id
        {
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
            return;
        }
        self.request_new_session(thread_id, turn, cwd);
    }

    fn request_new_session(&mut self, thread_id: &str, turn: u64, cwd: &str) {
        let req = NewSessionRequest::new(PathBuf::from(cwd));
        let id = self.send_request(AGENT_METHOD_NAMES.session_new, &req);
        self.pending.insert(
            id,
            PendingRpc::NewSession {
                thread_id: thread_id.to_string(),
                turn,
            },
        );
        let _ = self.on_writable();
    }

    /// Sends a `session/prompt` request for an active session.
    pub fn send_prompt(&mut self, thread_id: &str, turn: u64, session_id: &str, text: &str) {
        let req = PromptRequest::new(
            SessionId::new(session_id.to_string()),
            vec![ContentBlock::from(text.to_string())],
        );
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

    /// Reads available JSON-RPC lines from the child's stdout and returns any
    /// resulting events.
    pub fn on_readable(&mut self) -> Vec<AcpEvent> {
        let mut events = Vec::new();
        let mut buf = [0u8; 16384];
        loop {
            match self.stdout.read(&mut buf) {
                Ok(0) => {
                    self.drain_lines(&mut events);
                    events.push(AcpEvent::Exited);
                    return events;
                }
                Ok(n) => {
                    self.read_buf.extend_from_slice(&buf[..n]);
                    self.drain_lines(&mut events);
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
        while let Some(pos) = self.read_buf.iter().position(|&b| b == b'\n') {
            let line = self.read_buf[..pos].to_vec();
            self.read_buf.drain(..=pos);
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
    }

    fn handle_message(&mut self, value: Value, events: &mut Vec<AcpEvent>) {
        let has_method = value.get("method").and_then(Value::as_str).is_some();
        let has_id = value.get("id").is_some();

        if has_method && has_id {
            // Incoming JSON-RPC request from the harness: reject unsupported methods cleanly.
            if let Ok(id) = serde_json::from_value::<RequestId>(value["id"].clone()) {
                self.send_error_response(id, AcpError::method_not_found());
                let _ = self.on_writable();
            }
            return;
        }

        if has_method {
            self.handle_notification(&value, events);
            return;
        }

        if has_id {
            self.handle_response(value, events);
        }
    }

    fn handle_response(&mut self, value: Value, events: &mut Vec<AcpEvent>) {
        let Some(id) = value.get("id").and_then(Value::as_i64) else {
            return;
        };
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };

        if let Some(err_val) = value.get("error") {
            let raw_msg = serde_json::from_value::<AcpError>(err_val.clone())
                .map(|e| e.message)
                .unwrap_or_else(|_| "The harness reported an error.".to_string());
            match pending {
                PendingRpc::Initialize => events.push(AcpEvent::InitializeFailed {
                    message: self.redact_for_thread(None, &raw_msg),
                }),
                PendingRpc::NewSession { thread_id, turn } => {
                    let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                    events.push(AcpEvent::SessionFailed {
                        thread_id,
                        turn,
                        message,
                    });
                }
                PendingRpc::LoadSession {
                    thread_id,
                    turn,
                    session_id,
                    cwd,
                } => {
                    self.loading_sessions.remove(&session_id);
                    self.request_new_session(&thread_id, turn, &cwd);
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
                    events.push(AcpEvent::Initialized);
                }
                Ok(_) => events.push(AcpEvent::InitializeFailed {
                    message: "The harness uses an unsupported protocol version.".to_string(),
                }),
                Err(_) => events.push(AcpEvent::InitializeFailed {
                    message: "The harness returned an invalid initialization response.".to_string(),
                }),
            },
            PendingRpc::NewSession { thread_id, turn } => match serde_json::from_value::<NewSessionResponse>(result) {
                Ok(resp) => {
                    let sid = resp.session_id.0.as_ref().to_string();
                    self.thread_to_session.insert(thread_id.clone(), sid.clone());
                    self.session_to_thread.insert(sid.clone(), thread_id.clone());
                    events.push(AcpEvent::SessionReady {
                        thread_id,
                        turn,
                        session_id: sid,
                    });
                }
                Err(_) => events.push(AcpEvent::SessionFailed {
                    thread_id,
                    turn,
                    message: "The harness could not start a thread.".to_string(),
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
                    Ok(_) => {
                        self.thread_to_session.insert(thread_id.clone(), session_id.clone());
                        self.session_to_thread.insert(session_id.clone(), thread_id.clone());
                        events.push(AcpEvent::SessionReady {
                            thread_id,
                            turn,
                            session_id,
                        });
                    }
                    Err(_) => {
                        self.request_new_session(&thread_id, turn, &cwd);
                    }
                }
            }
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
            // Any other update (known to the schema or unknown/future) is kept
            // verbatim in the record and rendered plainly.
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

    /// Redacts harness-internal IDs from error strings and unrecognized updates
    /// without touching normal model answers.
    fn redact_for_thread(&self, thread_id: Option<&str>, input: &str) -> String {
        let mut out = input.to_string();
        if let Some(tid) = thread_id {
            if let Some(sid) = self.thread_to_session.get(tid)
                && !sid.is_empty()
                && out.contains(sid)
            {
                out = out.replace(sid, "[redacted]");
            }
            if let Some(tools) = self.thread_tool_ids.get(tid) {
                for call_id in tools {
                    if !call_id.is_empty() && out.contains(call_id) {
                        out = out.replace(call_id, "[redacted]");
                    }
                }
            }
        } else {
            for sid in self.session_to_thread.keys() {
                if !sid.is_empty() && out.contains(sid) {
                    out = out.replace(sid, "[redacted]");
                }
            }
        }
        out
    }

    /// Turns an unknown ACP update payload into plain text without exposing
    /// ACP's discriminator tag (`sessionUpdate`), schema field names, metadata,
    /// or harness IDs.
    fn render_unknown_update(&self, thread_id: &str, raw: &Value) -> String {
        let Some(obj) = raw.as_object() else {
            return self.redact_for_thread(Some(thread_id), &raw.to_string());
        };
        for preferred in ["summary", "message", "text", "note", "detail", "title"] {
            if let Some(s) = obj.get(preferred).and_then(Value::as_str)
                && !s.trim().is_empty()
            {
                return self.redact_for_thread(Some(thread_id), s.trim());
            }
        }
        if let Some(content_text) = obj.get("content").and_then(|c| c.get("text")).and_then(Value::as_str)
            && !content_text.trim().is_empty()
        {
            return self.redact_for_thread(Some(thread_id), content_text.trim());
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
            self.redact_for_thread(Some(thread_id), &parts.join(" "))
        }
    }
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
    if let Some(val) = raw_output {
        return value_to_detail(val);
    }
    if let Some(val) = raw_input {
        return value_to_detail(val);
    }
    None
}

fn value_to_detail(val: &Value) -> Option<String> {
    match val {
        Value::Null => None,
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::String(_) => None,
        Value::Object(map) => {
            if let Some(cmd) = map.get("command").and_then(Value::as_str) {
                return Some(cmd.to_string());
            }
            if let Some(path) = map.get("path").and_then(Value::as_str) {
                return Some(path.to_string());
            }
            let s = json!(map).to_string();
            (!s.is_empty()).then_some(s)
        }
        other => Some(other.to_string()),
    }
}
