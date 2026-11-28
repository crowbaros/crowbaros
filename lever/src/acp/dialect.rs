//! ACP dialect seam.
//!
//! Isolates harness-specific ACP extensions (such as OpenCode's
//! `OPENCODE_CONFIG_CONTENT`, `crowbar-ask-first` mode mapping,
//! `_session/steering`, `opencode/session/child_update`, child metadata, and
//! transport retry loop detection) behind a narrow trait so generic ACP v1
//! harnesses stay completely clean of OpenCode-specific logic.

use crate::harness::{HarnessConfig, HarnessDialect, HarnessLaunchOptions};
use crate::mcp::{self, McpServerSpec};
use crate::opencode;
use agent_client_protocol_schema::v1::ToolKind;
use serde_json::{Map, Value, json};

/// OpenCode's own request that steers a message into a session's running prompt.
const OPENCODE_STEERING: &str = "_session/steering";

/// OpenCode's custom notification carrying child/subagent session updates.
const OPENCODE_CHILD_UPDATE: &str = "opencode/session/child_update";

/// OpenCode's `_meta` key on MCP `tools/call` carrying the caller's native session ID.
const OPENCODE_MCP_META_SESSION_KEY: &str = "ai.opencode/sessionID";

/// What the person is told when the harness did not take a message into its
/// turn because the turn had ended as the message came.
const STEER_TOO_LATE: &str = "The Agent's turn ended before it took your message. Send it again.";

/// After this many consecutive `provider.transport` retry attempts (for
/// example `ECONNREFUSED` or offline DNS failures), Lever cancels the retry
/// loop and fails the turn with the transport error instead of letting the
/// harness retry indefinitely.
const MAX_TRANSPORT_RETRY_ATTEMPT: u64 = 3;

/// OpenCode's mode mapping: Lever mode ID -> OpenCode agent value, in display order.
const OPENCODE_MODES: [(&str, &str); 3] = [
    ("ask-first", opencode::ASK_FIRST_AGENT),
    ("full-access", "build"),
    ("plan", "plan"),
];

/// Generic ACP mode pass-through: Lever canonical mode IDs.
const GENERIC_MODES: [(&str, &str); 3] = [
    ("ask-first", "ask-first"),
    ("full-access", "full-access"),
    ("plan", "plan"),
];

/// Dialect-parsed body of a vendor child-session notification.
pub(crate) enum ChildUpdateBody<'a> {
    Status {
        state: Option<String>,
        error: Option<String>,
    },
    Update(&'a Value),
}

/// Dialect-parsed envelope of a vendor child-session notification.
pub(crate) struct ChildUpdateEnvelope<'a> {
    pub(crate) child_session_id: &'a str,
    pub(crate) root_session_id: Option<&'a str>,
    pub(crate) parent_session_id: Option<&'a str>,
    pub(crate) title: Option<&'a str>,
    pub(crate) body: ChildUpdateBody<'a>,
}

/// Per-harness ACP dialect hooks kept internal to the ACP adapter.
pub(crate) trait AcpDialect {
    /// Builds `(env_set, env_remove)` for spawning the long-lived ACP child.
    fn prepare_spawn_env(
        &self,
        config: &HarnessConfig,
        options: HarnessLaunchOptions,
    ) -> (Vec<(String, String)>, Vec<&'static str>);

    /// Builds `(env_set, env_remove)` for a one-shot ACP discovery child.
    fn prepare_discovery_env(&self, config: &HarnessConfig) -> (Vec<(String, String)>, Vec<&'static str>) {
        let env_set: Vec<(String, String)> = config.env.iter().filter(|(k, _)| k != "PWD").cloned().collect();
        (env_set, vec!["PWD"])
    }

    /// Records the agent identity reported in the ACP `initialize` response.
    fn on_initialize(&mut self, _agent_name: Option<&str>) {}

    /// Optional `_meta` map attached to `ClientCapabilities` in `initialize`.
    fn initialize_client_meta(&self) -> Option<Map<String, Value>> {
        None
    }

    /// Maps a harness mode value into Lever's canonical mode ID.
    fn lever_mode(&self, harness_value: &str) -> Option<&'static str>;

    /// Maps Lever's canonical mode ID into the harness's config option value.
    fn harness_mode(&self, lever_id: &str) -> Option<&'static str>;

    /// Ordered `(lever_id, harness_value)` pairs recognized by this dialect.
    fn mode_pairs(&self) -> &'static [(&'static str, &'static str)];

    /// Custom JSON-RPC method used for active-turn steering, if any.
    fn steering_method(&self) -> Option<&'static str> {
        None
    }

