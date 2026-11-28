//! Peer request routing (`status`, `list`, `resume`, `watch`, `send`) and
//! decomposed turn-initiation flow.

use super::Service;
use super::discovery::PendingSend;
use super::turns::{TurnDispatch, Wanted, WorkingTurn};
use super::wire::{
    ACTIVITY, Broadcast, NO_MODEL_MESSAGE, RequestOutcome, THREAD_LIST_SUBSCRIPTION, default_cwd, error_outcome,
    initial_title, store_failed, thread_record_json, thread_summary_json, unix_now,
};
use crate::fulcrum;
use crate::handoff;
use crate::log::Log;
use crate::store::NewThread;
use crate::sys;
use crate::version::LEVER_VERSION;
use serde_json::{Value, json};
use std::path::Path;

/// Validated fields from a `send` request.
struct ParsedSend<'a> {
    text: &'a str,
    requested_thread: Option<&'a str>,
    requested_cwd: Option<&'a str>,
    requested_model: Option<&'a str>,
    requested_mode: Option<&'a str>,
    requested_effort: Option<&'a str>,
}

/// Resolved target thread state ready to begin a new turn.
struct SendTarget {
    thread_id: String,
    cwd: String,
    existing_session_id: Option<String>,
    desired_model: Option<String>,
    desired_effort: Option<String>,
    mode: String,
    new_thread: bool,
    mode_changed: bool,
    effort_changed: bool,
    needs_handoff: bool,
    woken: Option<u64>,
}

impl Service {
    pub fn handle_request(
        &mut self,
        peer_id: u64,
        is_person: bool,
        is_tool: bool,
        value: &Value,
        log: &mut Log,
    ) -> RequestOutcome {
        let mut outcome = self.handle_op(peer_id, is_person, is_tool, value, log);
        outcome.broadcasts.extend(self.drain_queues(log));
        // An answer can free a prompt that waited on it.
        self.send_held_prompts();
        // An answer or a Stop can leave the harness with nothing to do.
        self.refresh_idle_timer();
        outcome.broadcasts.extend(self.turns.activity_changed());
        outcome
    }

    fn handle_op(
        &mut self,
        peer_id: u64,
        is_person: bool,
        is_tool: bool,
        value: &Value,
        log: &mut Log,
    ) -> RequestOutcome {
        let Some(op) = value.as_object().and_then(|o| o.get("op")).and_then(Value::as_str) else {
            return error_outcome("invalid-request", "Expected a JSON object with a string op field.");
        };

        if is_tool {
            return match op {
                "mcp-call" => self.handle_mcp_call(value, log),
                _ => error_outcome("unsupported", "This request is not supported on the tool socket."),
            };
        }

        match op {
            "answer" => self.handle_answer(value, log),
            "set-mode" => self.handle_set_mode(peer_id, value, log),
            "set-default-mode" => self.handle_set_default_mode(value, log),
            "watch" => {
                let (working, waiting) = self.turns.current_activity();
                RequestOutcome {
                    reply: Some(json!({ "reply": "activity", "working": working, "waiting": waiting })),
                    subscribe: Some(ACTIVITY.to_string()),
                    broadcasts: Vec::new(),
                }
            }
            "status" => self.handle_status(),
            "list" => self.handle_list(value),
            "archive" | "delete" => self.handle_lifecycle(peer_id, op, value, log),
            "make-project" => self.handle_make_project(value, log),
            "list-dirs" => self.handle_list_dirs(value),
            "send" => self.send_message(peer_id, is_person, value, None, log),
            "unqueue" => self.handle_unqueue(value, log),
            "resume" => self.handle_resume(peer_id, value, log),
            "stop" => self.handle_stop(value, log),
            "models" => self.handle_models(peer_id, value, log),
            "set-free-models" => self.handle_set_free_models(value, log),
            "set-model" => self.handle_set_model(peer_id, value, log),
            "commands" => self.handle_commands(value, log),
            "efforts" => self.handle_efforts(peer_id, value, log),
            "set-effort" => self.handle_set_effort(value, log),
            _ => error_outcome("unsupported", "This request is not supported."),
        }
    }

