//! Decomposed JSON-RPC response handlers for [`AcpHarness`].

use super::discovery::{MAX_LIST_PAGES, session_info_to_external};
use super::format::{extract_effort, extract_model_catalog, extract_modes, model_option, redact_ids};
use super::history::HistoryBuilder;
use super::{AcpHarness, PendingRpc};
use crate::harness::{Harness, HarnessEvent, ModelCatalog, TurnOutcome};
use crate::mcp::McpServerSpec;
use agent_client_protocol_schema::ProtocolVersion;
use agent_client_protocol_schema::v1::{
    AGENT_METHOD_NAMES, CloseSessionRequest, Error as AcpError, ErrorCode, InitializeResponse, ListSessionsRequest,
    ListSessionsResponse, LoadSessionResponse, NewSessionResponse, PromptResponse, SessionConfigId, SessionId,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse, StopReason,
};
use serde_json::Value;

impl AcpHarness {
    pub(super) fn handle_response(&mut self, value: Value, events: &mut Vec<HarnessEvent>) {
        let Some(id) = value.get("id").and_then(Value::as_i64) else {
            return;
        };
        let Some(pending) = self.pending.remove(&id) else {
            return;
        };

        // A result that carries `"error": null` beside it is still a result.
        if let Some(err_val) = value.get("error").filter(|v| !v.is_null()) {
            self.handle_error_response(pending, err_val, events);
            return;
        }

        let result = value.get("result").cloned().unwrap_or(Value::Null);
        match pending {
            PendingRpc::DeleteSession { thread_id, session_id } => {
                self.retired_root_to_thread.remove(&session_id);
                events.push(HarnessEvent::SessionDeleted {
                    thread_id,
                    result: Ok(()),
                });
            }
            PendingRpc::Initialize => self.on_initialize_result(result, events),
            PendingRpc::ProbeModels => self.on_probe_models_result(result, events),
            PendingRpc::CloseProbe => {}
            PendingRpc::NewSession {
                thread_id,
                turn,
                afresh,
                epoch,
            } => self.on_new_session_result(thread_id, turn, afresh, epoch, result, events),
            PendingRpc::LoadSession {
                thread_id,
                turn,
                session_id,
                cwd,
                mcp_servers,
                epoch,
            } => self.on_load_session_result(thread_id, turn, session_id, cwd, mcp_servers, epoch, result, events),
            PendingRpc::LoadHistory { thread_id, session_id } => {
                self.on_load_history_result(thread_id, session_id, result, events);
            }
            PendingRpc::ListSessions { page, prev_cursor } => {
                self.on_list_sessions_result(page, prev_cursor, result, events);
            }
            PendingRpc::SetModel {
                thread_id,
                turn,
                session_id,
                model,
            } => self.on_set_model_result(thread_id, turn, session_id, model, result, events),
            PendingRpc::SetMode {
                thread_id,
                turn,
                session_id,
                mode,
            } => self.on_set_mode_result(thread_id, turn, session_id, mode, result, events),
            PendingRpc::SetEffort {
                thread_id,
                turn,
                session_id,
                effort,
            } => self.on_set_effort_result(thread_id, turn, session_id, effort, result, events),
            PendingRpc::ProbeEffortSession { model } => self.on_probe_effort_session_result(model, result, events),
            PendingRpc::ProbeEffortModel { model, session_id } => {
                self.on_probe_effort_model_result(model, session_id, result, events);
            }
            PendingRpc::Steer { steer, .. } => {
                events.push(HarnessEvent::SteerAnswered {
                    steer,
                    taken: self.dialect.parse_steer_response(&result),
                });
            }
            PendingRpc::Prompt { thread_id, turn } => self.on_prompt_result(thread_id, turn, result, events),
        }
    }

