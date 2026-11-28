//! ACP dialect seam (inspired by T3 Code's `AcpAdapterV2Flavor`).
//!
//! Isolates harness-specific ACP extensions (such as OpenCode's
//! `OPENCODE_CONFIG_CONTENT`, `crowbar-ask-first` mode mapping,
//! `_session/steering`, `opencode/session/child_update`, child metadata, and
//! transport retry loop detection) behind a narrow trait so generic ACP v1
//! harnesses stay completely clean of OpenCode-specific logic.

use crate::harness::{HarnessConfig, HarnessLaunchOptions};
use crate::mcp::{self, McpServerSpec};
use crate::opencode;
use agent_client_protocol_schema::v1::ToolKind;
use serde_json::{Map, Value, json};
use std::path::Path;

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

/// Per-harness ACP dialect hooks kept internal to the ACP adapter.
pub(crate) trait AcpDialect {
    /// Builds `(env_set, env_remove)` for spawning the long-lived ACP child.
    fn prepare_spawn_env(
        &self,
        config: &HarnessConfig,
        options: HarnessLaunchOptions,
    ) -> (Vec<(String, String)>, Vec<&'static str>);

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

    /// Extracts `(child_session_id, optional_title)` from vendor child-session `_meta`.
    fn extract_child_meta(&self, _val: &Value) -> Option<(String, Option<String>)> {
        None
    }

    /// Strips any dialect-specific `"<childTitle>: "` prefix from child act/request titles.
    fn strip_child_title_prefix(&self, title: &str, _child_title: Option<&str>) -> String {
        title.to_string()
    }

    /// Extracts a human-readable error message from a dialect child status payload.
    fn extract_child_error(&self, _error: Option<&Value>) -> Option<String> {
        None
    }

    /// Whether a tool call is this dialect's subagent wrapper launcher tool.
    fn is_subagent_tool(
        &self,
        _title: &str,
        _kind: ToolKind,
        _raw_input: Option<&Value>,
        _raw_update: &Value,
        _is_opencode_agent: bool,
    ) -> bool {
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

    /// Detects an endless provider transport retry loop from a raw update payload.
    fn extract_transport_retry_failure(&self, _raw_update: &Value) -> Option<String> {
        None
    }

    /// Recognizes an error indicating that no Model is configured/ready.
    fn is_no_models_error(&self, _err_val: &Value, message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("no models") || lower.contains("no model available") || lower.contains("model catalog")
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
pub(crate) struct OpenCodeAcpDialect;

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

    fn extract_child_error(&self, error: Option<&Value>) -> Option<String> {
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

    fn is_subagent_tool(
        &self,
        title: &str,
        kind: ToolKind,
        raw_input: Option<&Value>,
        raw_update: &Value,
        is_opencode_agent: bool,
    ) -> bool {
        let named = |pointer: &str| {
            matches!(
                raw_update.pointer(pointer).and_then(Value::as_str),
                Some("subagent" | "task")
            )
        };
        if named("/_meta/opencode/toolName") || (is_opencode_agent && named("/_meta/toolName")) {
            return true;
        }
        let clean = title.trim();
        if clean.eq_ignore_ascii_case("subagent") {
            return true;
        }
        is_opencode_agent
            && clean.eq_ignore_ascii_case("task")
            && (kind == ToolKind::Think
                || raw_input.is_some_and(|v| {
                    v.get("prompt").is_some() && (v.get("agent").is_some() || v.get("subagent_type").is_some())
                }))
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
        let lower = message.to_ascii_lowercase();
        lower.contains("no models") || lower.contains("no model available") || lower.contains("model catalog")
    }

    fn provider_display_name(&self, provider_id: &str) -> String {
        opencode::provider_name(provider_id)
    }

    fn enrich_mcp_server(&self, spec: &McpServerSpec) -> McpServerSpec {
        let mut enriched = spec.clone();
        if !enriched
            .env
            .iter()
            .any(|(k, _)| k == mcp::ENV_MCP_META_SESSION_KEY)
        {
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

/// Selects the ACP dialect for a configured harness.
///
/// OpenCode and Lever's ACP test stub (`crowbar-stub-harness`, which exercises
/// OpenCode's ACP extensions) receive [`OpenCodeAcpDialect`]; other ACP
/// harnesses receive the clean [`GenericAcpDialect`].
pub(crate) fn dialect_for_config(config: &HarnessConfig) -> Box<dyn AcpDialect> {
    let program_name = Path::new(&config.program)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if opencode::is_opencode_config(config) || program_name == "crowbar-stub-harness" {
        Box::new(OpenCodeAcpDialect)
    } else {
        Box::new(GenericAcpDialect)
    }
}
