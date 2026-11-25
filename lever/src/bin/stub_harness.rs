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
//! Usage:
//!   crowbar-stub-harness --script <path> [--log <path>]

use agent_client_protocol_schema::ProtocolVersion;
use agent_client_protocol_schema::v1::{
    AGENT_METHOD_NAMES, AgentCapabilities, AvailableCommandsUpdate, CLIENT_METHOD_NAMES, CancelNotification,
    CloseSessionRequest, CloseSessionResponse, ContentBlock, ContentChunk, Error as AcpError, Implementation,
    InitializeRequest, InitializeResponse, JsonRpcMessage, LoadSessionRequest, LoadSessionResponse, NewSessionRequest,
    NewSessionResponse, Notification, PromptRequest, PromptResponse, Request, RequestId, Response, SessionCapabilities,
    SessionCloseCapabilities, SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelectOption, SessionId,
    SessionInfoUpdate, SessionNotification, SessionUpdate, SetSessionConfigOptionRequest,
    SetSessionConfigOptionResponse, StopReason, TextContent, ToolCall, ToolCallContent, ToolCallId, ToolCallStatus,
    ToolCallUpdate, ToolCallUpdateFields, ToolKind,
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
}

#[derive(Debug, Deserialize)]
struct Script {
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
    /// Updates sent on `session/load` before it is answered, as a harness
    /// replays a session's history.
    #[serde(default)]
    load_replay: Vec<Step>,
    /// Waits this long before it answers `session/load`, and goes on taking
    /// requests meanwhile.
    #[serde(default)]
    load_delay_ms: u64,
    /// If true, every result also carries `"error": null`, and every update
    /// `"id": null`.
    #[serde(default)]
    explicit_nulls: bool,
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
        }
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

fn build_config_options(models: Option<&[StubModel]>, current: Option<&str>) -> Option<Vec<SessionConfigOption>> {
    let models = models?;
    let current = current
        .or_else(|| models.first().map(|m| m.id.as_str()))
        .unwrap_or("")
        .to_string();
    let opts: Vec<SessionConfigSelectOption> = models
        .iter()
        .map(|m| SessionConfigSelectOption::new(m.id.clone(), m.name.clone()))
        .collect();
    let option =
        SessionConfigOption::select("model", "Model", current, opts).category(SessionConfigOptionCategory::Model);
    Some(vec![option])
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
                    let session_caps = SessionCapabilities::new().close(SessionCloseCapabilities::new());
                    let caps = AgentCapabilities::new()
                        .load_session(script.load_session)
                        .session_capabilities(session_caps);
                    let resp = InitializeResponse::new(ProtocolVersion::V1)
                        .agent_capabilities(caps)
                        .agent_info(Implementation::new("crowbar-stub-harness", "0.1.0"));
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
                    if let Some(ref dm) = default_model {
                        session_models.lock().unwrap().insert(sid.clone(), dm.clone());
                    }
                    let mut resp = NewSessionResponse::new(SessionId::new(sid.clone()));
                    if let Some(opts) = build_config_options(script.models.as_deref(), default_model.as_deref()) {
                        resp = resp.config_options(opts);
                    }
                    writer.send_result(id, resp);
                    if script.emit_available_commands {
                        writer.send_update(
                            &SessionId::new(sid),
                            SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(Vec::new())),
                        );
                    }
                }
                Err(e) => writer.send_error(id, AcpError::invalid_params().data(json!(e.to_string()))),
            }
        } else if req.method.as_ref() == AGENT_METHOD_NAMES.session_close {
            match serde_json::from_value::<CloseSessionRequest>(params) {
                Ok(close_req) => {
                    session_models.lock().unwrap().remove(close_req.session_id.0.as_ref());
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
                    let valid = script
                        .models
                        .as_ref()
                        .is_some_and(|list| list.iter().any(|m| m.id == val));
                    if set_req.config_id.0.as_ref() != "model" || !valid {
                        writer.send_error(id, AcpError::invalid_params().data(json!("unknown model")));
                    } else {
                        session_models.lock().unwrap().insert(sid, val.clone());
                        let opts = build_config_options(script.models.as_deref(), Some(&val)).unwrap_or_default();
                        writer.send_result(id, SetSessionConfigOptionResponse::new(opts));
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
                        let opts = build_config_options(script.models.as_deref(), cur.as_deref());
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
                        }));
                    }
                }
                Err(e) => writer.send_error(id, AcpError::invalid_params().data(json!(e.to_string()))),
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
            active_cancels.lock().unwrap().insert(sid_key.clone(), cancel.clone());

            let writer = writer.clone();
            let active_cancels = active_cancels.clone();
            let session_id = prompt_req.session_id;
            workers.push(thread::spawn(move || {
                let mut was_cancelled = false;
                for step in &turn_script.steps {
                    if cancel.is_cancelled() || emit_step(&writer, &session_id, step, Some(&cancel), model.as_deref()) {
                        was_cancelled = true;
                        break;
                    }
                }
                if turn_script.crash {
                    std::process::exit(1);
                }
                if !was_cancelled && turn_script.stall {
                    cancel.wait_cancelled();
                    was_cancelled = true;
                }
                active_cancels.lock().unwrap().remove(&sid_key);

                if was_cancelled && turn_script.cancel_with_error {
                    writer.send_error(id, AcpError::request_cancelled());
                } else if was_cancelled && turn_script.error.is_none() {
                    writer.send_result(id, PromptResponse::new(StopReason::Cancelled));
                } else if let Some(err) = &turn_script.error {
                    writer.send_error(id, AcpError::new(err.code, err.message.clone()));
                } else {
                    writer.send_result(id, PromptResponse::new(parse_stop_reason(&turn_script.stop_reason)));
                }

                for step in &turn_script.after {
                    let _ = emit_step(&writer, &session_id, step, None, model.as_deref());
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