    fn handle_error_response(&mut self, pending: PendingRpc, err_val: &Value, events: &mut Vec<HarnessEvent>) {
        let error = serde_json::from_value::<AcpError>(err_val.clone()).ok();
        let cancelled = error.as_ref().is_some_and(|e| e.code == ErrorCode::RequestCancelled);
        let raw_msg = error
            .map(|e| e.message)
            .unwrap_or_else(|| "The harness reported an error.".to_string());
        let no_models = self.dialect.is_no_models_error(err_val, &raw_msg);

        match pending {
            PendingRpc::DeleteSession { thread_id, .. } => {
                let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                events.push(HarnessEvent::SessionDeleted {
                    thread_id,
                    result: Err(message),
                });
            }
            PendingRpc::Initialize => events.push(HarnessEvent::InitializeFailed {
                message: self.redact_for_thread(None, &raw_msg),
            }),
            PendingRpc::ProbeModels => {
                self.probing_models = false;
                if no_models {
                    events.push(HarnessEvent::ModelsProbed(Ok(ModelCatalog {
                        supported: true,
                        models: Vec::new(),
                        default_model: None,
                        config_id: None,
                        modes: Vec::new(),
                    })));
                } else {
                    events.push(HarnessEvent::ModelsProbed(Err(self.redact_for_thread(None, &raw_msg))));
                }
            }
            PendingRpc::CloseProbe => {}
            PendingRpc::NewSession {
                thread_id, turn, epoch, ..
            } => {
                if epoch == self.current_session_gen(&thread_id) {
                    let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                    events.push(HarnessEvent::SessionFailed {
                        thread_id,
                        turn,
                        message,
                        no_models,
                    });
                }
            }
            PendingRpc::LoadSession {
                thread_id,
                turn,
                session_id,
                cwd,
                mcp_servers,
                epoch,
            } => {
                self.loading_sessions.remove(&session_id);
                if epoch == self.current_session_gen(&thread_id) {
                    self.request_new_session(&thread_id, turn, &cwd, true, &mcp_servers, epoch);
                }
            }
            PendingRpc::LoadHistory { thread_id, session_id } => {
                self.history_loads.remove(&session_id);
                let message = redact_ids(
                    &self.redact_for_thread(Some(&thread_id), &raw_msg),
                    vec![session_id.as_str()],
                );
                events.push(HarnessEvent::HistoryLoadFailed { thread_id, message });
            }
            PendingRpc::ListSessions { .. } => {
                self.listing_sessions = false;
                self.live_sessions_acc.clear();
                self.live_sessions_seen.clear();
                events.push(HarnessEvent::SessionsListed(Err(self.redact_for_thread(None, &raw_msg))));
            }
            PendingRpc::SetModel { thread_id, turn, .. } => {
                let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                events.push(HarnessEvent::ModelSetFailed {
                    thread_id,
                    turn,
                    message,
                });
            }
            PendingRpc::SetMode { thread_id, turn, .. } => {
                let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                events.push(HarnessEvent::ModeSetFailed {
                    thread_id,
                    turn,
                    message,
                });
            }
            PendingRpc::SetEffort { thread_id, turn, .. } => {
                let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                events.push(HarnessEvent::EffortSetFailed {
                    thread_id,
                    turn,
                    message,
                });
            }
            PendingRpc::ProbeEffortSession { model } => {
                self.probing_efforts.remove(&model);
                events.push(HarnessEvent::EffortsProbed {
                    model,
                    result: Err(self.redact_for_thread(None, &raw_msg)),
                });
            }
            PendingRpc::ProbeEffortModel { model, session_id } => {
                self.probing_efforts.remove(&model);
                self.close_probe(SessionId::new(session_id));
                events.push(HarnessEvent::EffortsProbed {
                    model,
                    result: Err(self.redact_for_thread(None, &raw_msg)),
                });
            }
            PendingRpc::Prompt { thread_id, turn } if cancelled => {
                let outcome = match self.transport_failures.remove(&thread_id) {
                    Some(message) => TurnOutcome::Failed(message),
                    None => TurnOutcome::Stopped,
                };
                events.push(HarnessEvent::PromptFinished {
                    thread_id,
                    turn,
                    outcome,
                });
            }
            PendingRpc::Prompt { thread_id, turn } => {
                self.transport_failures.remove(&thread_id);
                let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                events.push(HarnessEvent::PromptFinished {
                    thread_id,
                    turn,
                    outcome: TurnOutcome::Failed(message),
                });
            }
            PendingRpc::Steer { thread_id, steer } => {
                let message = self.redact_for_thread(Some(&thread_id), &raw_msg);
                events.push(HarnessEvent::SteerAnswered {
                    steer,
                    taken: Err(format!("The Agent did not take your message into its turn: {message}")),
                });
            }
        }
    }