    fn handle_status(&self) -> RequestOutcome {
        let harnesses: Vec<Value> = self
            .harness_config
            .iter()
            .map(|cfg| json!({ "id": cfg.id, "name": cfg.name }))
            .collect();
        RequestOutcome {
            reply: Some(json!({
                "reply": "status",
                "version": LEVER_VERSION,
                "harness_started": self.harness.is_some(),
                "harnesses": harnesses,
                "free_hosted_models": self.free_hosted_models(),
                "models_chosen": self.models_chosen(),
                "default_mode": self.default_mode(),
            })),
            subscribe: None,
            broadcasts: Vec::new(),
        }
    }

    fn handle_list(&self, value: &Value) -> RequestOutcome {
        for field in ["watch", "archived"] {
            if value.get(field).is_some_and(|v| !v.is_boolean()) {
                return error_outcome("invalid-request", "Expected watch and archived to be booleans.");
            }
        }
        let watch = value.get("watch").and_then(Value::as_bool).unwrap_or(false);
        let archived = value.get("archived").and_then(Value::as_bool).unwrap_or(false);
        match self.store.list_threads() {
            Ok(mut threads) => {
                for thread in &mut threads {
                    let has_working_turn = self.turns.working_turns.contains_key(&thread.id);
                    let has_working_subagent = self.turns.has_working_subagent(&thread.id);
                    let has_waiting_request = self.turns.has_waiting_request(&thread.id);
                    self.journal.overlay_thread_summary(
                        thread,
                        has_working_turn,
                        has_working_subagent,
                        has_waiting_request,
                    );
                }
                RequestOutcome {
                    reply: Some(json!({
                        "reply": "list",
                        "threads": threads.iter().filter(|t| watch || archived || !t.archived).map(thread_summary_json).collect::<Vec<_>>(),
                    })),
                    subscribe: watch.then(|| THREAD_LIST_SUBSCRIPTION.into()),
                    broadcasts: Vec::new(),
                }
            }
            Err(err) => error_outcome("storage-error", &err),
        }
    }

    /// A list watch is a metadata-only subscription; it never starts the harness.
    pub fn thread_list_event(&self) -> Option<Value> {
        let mut reply = self.handle_list(&json!({"watch":true})).reply?;
        if reply["reply"] != "list" {
            return None;
        }
        reply.as_object_mut()?.remove("reply");
        reply["event"] = json!("threads");
        Some(reply)
    }

