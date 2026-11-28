//! ACP payload formatting, config option parsing, diff rendering, and ID redaction.

use super::dialect::AcpDialect;
use crate::harness::{EffortOption, EffortSelector, HarnessCommand, ModelCatalog, ModelOption};
use crate::mcp::{self, McpServerSpec};
use agent_client_protocol_schema::v1::{
    Annotations, AvailableCommand, AvailableCommandInput, ContentBlock, EnvVariable, McpServer, McpServerStdio, Role,
    SessionConfigKind, SessionConfigOption, SessionConfigOptionCategory, SessionConfigSelect,
    SessionConfigSelectOptions, TextContent, ToolCallContent, ToolCallStatus, ToolCallUpdateFields, ToolKind,
};
use serde_json::Value;
use std::path::Path;

/// The longest a request's detail grows: a change to a long file is shown in
/// part, and says how much was left out.
const MAX_DETAIL_LINES: usize = 400;
/// Lines of a file shown around a change, as a diff has them.
const DIFF_CONTEXT: usize = 3;

/// A session's mode selector, as Lever reads it from its configuration options.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ModeSelector {
    pub(super) config_id: String,
    /// Lever's modes the harness offers, in Lever's order.
    pub(super) modes: Vec<String>,
    /// The session's mode, when it is one of Lever's.
    pub(super) current: Option<String>,
    /// The harness's value for the session's mode, whatever it is.
    pub(super) current_value: String,
}

/// A prompt's content: desktop context, for the Model's eyes alone so a slash
/// command at the start of the person's text is still one, then the text.
pub(super) fn prompt_blocks(context: Option<&str>, text: &str) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    if let Some(ctx) = context {
        blocks.push(ContentBlock::Text(
            TextContent::new(format!("{ctx}\n")).annotations(Annotations::new().audience(vec![Role::Assistant])),
        ));
    }
    blocks.push(ContentBlock::from(text.to_string()));
    blocks
}

/// Reads a session's mode selector: the configuration option of category
/// `mode`, or with the ID `mode`.
pub(super) fn extract_modes(options: &[SessionConfigOption], dialect: &dyn AcpDialect) -> Option<ModeSelector> {
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
    let modes: Vec<String> = dialect
        .mode_pairs()
        .iter()
        .filter(|(_, value)| values.iter().any(|v| v == value))
        .map(|(id, _)| (*id).to_string())
        .collect();
    if modes.is_empty() {
        return None;
    }
    let current_value = select.current_value.0.as_ref().to_string();
    Some(ModeSelector {
        config_id: opt.id.0.as_ref().to_string(),
        modes,
        current: dialect.lever_mode(&current_value).map(str::to_string),
        current_value,
    })
}

/// What a request shows the person: the exact command, or the file and the
/// change, and the file's path for an edit.
pub(super) fn request_detail(
    kind: Option<ToolKind>,
    fields: &ToolCallUpdateFields,
    home: &Path,
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
        _ => {
            let mcp_detail = fields
                .title
                .as_deref()
                .filter(|t| *t == mcp::TOOL_MAKE_PROJECT || t.ends_with(&format!("_{}", mcp::TOOL_MAKE_PROJECT)))
                .and_then(|_| input.and_then(|v| mcp::format_make_project_detail(v, home)));
            (mcp_detail.or_else(|| tool_detail(content, None, input)), None)
        }
    }
}

/// Translates Lever's harness-agnostic [`McpServerSpec`] list into ACP v1
/// stdio MCP server entries, applying any dialect-specific enrichments.
pub(super) fn to_acp_mcp_servers(specs: &[McpServerSpec], dialect: &dyn AcpDialect) -> Vec<McpServer> {
    specs
        .iter()
        .map(|spec| {
            let spec = dialect.enrich_mcp_server(spec);
            let env: Vec<EnvVariable> = spec
                .env
                .into_iter()
                .map(|(name, value)| EnvVariable::new(name, value))
                .collect();
            McpServer::Stdio(
                McpServerStdio::new(spec.name, spec.command)
                    .args(spec.args)
                    .env(env),
            )
        })
        .collect()
}

/// The change from `old` to `new` as lines that begin `-` and `+`, with a few
/// unchanged lines around it that begin with a space.
pub(super) fn line_diff(old: Option<&str>, new: &str) -> String {
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

/// The Model selector among a session's configuration options: the option of
/// category `model`, or with the ID `model`.
pub(super) fn model_option(options: &[SessionConfigOption]) -> Option<(&SessionConfigOption, &SessionConfigSelect)> {
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
/// has before the first `/`, and its name loses that provider where present.
fn ungrouped_model(id: &str, name: &str, dialect: &dyn AcpDialect) -> ModelOption {
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
        provider: provider.map(|p| (p.to_string(), dialect.provider_display_name(p))),
    }
}

/// Extracts the Model selector from ACP session configuration options.
pub(super) fn extract_model_catalog(
    options: &[SessionConfigOption],
    dialect: &dyn AcpDialect,
) -> Option<ModelCatalog> {
    let (opt, select) = model_option(options)?;
    let mut models = Vec::new();
    match &select.options {
        SessionConfigSelectOptions::Ungrouped(items) => {
            for item in items {
                models.push(ungrouped_model(item.value.0.as_ref(), &item.name, dialect));
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
pub(super) fn extract_effort(options: &[SessionConfigOption]) -> Option<EffortSelector> {
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
pub(super) fn harness_commands(commands: Vec<AvailableCommand>) -> Vec<HarnessCommand> {
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

pub(super) fn map_tool_kind(kind: ToolKind) -> &'static str {
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

pub(super) fn map_tool_status(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::InProgress => "working",
        ToolCallStatus::Completed => "done",
        ToolCallStatus::Failed => "failed",
        _ => "pending",
    }
}

pub(super) fn content_block_text(block: &ContentBlock) -> Option<String> {
    match block {
        ContentBlock::Text(text) => Some(text.text.clone()),
        ContentBlock::ResourceLink(link) => Some(format!("[resource: {}]", link.name)),
        _ => None,
    }
}

pub(super) fn tool_detail(
    content: &[ToolCallContent],
    raw_output: Option<&Value>,
    raw_input: Option<&Value>,
) -> Option<String> {
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

pub(super) fn value_to_detail(val: &Value) -> Option<String> {
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

/// Puts `[redacted]` where `input` has one of `ids` as a whole word.
pub(super) fn redact_ids(input: &str, mut ids: Vec<&str>) -> String {
    ids.retain(|id| !id.is_empty());
    ids.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    ids.dedup();
    let mut out = input.to_string();
    for id in ids {
        let mut redacted = String::with_capacity(out.len());
        let mut from = 0;
        while let Some(found) = out[from..].find(id) {
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