    /// Whether the harness's `initialize` response `_meta` advertises steering.
    fn steering_supported(&self, _init_meta: Option<&Map<String, Value>>) -> bool {
        false
    }

    /// Parses the JSON-RPC result of a steering request.
    fn parse_steer_response(&self, _result: &Value) -> Result<(), String> {
        Err("The harness does not support steering into a running turn.".to_string())
    }

    /// Custom JSON-RPC notification method carrying child-session updates, if any.
    fn child_update_notification_method(&self) -> Option<&'static str> {
        None
    }

    /// Parses a dialect-specific child-session update notification payload.
    fn parse_child_update<'a>(&self, _params: &'a Value) -> Option<ChildUpdateEnvelope<'a>> {
        None
    }

    /// Extracts `(child_session_id, optional_title)` from vendor child-session `_meta`.
    fn extract_child_meta(&self, _val: &Value) -> Option<(String, Option<String>)> {
        None
    }

    /// Strips any dialect-specific `"<childTitle>: "` prefix from child act/request titles.
    fn strip_child_title_prefix(&self, title: &str, _child_title: Option<&str>) -> String {
        title.to_string()
    }

    /// Whether a tool call is this dialect's subagent wrapper launcher tool.
    fn is_subagent_tool(&self, _title: &str, _kind: ToolKind, _raw_input: Option<&Value>, _raw_update: &Value) -> bool {
        false
    }

    /// Whether a subagent launcher's fallback title is a generic placeholder
    /// (for example OpenCode's `"subagent"` or `"task"`).
    fn is_launcher_placeholder_title(&self, _title: &str) -> bool {
        false
    }

    /// Extracts `(title, prompt, child_session_id)` from a subagent launcher tool call.
    fn extract_subagent_call_fields(
        &self,
        _raw_input: Option<&Value>,
        _raw_output: Option<&Value>,
    ) -> (Option<String>, Option<String>, Option<String>) {
        (None, None, None)
    }

    /// Extracts a child session ID from a launcher's plain-text output when
    /// structured output metadata was omitted.
    fn extract_session_id_from_text(&self, _text: &str) -> Option<String> {
        None
    }

    /// Detects an endless provider transport retry loop from a raw update payload.
    fn extract_transport_retry_failure(&self, _raw_update: &Value) -> Option<String> {
        None
    }

    /// Whether a JSON-RPC error from `session/new` means the harness has no
    /// Models available (for example OpenCode's `CatalogNotReady` when free
    /// hosted Models are switched off and no provider is signed in).
    fn is_no_models_error(&self, _err_val: &Value, message: &str) -> bool {
        message.to_ascii_lowercase().contains("no models are available")
    }

    /// Display name for a provider prefix in an ungrouped `<provider>/<model>` ID.
    fn provider_display_name(&self, provider_id: &str) -> String {
        provider_id.to_string()
    }

    /// Optionally enriches Lever's [`McpServerSpec`] with dialect-specific env vars.
    fn enrich_mcp_server(&self, spec: &McpServerSpec) -> McpServerSpec {
        spec.clone()
    }

    /// Whether sending a parent prompt while a subagent has a waiting request
    /// strands that request.
    fn holds_prompt_during_subagent_request(&self) -> bool {
        false
    }
}

/// Pure ACP v1 dialect with no vendor extensions.
pub(crate) struct GenericAcpDialect;

impl AcpDialect for GenericAcpDialect {
    fn prepare_spawn_env(
        &self,
        config: &HarnessConfig,
        _options: HarnessLaunchOptions,
    ) -> (Vec<(String, String)>, Vec<&'static str>) {
        (config.env.clone(), Vec::new())
    }