    fn on_initialize_result(&mut self, result: Value, events: &mut Vec<HarnessEvent>) {
        match serde_json::from_value::<InitializeResponse>(result) {
            Ok(resp) if resp.protocol_version == ProtocolVersion::V1 => {
                self.initialized = true;
                self.supports_load_session = resp.agent_capabilities.load_session;
                self.supports_list_sessions = resp.agent_capabilities.session_capabilities.list.is_some();
                self.supports_close_session = resp.agent_capabilities.session_capabilities.close.is_some();
                self.supports_delete_session = resp.agent_capabilities.session_capabilities.delete.is_some();
                self.supports_steering = self.dialect.steering_supported(resp.meta.as_ref());
                self.is_opencode_agent = resp
                    .agent_info
                    .as_ref()
                    .is_some_and(|info| info.name.eq_ignore_ascii_case("opencode"));
                events.push(HarnessEvent::Initialized);
            }
            Ok(_) => events.push(HarnessEvent::InitializeFailed {
                message: "The harness uses an unsupported protocol version.".to_string(),
            }),
            Err(_) => events.push(HarnessEvent::InitializeFailed {
                message: "The harness returned an invalid initialization response.".to_string(),
            }),
        }
    }

    fn on_probe_models_result(&mut self, result: Value, events: &mut Vec<HarnessEvent>) {
        self.probing_models = false;
        match serde_json::from_value::<NewSessionResponse>(result) {
            Ok(resp) => {
                let mut catalog = resp
                    .config_options
                    .as_deref()
                    .and_then(|opts| extract_model_catalog(opts, self.dialect.as_ref()))
                    .unwrap_or(ModelCatalog {
                        supported: false,
                        models: Vec::new(),
                        default_model: None,
                        config_id: None,
                        modes: Vec::new(),
                    });
                if let Some(selector) = resp
                    .config_options
                    .as_deref()
                    .and_then(|opts| extract_modes(opts, self.dialect.as_ref()))
                {
                    self.mode_config_id = Some(selector.config_id);
                    catalog.modes = selector.modes;
                }
                if let Some(ref cid) = catalog.config_id {
                    self.model_config_id = Some(cid.clone());
                }
                let efforts = catalog.default_model.clone().map(|model| HarnessEvent::EffortsProbed {
                    model,
                    result: Ok(resp
                        .config_options
                        .as_deref()
                        .and_then(extract_effort)
                        .map(|selector| selector.options)
                        .unwrap_or_default()),
                });
                self.close_probe(resp.session_id);
                events.push(HarnessEvent::ModelsProbed(Ok(catalog)));
                events.extend(efforts);
            }
            Err(_) => events.push(HarnessEvent::ModelsProbed(Err(
                "The harness returned an invalid session response.".to_string(),
            ))),
        }
    }