    fn handle_resume(&mut self, peer_id: u64, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(thread_id) = value.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "Expected a non-empty string thread field.");
        };
        let meta = match self.store.thread_meta(thread_id) {
            Ok(Some(meta)) => meta,
            Ok(None) => return error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
            Err(err) => return error_outcome("storage-error", &err),
        };
        // A thread discovered from a harness's own store reads its history from
        // the harness into Lever's record when opened, before answering.
        if meta.needs_history
            && let Some(session_id) = meta.harness_session_id.as_deref().filter(|s| !s.is_empty())
        {
            if let Err((code, msg)) = self.ensure_history_load(thread_id, session_id, &meta.cwd, log) {
                return error_outcome(code, &msg);
            }
            self.discovery.pending_resumes.push((peer_id, thread_id.to_string()));
            return RequestOutcome {
                reply: None,
                subscribe: None,
                broadcasts: Vec::new(),
            };
        }
        self.resume_from_store(thread_id)
    }

    pub(super) fn resume_from_store(&self, thread_id: &str) -> RequestOutcome {
        match self.store.get_thread(thread_id) {
            Ok(Some(mut record)) => {
                // As in `list`: whoever resumes the thread now was not
                // watching when its turn ended, and no event will say so again.
                self.journal.overlay_thread_record(thread_id, &mut record);
                record.working = record.turns.last().is_some_and(|turn| turn.state == "working")
                    || record
                        .turns
                        .iter()
                        .any(|turn| turn.subagents.iter().any(|sub| sub.state == "working"));
                RequestOutcome {
                    reply: Some(thread_record_json(&record)),
                    subscribe: Some(thread_id.to_string()),
                    broadcasts: Vec::new(),
                }
            }
            Ok(None) => error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
            Err(err) => error_outcome("storage-error", &err),
        }
    }

    fn parse_send_request<'a>(&self, value: &'a Value) -> Result<ParsedSend<'a>, RequestOutcome> {
        let Some(text) = value
            .get("text")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        else {
            return Err(error_outcome("invalid-request", "Expected a non-empty string text field."));
        };
        let requested_thread = match value.get("thread") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
            _ => return Err(error_outcome("invalid-request", "Expected thread to be a string or null.")),
        };
        let requested_cwd = match value.get("cwd") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if Path::new(s).is_absolute() => Some(s.as_str()),
            _ => return Err(error_outcome("invalid-request", "Expected cwd to be an absolute path.")),
        };
        let requested_model = match value.get("model") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.as_str()),
            _ => {
                return Err(error_outcome(
                    "invalid-request",
                    "Expected model to be a non-empty string or null.",
                ));
            }
        };
        let requested_mode = match value.get("mode") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => {
                self.check_mode(s)?;
                Some(s.as_str())
            }
            _ => return Err(error_outcome("invalid-request", "Expected mode to be a string or null.")),
        };
        let requested_effort = match value.get("effort") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.as_str()),
            _ => {
                return Err(error_outcome(
                    "invalid-request",
                    "Expected effort to be a non-empty string or null.",
                ));
            }
        };
        Ok(ParsedSend {
            text,
            requested_thread,
            requested_cwd,
            requested_model,
            requested_mode,
            requested_effort,
        })
    }

    fn resolve_send_target(
        &mut self,
        peer_id: u64,
        is_person: bool,
        value: &Value,
        parsed: &ParsedSend<'_>,
        from_queue: bool,
        log: &mut Log,
    ) -> Result<SendTarget, RequestOutcome> {
        let woken = parsed
            .requested_thread
            .and_then(|tid| self.turns.harness_turn_to_take_over(tid));
        let Some(tid) = parsed.requested_thread else {
            let tid = format!("t-{}", sys::random_hex(6));
            let cwd = parsed.requested_cwd.map(str::to_string).unwrap_or_else(default_cwd);
            let mode = parsed
                .requested_mode
                .map(str::to_string)
                .unwrap_or_else(|| self.default_mode());
            return Ok(SendTarget {
                thread_id: tid,
                cwd,
                existing_session_id: None,
                desired_model: parsed.requested_model.map(str::to_string),
                desired_effort: parsed.requested_effort.map(str::to_string),
                mode,
                new_thread: true,
                mode_changed: false,
                effort_changed: parsed.requested_effort.is_some(),
                needs_handoff: false,
                woken: None,
            });
        };

        let meta = match self.store.thread_meta(tid) {
            Ok(Some(meta)) => meta,
            Ok(None) => return Err(error_outcome("no-thread", &format!("There is no thread {tid}."))),
            Err(err) => return Err(error_outcome("storage-error", &err)),
        };

        // If a client sends directly to an unopened discovered thread before
        // calling `resume`, load its history into Lever's record first so the
        // new message becomes turn N+1.
        if !from_queue
            && meta.needs_history
            && let Some(session_id) = meta.harness_session_id.as_deref().filter(|s| !s.is_empty())
        {
            if let Err((code, msg)) = self.ensure_history_load(tid, session_id, &meta.cwd, log) {
                return Err(error_outcome(code, &msg));
            }
            self.discovery.pending_sends.push(PendingSend {
                peer_id,
                is_person,
                thread_id: tid.to_string(),
                value: value.clone(),
                stopped: false,
            });
            return Err(RequestOutcome {
                reply: None,
                subscribe: None,
                broadcasts: Vec::new(),
            });
        }

        let busy = if woken.is_some() {
            self.session_change_pending(tid)
        } else {
            meta.working_turn.is_some() || self.thread_busy(tid)
        };
        let wanted = Wanted {
            mode: parsed.requested_mode,
            model: parsed.requested_model,
            effort: parsed.requested_effort,
        };
        // A message for a thread with messages waiting waits behind them.
        let steer_pending = !self.turns.working_turns.contains_key(tid) && self.turns.has_pending_steer(tid);
        if !from_queue && (self.has_queued(tid) || steer_pending) {
            return Err(self.queue_message(peer_id, is_person, tid, &meta, parsed.text, wanted, log));
        }
        if busy {
            // A message sent while the person's turn works is taken into that
            // turn, where the harness can, and otherwise waits behind it.
            if woken.is_none()
                && let Some((turn, session_id)) = self.steerable_turn(tid)
            {
                return Err(self.steer_into_turn(
                    peer_id,
                    is_person,
                    tid,
                    turn,
                    &session_id,
                    &meta,
                    parsed.text,
                    wanted,
                    log,
                ));
            }
            if woken.is_none() && self.queueable_turn(tid) {
                return Err(self.queue_message(peer_id, is_person, tid, &meta, parsed.text, wanted, log));
            }
            return Err(error_outcome("working", "This thread is already working on a turn."));
        }
        if self.subagent_request_holds_prompt(tid) {
            return Err(error_outcome(
                "subagent-asks",
                "A subagent of this thread waits for your answer. Answer its request, or stop the thread, first.",
            ));
        }

        let mode_changed = parsed.requested_mode.is_some_and(|m| m != meta.mode);
        let effort_changed = parsed.requested_effort.is_some_and(|e| meta.effort.as_deref() != Some(e));
        let model = parsed
            .requested_model
            .map(str::to_string)
            .or(meta.model)
            .or_else(|| {
                self.catalog
                    .model_catalog
                    .as_ref()
                    .and_then(|c| c.default_model.clone())
            });
        let effort = parsed.requested_effort.map(str::to_string).or(meta.effort);
        let mode = parsed.requested_mode.map(str::to_string).unwrap_or(meta.mode);
        Ok(SendTarget {
            thread_id: tid.to_string(),
            cwd: meta.cwd,
            existing_session_id: meta.harness_session_id,
            desired_model: model,
            desired_effort: effort,
            mode,
            new_thread: false,
            mode_changed,
            effort_changed,
            needs_handoff: meta.needs_handoff,
            woken,
        })
    }

    /// Sends a message to a thread, or begins one. `from_queue` is the
    /// desktop context of a queued message that now goes as its own turn.
    pub(super) fn send_message(
        &mut self,
        peer_id: u64,
        is_person: bool,
        value: &Value,
        from_queue: Option<(Option<Value>, Option<String>)>,
        log: &mut Log,
    ) -> RequestOutcome {
        let parsed = match self.parse_send_request(value) {
            Ok(p) => p,
            Err(outcome) => return outcome,
        };
        let Some(harness_name) = self.harness_config.as_ref().map(|c| c.name.clone()) else {
            return error_outcome("no-harness", "No harness is available.");
        };
        if self.harness_env_stale {
            return error_outcome(
                "working",
                "Lever is restarting the harness after the free hosted Models setting changed; try again when active turns finish.",
            );
        }
        if self
            .catalog
            .model_catalog
            .as_ref()
            .is_some_and(|c| c.supported && c.models.is_empty())
        {
            return error_outcome("no-model", NO_MODEL_MESSAGE);
        }

        // A thread whose last turn has ended, with that end still to be
        // saved, is not working, and is not free either: the record must
        // have how the turn ended before the next one begins.
        self.save_endings();
        if let Some(tid) = parsed.requested_thread
            && self.journal.has_unsaved_for_thread(tid)
        {
            return error_outcome(
                "storage-error",
                "Lever is still saving how this thread's last turn ended.",
            );
        }

        let target = match self.resolve_send_target(peer_id, is_person, value, &parsed, from_queue.is_some(), log) {
            Ok(target) => target,
            Err(outcome) => return outcome,
        };
        if let Some(effort) = parsed.requested_effort
            && let Err(outcome) = self.check_effort(target.desired_model.as_deref(), effort)
        {
            return outcome;
        }

        let (context_value, context_prompt) = match from_queue {
            Some(context) => context,
            None => self.desktop_context(is_person),
        };
        let context_json = context_value.as_ref().map(Value::to_string);
        let now = unix_now();
        let title = initial_title(parsed.text);

        // A new thread's requested Model is persisted only after the harness
        // accepts it (in `SessionReady` or `ModelSet`), so a rejected `--model`
        // does not stick on later turns.
        let thread = target.new_thread.then_some(NewThread {
            title: &title,
            cwd: &target.cwd,
            harness_name: Some(harness_name.as_str()),
            mode: &target.mode,
        });
        let mut broadcasts = Vec::new();
        if let (Some(tid), Some(turn)) = (parsed.requested_thread, target.woken) {
            broadcasts.push(self.end_harness_turn(tid, turn, log));
            // The record must have how that turn ended before the next one
            // begins, as for any turn.
            if self.journal.has_unsaved_turn_ending(tid) {
                let mut outcome = error_outcome(
                    "storage-error",
                    "Lever is still saving how this thread's last turn ended.",
                );
                outcome.broadcasts = broadcasts;
                return outcome;
            }
        }
        let turn = match self.store.begin_turn(
            &target.thread_id,
            thread,
            parsed.text,
            context_json.as_deref(),
            parsed.requested_mode.filter(|_| !target.new_thread),
            now,
        ) {
            Ok(turn) => {
                self.turns.held_stops.remove(&target.thread_id);
                turn
            }
            Err(err) => {
                store_failed(log, &err);
                // The turn the harness began has ended all the same.
                let mut outcome = error_outcome("storage-error", &err);
                outcome.broadcasts = broadcasts;
                return outcome;
            }
        };

        let mut msg_event = json!({
            "event": "message",
            "thread": target.thread_id,
            "turn": turn,
            "text": parsed.text,
        });
        if let Some(ctx) = context_value {
            msg_event["context"] = ctx;
        }
        if target.mode_changed {
            broadcasts.push(Broadcast {
                thread_id: target.thread_id.clone(),
                message: json!({ "event": "thread-updated", "thread": target.thread_id, "mode": target.mode }),
            });
        }
        // The effort a message asks for is the thread's from this turn on.
        // Where the thread's Model turns out not to offer it, the thread
        // takes the effort the harness has it at.
        if target.effort_changed {
            broadcasts.extend(self.save_effort(&target.thread_id, parsed.requested_effort, log));
        }
        broadcasts.push(Broadcast {
            thread_id: target.thread_id.clone(),
            message: msg_event,
        });

        let handoff_prompt = if target.needs_handoff && !target.new_thread {
            self.store
                .get_thread(&target.thread_id)
                .ok()
                .flatten()
                .and_then(|rec| {
                    handoff::render_thread_handoff(&rec, &target.cwd, Some(turn), handoff::MAX_HANDOFF_BYTES)
                })
        } else {
            None
        };

        // Detach any pre-move session that stayed mapped while idle so this
        // turn opens a fresh session in the project's folder.
        self.flush_pending_session_reset(&target.thread_id);

        let handoff_pending = handoff_prompt.is_some();
        self.idle_since = None;
        self.turns.working_turns.insert(
            target.thread_id.clone(),
            WorkingTurn {
                turn,
                message: parsed.text.to_string(),
                context_prompt,
                handoff_prompt,
                handoff_pending,
                cwd: target.cwd,
                existing_session_id: target.existing_session_id,
                desired_model: target.desired_model,
                desired_effort: target.desired_effort,
                mode: target.mode,
                dispatch: TurnDispatch::WaitingForInit,
                unsaved: false,
                stop_asked: None,
                heard_before_prompt: false,
            },
        );

        // Ensure the harness child is running, then dispatch session/prompt.
        if self.harness.is_none()
            && let Err(err) = self.start_harness(log)
        {
            broadcasts.extend(self.fail_turn(&target.thread_id, turn, &err, log));
            return RequestOutcome {
                reply: Some(json!({ "reply": "sent", "thread": target.thread_id, "turn": turn })),
                subscribe: Some(target.thread_id),
                broadcasts,
            };
        }

        if self.harness.as_ref().is_some_and(|h| h.is_initialized()) {
            self.dispatch_turn(&target.thread_id);
        }

        RequestOutcome {
            reply: Some(json!({
                "reply": "sent",
                "thread": target.thread_id,
                "turn": turn,
            })),
            subscribe: Some(target.thread_id),
            broadcasts,
        }
    }

    /// The desktop context a message carries, as the record keeps it and as
    /// the harness is told it. Messages from the Agent's surface carry it,
    /// fetched from Fulcrum (active window title/app_id, windows by
    /// workspace, displays — never window contents or clipboard). With
    /// Fulcrum away, none is sent.
    pub(super) fn desktop_context(&self, is_person: bool) -> (Option<Value>, Option<String>) {
        let desktop_context = if !is_person {
            self.fulcrum_socket
                .as_deref()
                .and_then(|path| fulcrum::fetch_snapshot(path, fulcrum::SNAPSHOT_TIMEOUT))
        } else {
            None
        };
        (
            desktop_context.as_ref().and_then(|ctx| serde_json::to_value(ctx).ok()),
            desktop_context.as_ref().map(fulcrum::format_desktop_context),
        )
    }
}