    fn lever_mode(&self, harness_value: &str) -> Option<&'static str> {
        GENERIC_MODES
            .iter()
            .find(|(_, value)| *value == harness_value)
            .map(|(id, _)| *id)
    }

    fn harness_mode(&self, lever_id: &str) -> Option<&'static str> {
        GENERIC_MODES
            .iter()
            .find(|(id, _)| *id == lever_id)
            .map(|(_, value)| *value)
    }

    fn mode_pairs(&self) -> &'static [(&'static str, &'static str)] {
        &GENERIC_MODES
    }
}

/// OpenCode ACP dialect: encapsulates all OpenCode-specific env vars,
/// agent-mode mapping, steering RPC, child-session notifications, subagent
/// launcher normalization, transport retry detection, and MCP session meta key.
#[derive(Default)]
pub(crate) struct OpenCodeAcpDialect {
    /// True when the harness's `initialize` response named itself `opencode`,
    /// gating OpenCode's legacy `task` subagent launcher heuristic.
    is_opencode_agent: bool,
}

impl OpenCodeAcpDialect {
    fn extract_child_error(error: Option<&Value>) -> Option<String> {
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
}

impl AcpDialect for OpenCodeAcpDialect {
    fn prepare_spawn_env(
        &self,
        config: &HarnessConfig,
        options: HarnessLaunchOptions,
    ) -> (Vec<(String, String)>, Vec<&'static str>) {
        let existing_config = config
            .env
            .iter()
            .rev()
            .find(|(k, _)| k == "OPENCODE_CONFIG_CONTENT")
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var("OPENCODE_CONFIG_CONTENT").ok());
        let person = opencode::person_settings(&config.env, existing_config.as_deref());
        opencode::build_harness_env(
            &config.env,
            existing_config.as_deref(),
            &person,
            options.free_hosted_models,
        )
    }

    fn prepare_discovery_env(&self, config: &HarnessConfig) -> (Vec<(String, String)>, Vec<&'static str>) {
        opencode::build_discovery_env(config)
    }

    fn on_initialize(&mut self, agent_name: Option<&str>) {
        self.is_opencode_agent = agent_name.is_some_and(|name| name.eq_ignore_ascii_case("opencode"));
    }

    fn initialize_client_meta(&self) -> Option<Map<String, Value>> {
        let mut client_meta = Map::new();
        client_meta.insert("opencode/child-session-updates".to_string(), json!(true));
        Some(client_meta)
    }