    fn on_new_session_result(
        &mut self,
        thread_id: String,
        turn: u64,
        afresh: bool,
        epoch: u64,
        result: Value,
        events: &mut Vec<HarnessEvent>,
    ) {
        if epoch != self.current_session_gen(&thread_id) {
            if let Ok(resp) = serde_json::from_value::<NewSessionResponse>(result)
                && !resp.session_id.0.is_empty()
            {
                self.close_probe(resp.session_id);
            }
            return;
        }
        match serde_json::from_value::<NewSessionResponse>(result) {
            Ok(resp) if !resp.session_id.0.is_empty() => {
                let sid = resp.session_id.0.as_ref().to_string();
                let mut catalog = resp
                    .config_options
                    .as_deref()
                    .and_then(|opts| extract_model_catalog(opts, self.dialect.as_ref()));
                let current_model = catalog.as_ref().and_then(|c| c.default_model.clone());
                if let Some(ref cid) = catalog.as_ref().and_then(|c| c.config_id.clone()) {
                    self.model_config_id = Some(cid.clone());
                }
                if self.bind_session(&thread_id, &sid) {
                    if let Some(ref m) = current_model {
                        self.session_models.insert(sid.clone(), m.clone());
                    }
                    self.take_effort(&sid, resp.config_options.as_deref().unwrap_or(&[]));
                    let modes = self.take_modes(&sid, resp.config_options.as_deref());
                    if let (Some(cat), Some(selector)) = (catalog.as_mut(), modes.as_ref()) {
                        cat.modes = selector.modes.clone();
                    }
                    events.push(HarnessEvent::SessionReady {
                        thread_id,
                        session_id: sid,
                        afresh,
                        catalog,
                        current_model,
                    });
                } else {
                    events.push(HarnessEvent::SessionFailed {
                        thread_id,
                        turn,
                        message: "The harness gave this thread a session that another thread is using.".to_string(),
                        no_models: false,
                    });
                }
            }
            _ => events.push(HarnessEvent::SessionFailed {
                thread_id,
                turn,
                message: "The harness could not start a thread.".to_string(),
                no_models: false,
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_load_session_result(
        &mut self,
        thread_id: String,
        turn: u64,
        session_id: String,
        cwd: String,
        mcp_servers: Vec<McpServerSpec>,
        epoch: u64,
        result: Value,
        events: &mut Vec<HarnessEvent>,
    ) {
        self.loading_sessions.remove(&session_id);
        if epoch != self.current_session_gen(&thread_id) {
            if self.supports_close_session {
                let close = CloseSessionRequest::new(SessionId::new(session_id));
                let id = self.transport.send_request(AGENT_METHOD_NAMES.session_close, &close);
                self.pending.insert(id, PendingRpc::CloseProbe);
                let _ = self.transport.on_writable();
            }
            return;
        }
        match serde_json::from_value::<LoadSessionResponse>(result) {
            Ok(resp) if self.bind_session(&thread_id, &session_id) => {
                let mut catalog = resp
                    .config_options
                    .as_deref()
                    .and_then(|opts| extract_model_catalog(opts, self.dialect.as_ref()));
                let current_model = catalog.as_ref().and_then(|c| c.default_model.clone());
                if let Some(ref cid) = catalog.as_ref().and_then(|c| c.config_id.clone()) {
                    self.model_config_id = Some(cid.clone());
                }
                if let Some(ref m) = current_model {
                    self.session_models.insert(session_id.clone(), m.clone());
                }
                self.take_effort(&session_id, resp.config_options.as_deref().unwrap_or(&[]));
                let modes = self.take_modes(&session_id, resp.config_options.as_deref());
                if let (Some(cat), Some(selector)) = (catalog.as_mut(), modes.as_ref()) {
                    cat.modes = selector.modes.clone();
                }
                events.push(HarnessEvent::SessionReady {
                    thread_id,
                    session_id,
                    afresh: false,
                    catalog,
                    current_model,
                });
            }
            _ => self.request_new_session(&thread_id, turn, &cwd, true, &mcp_servers, epoch),
        }
    }

    fn on_load_history_result(
        &mut self,
        thread_id: String,
        session_id: String,
        result: Value,
        events: &mut Vec<HarnessEvent>,
    ) {
        let builder = self.history_loads.remove(&session_id);
        match serde_json::from_value::<LoadSessionResponse>(result) {
            Ok(resp) if self.bind_session(&thread_id, &session_id) => {
                let mut catalog = resp
                    .config_options
                    .as_deref()
                    .and_then(|opts| extract_model_catalog(opts, self.dialect.as_ref()));
                let current_model = catalog.as_ref().and_then(|c| c.default_model.clone());
                if let Some(ref cid) = catalog.as_ref().and_then(|c| c.config_id.clone()) {
                    self.model_config_id = Some(cid.clone());
                }
                if let Some(ref m) = current_model {
                    self.session_models.insert(session_id.clone(), m.clone());
                }
                self.take_effort(&session_id, resp.config_options.as_deref().unwrap_or(&[]));
                let effort = self.session_efforts.get(&session_id).map(|e| e.current.clone());
                let modes = self.take_modes(&session_id, resp.config_options.as_deref());
                if let (Some(cat), Some(selector)) = (catalog.as_mut(), modes.as_ref()) {
                    cat.modes = selector.modes.clone();
                }
                let (mut history, tool_ids, subagent_ids) = builder
                    .unwrap_or_else(|| HistoryBuilder::new(self.is_opencode_agent))
                    .finish(current_model.clone(), effort);
                if !tool_ids.is_empty() {
                    self.remember_tool_ids(&thread_id, tool_ids);
                }
                if !subagent_ids.is_empty() {
                    for cid in &subagent_ids {
                        let _ = self.register_child(&thread_id, cid, None);
                    }
                }
                if let Some(ref title) = history.title
                    && title.trim().is_empty()
                {
                    history.title = None;
                }
                events.push(HarnessEvent::HistoryLoaded {
                    thread_id,
                    history,
                    catalog,
                });
            }
            Ok(_) => events.push(HarnessEvent::HistoryLoadFailed {
                thread_id,
                message: "The session is already in use by another thread.".to_string(),
            }),
            Err(_) => events.push(HarnessEvent::HistoryLoadFailed {
                thread_id,
                message: "The harness returned an invalid session response.".to_string(),
            }),
        }
    }

    fn on_list_sessions_result(
        &mut self,
        page: usize,
        prev_cursor: Option<String>,
        result: Value,
        events: &mut Vec<HarnessEvent>,
    ) {
        match serde_json::from_value::<ListSessionsResponse>(result) {
            Ok(resp) => {
                let mut added = 0;
                for info in &resp.sessions {
                    if let Some(ext) = session_info_to_external(info)
                        && self.live_sessions_seen.insert(ext.harness_session_id.clone())
                    {
                        self.live_sessions_acc.push(ext);
                        added += 1;
                    }
                }
                let next = resp
                    .next_cursor
                    .filter(|c| !c.is_empty() && Some(c) != prev_cursor.as_ref());
                if added > 0
                    && page + 1 < MAX_LIST_PAGES
                    && let Some(cursor) = next
                {
                    let req = ListSessionsRequest::new().cursor(cursor.clone());
                    let id = self.transport.send_request(AGENT_METHOD_NAMES.session_list, &req);
                    self.pending.insert(
                        id,
                        PendingRpc::ListSessions {
                            page: page + 1,
                            prev_cursor: Some(cursor),
                        },
                    );
                    let _ = self.transport.on_writable();
                } else {
                    self.listing_sessions = false;
                    let sessions = std::mem::take(&mut self.live_sessions_acc);
                    self.live_sessions_seen.clear();
                    events.push(HarnessEvent::SessionsListed(Ok(sessions)));
                }
            }
            Err(_) => {
                self.listing_sessions = false;
                self.live_sessions_acc.clear();
                self.live_sessions_seen.clear();
                events.push(HarnessEvent::SessionsListed(Err(
                    "The harness returned an invalid session list response.".to_string(),
                )));
            }
        }
    }

    fn on_set_model_result(
        &mut self,
        thread_id: String,
        turn: Option<u64>,
        session_id: String,
        model: String,
        result: Value,
        events: &mut Vec<HarnessEvent>,
    ) {
        match serde_json::from_value::<SetSessionConfigOptionResponse>(result) {
            Ok(resp) => {
                let mut catalog = extract_model_catalog(&resp.config_options, self.dialect.as_ref());
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
                self.take_effort(&session_id, &resp.config_options);
                self.session_models.insert(session_id, applied.clone());
                events.push(HarnessEvent::ModelSet {
                    thread_id,
                    turn,
                    model: applied,
                    catalog,
                });
            }
            Err(_) => events.push(HarnessEvent::ModelSetFailed {
                thread_id,
                turn,
                message: "The harness returned an invalid configuration response.".to_string(),
            }),
        }
    }

    fn on_set_mode_result(
        &mut self,
        thread_id: String,
        turn: Option<u64>,
        session_id: String,
        mode: String,
        result: Value,
        events: &mut Vec<HarnessEvent>,
    ) {
        match serde_json::from_value::<SetSessionConfigOptionResponse>(result) {
            Ok(resp) => {
                self.take_effort(&session_id, &resp.config_options);
                let selector = self.take_modes(&session_id, Some(&resp.config_options));
                match selector {
                    Some(sel) if sel.current.as_deref() != Some(mode.as_str()) => {
                        events.push(HarnessEvent::ModeSetFailed {
                            thread_id,
                            turn,
                            message: "The harness did not take this thread's mode.".to_string(),
                        });
                    }
                    _ => {
                        self.session_modes.insert(session_id, mode);
                        events.push(HarnessEvent::ModeSet { thread_id, turn });
                    }
                }
            }
            Err(_) => events.push(HarnessEvent::ModeSetFailed {
                thread_id,
                turn,
                message: "The harness returned an invalid configuration response.".to_string(),
            }),
        }
    }

    fn on_set_effort_result(
        &mut self,
        thread_id: String,
        turn: u64,
        session_id: String,
        effort: String,
        result: Value,
        events: &mut Vec<HarnessEvent>,
    ) {
        match serde_json::from_value::<SetSessionConfigOptionResponse>(result) {
            Ok(resp) => {
                self.take_modes(&session_id, Some(&resp.config_options));
                let told = extract_effort(&resp.config_options).or_else(|| self.session_efforts.get(&session_id).cloned());
                if let Some(mut selector) = told {
                    selector.current = effort;
                    self.session_efforts.insert(session_id, selector);
                }
                events.push(HarnessEvent::EffortSet { thread_id, turn });
            }
            Err(_) => events.push(HarnessEvent::EffortSetFailed {
                thread_id,
                turn,
                message: "The harness returned an invalid configuration response.".to_string(),
            }),
        }
    }

    fn on_probe_effort_session_result(&mut self, model: String, result: Value, events: &mut Vec<HarnessEvent>) {
        match serde_json::from_value::<NewSessionResponse>(result) {
            Ok(resp) if !resp.session_id.0.is_empty() => {
                let options = resp.config_options.as_deref().unwrap_or(&[]);
                let at_model = model_option(options)
                    .and_then(|(_, select)| (select.current_value.0.as_ref() == model).then_some(()))
                    .is_some();
                if at_model {
                    let efforts = extract_effort(options).map(|s| s.options).unwrap_or_default();
                    self.probing_efforts.remove(&model);
                    self.close_probe(resp.session_id);
                    events.push(HarnessEvent::EffortsProbed {
                        model,
                        result: Ok(efforts),
                    });
                } else {
                    let config_id = model_option(options)
                        .map(|(option, _)| option.id.0.as_ref().to_string())
                        .or_else(|| self.model_config_id.clone())
                        .unwrap_or_else(|| "model".to_string());
                    let session_id = resp.session_id.0.as_ref().to_string();
                    self.remember_probe_session(session_id.clone());
                    let req = SetSessionConfigOptionRequest::new(
                        resp.session_id,
                        SessionConfigId::new(config_id),
                        model.as_str(),
                    );
                    let id = self
                        .transport
                        .send_request(AGENT_METHOD_NAMES.session_set_config_option, &req);
                    self.pending
                        .insert(id, PendingRpc::ProbeEffortModel { model, session_id });
                    let _ = self.transport.on_writable();
                }
            }
            _ => {
                self.probing_efforts.remove(&model);
                events.push(HarnessEvent::EffortsProbed {
                    model,
                    result: Err("The harness returned an invalid session response.".to_string()),
                });
            }
        }
    }

    fn on_probe_effort_model_result(
        &mut self,
        model: String,
        session_id: String,
        result: Value,
        events: &mut Vec<HarnessEvent>,
    ) {
        self.probing_efforts.remove(&model);
        let outcome = match serde_json::from_value::<SetSessionConfigOptionResponse>(result) {
            Ok(resp) => Ok(extract_effort(&resp.config_options)
                .map(|s| s.options)
                .unwrap_or_default()),
            Err(_) => Err("The harness returned an invalid configuration response.".to_string()),
        };
        self.close_probe(SessionId::new(session_id));
        events.push(HarnessEvent::EffortsProbed { model, result: outcome });
    }

    fn on_prompt_result(&mut self, thread_id: String, turn: u64, result: Value, events: &mut Vec<HarnessEvent>) {
        let transport_failure = self.transport_failures.remove(&thread_id);
        match serde_json::from_value::<PromptResponse>(result) {
            Ok(resp) => {
                let outcome = match resp.stop_reason {
                    StopReason::EndTurn => TurnOutcome::Done,
                    StopReason::Cancelled => match transport_failure {
                        Some(message) => TurnOutcome::Failed(message),
                        None => TurnOutcome::Stopped,
                    },
                    StopReason::Refusal => TurnOutcome::Failed("The harness refused to continue.".to_string()),
                    StopReason::MaxTokens => TurnOutcome::Failed("The turn reached its token limit.".to_string()),
                    StopReason::MaxTurnRequests => {
                        TurnOutcome::Failed("The turn reached its request limit.".to_string())
                    }
                    _ => TurnOutcome::Failed("The turn ended unexpectedly.".to_string()),
                };
                events.push(HarnessEvent::PromptFinished {
                    thread_id,
                    turn,
                    outcome,
                });
            }
            Err(_) => events.push(HarnessEvent::PromptFinished {
                thread_id,
                turn,
                outcome: TurnOutcome::Failed("The harness returned an invalid turn response.".to_string()),
            }),
        }
    }
}
