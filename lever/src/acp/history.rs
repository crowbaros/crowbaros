//! ACP `session/load` history replay accumulator into [`SessionHistory`].

use super::dialect::AcpDialect;
use super::format::{content_block_text, map_tool_kind, map_tool_status, tool_detail};
use crate::harness::{ImportedAct, ImportedSubagent, ImportedTurn, SessionHistory};
use agent_client_protocol_schema::MaybeUndefined;
use agent_client_protocol_schema::v1::{IdleStopReason, SessionUpdate, StateUpdate, ToolCallStatus};
use serde_json::Value;
use std::collections::HashMap;

/// Accumulates replayed ACP `session/update` notifications during `session/load`
/// for an externally-started session into Lever's harness-agnostic
/// [`SessionHistory`].
pub(super) struct HistoryBuilder {
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
    pub(super) fn new() -> Self {
        Self {
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

    pub(super) fn push_update(&mut self, raw_update: &Value, dialect: &dyn AcpDialect) {
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
                if dialect.is_subagent_tool(&tc.title, tc.kind, tc.raw_input.as_ref(), raw_update) {
                    let (title, prompt, child_sid) =
                        dialect.extract_subagent_call_fields(tc.raw_input.as_ref(), tc.raw_output.as_ref());
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
                    let (new_title, new_prompt, child_sid) = dialect
                        .extract_subagent_call_fields(tcu.fields.raw_input.as_ref(), tcu.fields.raw_output.as_ref());
                    let child_sid = child_sid.or_else(|| {
                        tool_detail(
                            tcu.fields.content.as_deref().unwrap_or(&[]),
                            tcu.fields.raw_output.as_ref(),
                            tcu.fields.raw_input.as_ref(),
                        )
                        .and_then(|d| dialect.extract_session_id_from_text(&d))
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
            // A harness that tells of its subagents as ACP does, rather
            // than through OpenCode's launcher calls, replays them so too.
            Ok(SessionUpdate::SubagentUpdate(sub)) => {
                let child_sid = sub.session_id.0.as_ref().to_string();
                if child_sid.is_empty() {
                    return;
                }
                let title = match (sub.title, sub.description) {
                    (MaybeUndefined::Value(t), _) if !t.trim().is_empty() => Some(t.trim().to_string()),
                    (_, MaybeUndefined::Value(d)) if !d.trim().is_empty() => Some(d.trim().to_string()),
                    _ => None,
                };
                // The session ended before it was read: a subagent it left
                // working is done, as its acts are.
                let ended = match sub.state {
                    MaybeUndefined::Value(StateUpdate::Idle(idle)) => Some(match idle.stop_reason {
                        Some(IdleStopReason::Cancelled) => ("stopped", None),
                        Some(IdleStopReason::Refusal) => ("failed", Some("The subagent refused to continue.")),
                        Some(IdleStopReason::MaxTokens) => ("failed", Some("The subagent reached its token limit.")),
                        Some(IdleStopReason::MaxTurnRequests) => {
                            ("failed", Some("The subagent reached its request limit."))
                        }
                        Some(IdleStopReason::Error(_)) => ("failed", Some("The subagent failed.")),
                        _ => ("done", None),
                    }),
                    _ => None,
                };
                if let Some(existing) = self
                    .turns
                    .iter_mut()
                    .flat_map(|turn| turn.subagents.iter_mut())
                    .find(|existing| existing.harness_session_id == child_sid)
                {
                    if let Some(title) = title {
                        existing.title = title;
                    }
                    if let Some((state, error)) = ended {
                        existing.state = state.to_string();
                        existing.error = error.map(str::to_string);
                    }
                    return;
                }
                self.subagent_ids.push(child_sid.clone());
                let (state, error) = ended.unwrap_or(("done", None));
                let (_, turn) = self.current_turn_mut();
                turn.subagents.push(ImportedSubagent {
                    harness_session_id: child_sid,
                    title: title.unwrap_or_else(|| "Subagent".to_string()),
                    state: state.to_string(),
                    error: error.map(str::to_string),
                    prompt: None,
                });
            }
            // What the session asked of one of its subagents is that
            // subagent's prompt.
            Ok(SessionUpdate::SessionMessage(msg)) => {
                let MaybeUndefined::Value(recipient) = msg.recipient_session_id else {
                    return;
                };
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
                if let Some(sub) = self
                    .turns
                    .iter_mut()
                    .flat_map(|turn| turn.subagents.iter_mut())
                    .find(|sub| sub.harness_session_id == recipient.0.as_ref())
                    && sub.prompt.is_none()
                {
                    sub.prompt = Some(text);
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

    pub(super) fn finish(
        mut self,
        model: Option<String>,
        effort: Option<String>,
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
            },
            self.tool_ids,
            self.subagent_ids,
        )
    }
}
