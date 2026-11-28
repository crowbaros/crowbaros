//! Incoming JSON-RPC message dispatch, permission requests, session updates,
//! subagent routing, and ID redaction for [`AcpHarness`].

use super::format::{
    content_block_text, extract_model_catalog, harness_commands, map_tool_kind, map_tool_status, redact_ids,
    request_detail, tool_detail,
};
use super::{AcpHarness, PendingRpc, PendingSubagentCall, Permission, PromptFrom};
use crate::harness::HarnessEvent;
use agent_client_protocol_schema::MaybeUndefined;
use agent_client_protocol_schema::v1::{
    AGENT_METHOD_NAMES, CLIENT_METHOD_NAMES, CancelNotification, CancelRequestNotification, Error as AcpError,
    IdleStopReason, PROTOCOL_LEVEL_METHOD_NAMES, PermissionOptionKind, RequestId, RequestPermissionOutcome,
    RequestPermissionRequest, RequestPermissionResponse, SessionId, SessionUpdate, StateUpdate, ToolCall,
    ToolCallStatus, ToolCallUpdate,
};
use serde_json::Value;

impl AcpHarness {
    pub(super) fn handle_message(&mut self, value: Value, events: &mut Vec<HarnessEvent>) {
        let has_method = value.get("method").and_then(Value::as_str).is_some();
        let has_id = value.get("id").is_some_and(|id| !id.is_null());

        if has_method && has_id {
            if let Ok(id) = serde_json::from_value::<RequestId>(value["id"].clone()) {
                let method = value.get("method").and_then(Value::as_str).unwrap_or("");
                if method == CLIENT_METHOD_NAMES.session_request_permission {
                    self.handle_permission_request(id, value.get("params"), events);
                } else {
                    self.transport.send_error_response(id, AcpError::method_not_found());
                }
                let _ = self.transport.on_writable();
            }
            return;
        }

        if has_method {
            let method = value.get("method").and_then(Value::as_str);
            if method == Some(PROTOCOL_LEVEL_METHOD_NAMES.cancel_request) {
                self.handle_cancel_request(value.get("params"), events);
            } else if method.is_some() && method == self.dialect.child_update_notification_method() {
                self.handle_child_update_notification(value.get("params"), events);
            } else {
                self.handle_notification(&value, events);
            }
            return;
        }

        if has_id {
            self.handle_response(value, events);
        }
    }