    fn lever_mode(&self, harness_value: &str) -> Option<&'static str> {
        OPENCODE_MODES
            .iter()
            .find(|(_, value)| *value == harness_value)
            .map(|(id, _)| *id)
    }

    fn harness_mode(&self, lever_id: &str) -> Option<&'static str> {
        OPENCODE_MODES
            .iter()
            .find(|(id, _)| *id == lever_id)
            .map(|(_, value)| *value)
    }

    fn mode_pairs(&self) -> &'static [(&'static str, &'static str)] {
        &OPENCODE_MODES
    }

    fn steering_method(&self) -> Option<&'static str> {
        Some(OPENCODE_STEERING)
    }

    /// Whether an `initialize` answer's `_meta` says the harness steers, as
    /// OpenCode 2.4.1 says it.
    fn steering_supported(&self, init_meta: Option<&Map<String, Value>>) -> bool {
        init_meta
            .and_then(|m| m.get("steering"))
            .and_then(|s| s.get("supported"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    fn parse_steer_response(&self, result: &Value) -> Result<(), String> {
        match result.get("outcome").and_then(Value::as_str) {
            Some("injected") => Ok(()),
            Some("promptRequired") => Err(STEER_TOO_LATE.to_string()),
            _ => Err("The Agent did not take your message into its turn. Send it again.".to_string()),
        }
    }

    fn child_update_notification_method(&self) -> Option<&'static str> {
        Some(OPENCODE_CHILD_UPDATE)
    }

    fn parse_child_update<'a>(&self, params: &'a Value) -> Option<ChildUpdateEnvelope<'a>> {
        let obj = params.as_object()?;
        let child_session_id = obj
            .get("childSessionId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())?;
        let root_session_id = obj.get("rootSessionId").and_then(Value::as_str);
        let parent_session_id = obj.get("parentSessionId").and_then(Value::as_str);
        let title = obj
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let body = match obj.get("type").and_then(Value::as_str) {
            Some("status") => {
                let state = match obj.get("status").and_then(Value::as_str) {
                    Some("created" | "running") => Some("working".to_string()),
                    Some("completed") => Some("done".to_string()),
                    Some("cancelled" | "canceled") => Some("stopped".to_string()),
                    Some("interrupted") => Some("interrupted".to_string()),
                    Some("failed") => Some("failed".to_string()),
                    _ => None,
                };
                let error = if state.as_deref() == Some("failed") {
                    Self::extract_child_error(obj.get("error"))
                } else {
                    None
                };
                ChildUpdateBody::Status { state, error }
            }
            Some("update") => ChildUpdateBody::Update(obj.get("update")?),
            _ => return None,
        };
        Some(ChildUpdateEnvelope {
            child_session_id,
            root_session_id,
            parent_session_id,
            title,
            body,
        })
    }

    fn extract_child_meta(&self, val: &Value) -> Option<(String, Option<String>)> {
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

    fn strip_child_title_prefix(&self, title: &str, child_title: Option<&str>) -> String {
        if let Some(prefix) = child_title.map(str::trim).filter(|s| !s.is_empty())
            && let Some(rest) = title.strip_prefix(prefix)
            && let Some(after_colon) = rest.strip_prefix(": ")
            && !after_colon.trim().is_empty()
        {
            return after_colon.to_string();
        }
        title.to_string()
    }

    fn is_subagent_tool(&self, title: &str, kind: ToolKind, raw_input: Option<&Value>, raw_update: &Value) -> bool {
        let named = |pointer: &str| {
            matches!(
                raw_update.pointer(pointer).and_then(Value::as_str),
                Some("subagent" | "task")
            )
        };
        if named("/_meta/opencode/toolName") || (self.is_opencode_agent && named("/_meta/toolName")) {
            return true;
        }
        let clean = title.trim();
        if clean.eq_ignore_ascii_case("subagent") {
            return true;
        }
        self.is_opencode_agent
            && clean.eq_ignore_ascii_case("task")
            && (kind == ToolKind::Think
                || raw_input.is_some_and(|v| {
                    v.get("prompt").is_some() && (v.get("agent").is_some() || v.get("subagent_type").is_some())
                }))
    }

    fn is_launcher_placeholder_title(&self, title: &str) -> bool {
        title.eq_ignore_ascii_case("subagent") || title.eq_ignore_ascii_case("task")
    }

    fn extract_subagent_call_fields(
        &self,
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

    fn extract_session_id_from_text(&self, text: &str) -> Option<String> {
        let idx = text.find("sessionID:")?;
        let rest = text[idx + "sessionID:".len()..].trim_start();
        let id: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            .collect();
        (!id.is_empty()).then_some(id)
    }

    fn extract_transport_retry_failure(&self, raw_update: &Value) -> Option<String> {
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

    fn is_no_models_error(&self, err_val: &Value, message: &str) -> bool {
        if err_val
            .get("data")
            .and_then(|d| d.get("errorName"))
            .and_then(Value::as_str)
            == Some("CatalogNotReady")
        {
            return true;
        }
        message.to_ascii_lowercase().contains("no models are available")
    }

    fn provider_display_name(&self, provider_id: &str) -> String {
        opencode::provider_name(provider_id)
    }

    fn enrich_mcp_server(&self, spec: &McpServerSpec) -> McpServerSpec {
        let mut enriched = spec.clone();
        if !enriched.env.iter().any(|(k, _)| k == mcp::ENV_MCP_META_SESSION_KEY) {
            enriched.env.push((
                mcp::ENV_MCP_META_SESSION_KEY.to_string(),
                OPENCODE_MCP_META_SESSION_KEY.to_string(),
            ));
        }
        enriched
    }

    fn holds_prompt_during_subagent_request(&self) -> bool {
        true
    }
}

/// Selects the ACP dialect from `config.dialect`.
pub(crate) fn dialect_for_config(config: &HarnessConfig) -> Box<dyn AcpDialect> {
    match config.dialect {
        HarnessDialect::OpenCodeAcp => Box::new(OpenCodeAcpDialect::default()),
        HarnessDialect::GenericAcp => Box::new(GenericAcpDialect),
    }
}