    fn handle_permission_request(&mut self, rpc_id: RequestId, params: Option<&Value>, events: &mut Vec<HarnessEvent>) {
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
                .and_then(|tc| self.dialect.extract_child_meta(tc))
            {
                let effective_title = child_title.or_else(|| self.child_titles.get(&child_sid).cloned());
                Some((owner, Some(child_sid), effective_title))
            } else {
                Some((owner, None, None))
            }
        });
        let (Some(request), Some((thread_id, harness_subagent_id, subagent_title))) = (request, resolved) else {
            self.transport.send_result_response(
                rpc_id,
                RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
            );
            let _ = self.transport.on_writable();
            return;
        };
        if let Some(ref child_sid) = harness_subagent_id
            && !self.register_child(&thread_id, child_sid, subagent_title.as_deref())
        {
            self.transport.send_result_response(
                rpc_id,
                RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
            );
            let _ = self.transport.on_writable();
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
            self.transport.send_result_response(
                rpc_id,
                RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
            );
            let _ = self.transport.on_writable();
            return;
        }
        let call = &request.tool_call;
        let call_id = call.tool_call_id.0.as_ref().to_string();
        self.thread_tool_ids
            .entry(thread_id.clone())
            .or_default()
            .insert(call_id.clone());
        let kind = call.fields.kind.map(|k| map_tool_kind(k).to_string());
        let (detail, path) = request_detail(call.fields.kind, &call.fields, &self.home);
        let raw_title = call
            .fields
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| path.clone())
            .or_else(|| detail.as_ref().and_then(|d| d.lines().next().map(str::to_string)))
            .unwrap_or_else(|| "Act".to_string());
        let title = if harness_subagent_id.is_some() {
            self.dialect
                .strip_child_title_prefix(&raw_title, subagent_title.as_deref())
        } else {
            raw_title
        };
        let handle = self.next_permission;
        self.next_permission += 1;
        self.permissions.insert(handle, Permission { rpc_id, options });
        events.push(HarnessEvent::PermissionRequested {
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

    fn handle_cancel_request(&mut self, params: Option<&Value>, events: &mut Vec<HarnessEvent>) {
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
            self.transport.send_result_response(
                permission.rpc_id,
                RequestPermissionResponse::new(RequestPermissionOutcome::Cancelled),
            );
            let _ = self.transport.on_writable();
            events.push(HarnessEvent::PermissionWithdrawn { handle });
        }
    }

    #[must_use]
    pub(super) fn register_child(&mut self, thread_id: &str, child_sid: &str, title: Option<&str>) -> bool {
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

    fn take_launcher_call(
        &mut self,
        thread_id: &str,
        session_id: &str,
        tc: &ToolCall,
        title: &str,
        raw_update: &Value,
        events: &mut Vec<HarnessEvent>,
    ) -> bool {
        if !self.dialect.is_subagent_tool(
            title,
            tc.kind,
            tc.raw_input.as_ref(),
            raw_update,
            self.is_opencode_agent,
        ) {
            return false;
        }
        let call_id = tc.tool_call_id.0.as_ref().to_string();
        let (title, prompt, child_sid) = self
            .dialect
            .extract_subagent_call_fields(tc.raw_input.as_ref(), tc.raw_output.as_ref());
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

    fn take_launcher_update(
        &mut self,
        thread_id: &str,
        session_id: &str,
        launched_by: Option<&str>,
        tcu: &ToolCallUpdate,
        events: &mut Vec<HarnessEvent>,
    ) -> bool {
        let call_id = tcu.tool_call_id.0.as_ref().to_string();
        let key = (session_id.to_string(), call_id.clone());
        let Some(call) = self.pending_subagent_calls.get_mut(&key) else {
            return false;
        };
        if call.thread_id != thread_id {
            return true;
        }
        let (new_title, new_prompt, new_child_sid) = self
            .dialect
            .extract_subagent_call_fields(tcu.fields.raw_input.as_ref(), tcu.fields.raw_output.as_ref());
        let launcher_title = tcu
            .fields
            .title
            .as_deref()
            .map(|t| match launched_by {
                Some(parent) => self
                    .dialect
                    .strip_child_title_prefix(t, self.child_titles.get(parent).map(String::as_str)),
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
        if call.child_session_id.is_none() {
            call.child_session_id = new_child_sid;
        }
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
                    None => events.push(HarnessEvent::Act {
                        thread_id: thread_id.to_string(),
                        harness_call_id: call_id,
                        is_new: false,
                        kind: Some("other".to_string()),
                        title: Some("Subagent".to_string()),
                        state: Some("failed".to_string()),
                        detail,
                    }),
                    Some(parent) => events.push(HarnessEvent::SubagentAct {
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

    fn bind_launcher(
        &mut self,
        thread_id: &str,
        session_id: &str,
        call_id: &str,
        end: Option<&str>,
        error: Option<String>,
        events: &mut Vec<HarnessEvent>,
    ) -> bool {
        let key = (session_id.to_string(), call_id.to_string());
        let Some(call) = self.pending_subagent_calls.get(&key) else {
            return false;
        };
        let Some(cid) = call.child_session_id.clone() else {
            return false;
        };
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
            events.push(HarnessEvent::SubagentUpdated {
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
            events.push(HarnessEvent::SubagentMessage {
                thread_id: thread_id.to_string(),
                harness_subagent_id: cid,
                title,
                role: "prompt".to_string(),
                text,
            });
        }
        true
    }

    pub(super) fn handle_child_update_notification(&mut self, params: Option<&Value>, events: &mut Vec<HarnessEvent>) {
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
        let retired_turn = parent_sid
            .and_then(|s| self.child_retired_turn.get(s).copied())
            .or_else(|| {
                let parent_is_child = parent_sid.is_some_and(|s| self.child_to_thread.contains_key(s));
                if parent_is_child {
                    None
                } else {
                    root_sid
                        .and_then(|s| self.retired_root_to_thread.get(s).and_then(|(_, turn)| *turn))
                        .or_else(|| {
                            parent_sid.and_then(|s| self.retired_root_to_thread.get(s).and_then(|(_, turn)| *turn))
                        })
                }
            });
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
            })
            .or_else(|| root_sid.and_then(|s| self.retired_root_to_thread.get(s).map(|(tid, _)| tid.clone())))
            .or_else(|| parent_sid.and_then(|s| self.retired_root_to_thread.get(s).map(|(tid, _)| tid.clone())));
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
        let had_child = self.child_to_thread.contains_key(child_sid);
        if !self.register_child(&thread_id, child_sid, title) {
            return;
        }
        if !had_child && let Some(turn) = retired_turn {
            self.child_retired_turn.entry(child_sid.to_string()).or_insert(turn);
        }
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
                    self.dialect
                        .extract_child_error(obj.get("error"))
                        .map(|raw| self.redact_for_thread(Some(&thread_id), &raw))
                        .or_else(|| Some("The subagent failed.".to_string()))
                } else {
                    None
                };
                events.push(HarnessEvent::SubagentUpdated {
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

    fn handle_child_session_update(
        &mut self,
        thread_id: &str,
        child_sid: &str,
        known_title: Option<&str>,
        raw_update: &Value,
        events: &mut Vec<HarnessEvent>,
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
                    events.push(HarnessEvent::SubagentMessage {
                        thread_id: thread_id.to_string(),
                        harness_subagent_id: child_sid.to_string(),
                        title: child_title,
                        role: "answer".to_string(),
                        text,
                    });
                }
            }
            Ok(SessionUpdate::UserMessageChunk(chunk)) => {
                let from_launcher = self.child_prompts.get(child_sid) == Some(&PromptFrom::Launcher);
                if !from_launcher && let Some(text) = content_block_text(&chunk.content) {
                    self.child_prompts.insert(child_sid.to_string(), PromptFrom::Stream);
                    events.push(HarnessEvent::SubagentMessage {
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
                let clean_title = self.dialect.strip_child_title_prefix(&tc.title, child_title.as_deref());
                if self.take_launcher_call(thread_id, child_sid, &tc, &clean_title, raw_update, events) {
                    return;
                }
                let detail = tool_detail(&tc.content, tc.raw_output.as_ref(), tc.raw_input.as_ref());
                events.push(HarnessEvent::SubagentAct {
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
                    .map(|t| self.dialect.strip_child_title_prefix(&t, child_title.as_deref()));
                events.push(HarnessEvent::SubagentAct {
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
                        events.push(HarnessEvent::SubagentUpdated {
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

    fn handle_notification(&mut self, value: &Value, events: &mut Vec<HarnessEvent>) {
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
        if self.history_loads.contains_key(session_id) {
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
                    events.push(HarnessEvent::Commands {
                        thread_id: Some(thread_id),
                        commands: harness_commands(update.available_commands),
                    });
                }
            } else if let Some(builder) = self.history_loads.get_mut(session_id) {
                builder.push_update(raw_update, self.dialect.as_ref());
            }
            return;
        }
        let loading = self.loading_sessions.contains(session_id);
        if loading || self.probe_sessions.iter().any(|s| s == session_id) {
            let thread_id = if loading { self.loading_thread(session_id) } else { None };
            if (thread_id.is_some() || !loading)
                && let Ok(SessionUpdate::AvailableCommandsUpdate(update)) =
                    serde_json::from_value::<SessionUpdate>(raw_update.clone())
            {
                events.push(HarnessEvent::Commands {
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
        if let Some((child_sid, child_title)) = self.dialect.extract_child_meta(raw_update) {
            self.handle_child_session_update(&thread_id, &child_sid, child_title.as_deref(), raw_update, events);
            return;
        }

        match serde_json::from_value::<SessionUpdate>(raw_update.clone()) {
            Ok(SessionUpdate::AgentMessageChunk(chunk)) => {
                if let Some(text) = content_block_text(&chunk.content) {
                    events.push(HarnessEvent::TextChunk { thread_id, text });
                } else {
                    let plain = self.render_unknown_update(&thread_id, raw_update);
                    events.push(HarnessEvent::UnknownUpdate {
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
                events.push(HarnessEvent::Act {
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
                events.push(HarnessEvent::Act {
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
                events.push(HarnessEvent::SubagentUpdated {
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
                    events.push(HarnessEvent::SubagentMessage {
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
                    events.push(HarnessEvent::SubagentMessage {
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
                        events.push(HarnessEvent::TitleUpdated {
                            thread_id: thread_id.clone(),
                            title: clean,
                        });
                    }
                }
                if let Some(raw_msg) = self.dialect.extract_transport_retry_failure(raw_update)
                    && !self.transport_failures.contains_key(&thread_id)
                {
                    let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                    self.transport_failures.insert(thread_id, message);
                    let cancel = CancelNotification::new(SessionId::new(session_id.to_string()));
                    self.transport
                        .send_notification(AGENT_METHOD_NAMES.session_cancel, &cancel);
                    let _ = self.transport.on_writable();
                }
            }
            Ok(SessionUpdate::AvailableCommandsUpdate(update)) => {
                events.push(HarnessEvent::Commands {
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
                let mut catalog = extract_model_catalog(&cfg.config_options, self.dialect.as_ref());
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
                    events.push(HarnessEvent::CatalogUpdated {
                        thread_id,
                        catalog,
                        current_model,
                        current_mode,
                        effort_changed,
                    });
                }
            }
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
            Ok(_) | Err(_) => {
                let plain = self.render_unknown_update(&thread_id, raw_update);
                events.push(HarnessEvent::UnknownUpdate {
                    thread_id,
                    raw_json: raw_update.to_string(),
                    plain_text: plain,
                });
            }
        }
    }

    pub(super) fn known_ids(&self, thread_id: Option<&str>) -> Vec<&str> {
        let mut ids: Vec<&str> = Vec::new();
        if let Some(tid) = thread_id {
            ids.extend(self.thread_to_session.get(tid).map(String::as_str));
            ids.extend(
                self.retired_root_to_thread
                    .iter()
                    .filter(|(_, (owner, _))| owner.as_str() == tid)
                    .map(|(sid, _)| sid.as_str()),
            );
            if let Some(tools) = self.thread_tool_ids.get(tid) {
                ids.extend(tools.iter().map(String::as_str));
            }
            if let Some(subs) = self.thread_subagent_ids.get(tid) {
                ids.extend(subs.iter().map(String::as_str));
            }
        } else {
            ids.extend(self.session_to_thread.keys().map(String::as_str));
            ids.extend(self.retired_root_to_thread.keys().map(String::as_str));
            ids.extend(self.child_to_thread.keys().map(String::as_str));
        }
        ids
    }

    pub(super) fn redact_for_thread(&self, thread_id: Option<&str>, input: &str) -> String {
        redact_ids(input, self.known_ids(thread_id))
    }

    fn render_unknown_update(&self, thread_id: &str, raw: &Value) -> String {
        let Some(obj) = raw.as_object() else {
            return self.redact_for_thread(Some(thread_id), &raw.to_string());
        };
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
            let rendered = match v {
                Value::String(s) => s.clone(),
                Value::Bool(b) => b.to_string(),
                Value::Number(n) => n.to_string(),
                _ => continue,
            };
            if !rendered.is_empty() {
                parts.push(redact(&rendered));
            }
        }
        if parts.is_empty() {
            "The Agent sent an update Lever does not recognize.".to_string()
        } else {
            parts.join(" ")
        }
    }
}
