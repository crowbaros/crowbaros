//! Harness event dispatch: maps each normalized [`HarnessEvent`] into state
//! updates, SQLite writes, and outbound [`Effects`].

use super::Service;
use super::turns::TurnDispatch;
use super::wire::{
    ACTIVITY, Broadcast, Effects, NEW_SESSION_NOTE, NEW_SESSION_SOURCE, NO_MODEL_MESSAGE, PeerReply, UNSAVED_MESSAGE,
    UNSAVED_SESSION_MESSAGE, act_event_json, commands_json, default_cwd, efforts_reply_json, error_json,
    models_reply_json, store_failed, subagent_act_event_json, unix_now,
};
use crate::harness::{EffortOption, HarnessCommand, HarnessEvent, ModelCatalog, TurnOutcome};
use crate::log::{Event, Log};
use crate::store::{ActPatch, SubagentActPatch};
use serde_json::json;

impl Service {
    pub(super) fn apply_harness_event(&mut self, event: HarnessEvent, log: &mut Log) -> Effects {
        match event {
            HarnessEvent::SessionDeleted { thread_id, result } => self.finish_delete(&thread_id, result, None, log),
            HarnessEvent::SteerAnswered { steer, taken } => self.steer_answered(steer, taken, log),
            HarnessEvent::Initialized => self.on_initialized(log),
            HarnessEvent::InitializeFailed { message } => self.on_initialize_failed(&message, log),
            HarnessEvent::ModelsProbed(result) => self.on_models_probed(result, log),
            HarnessEvent::SessionReady {
                thread_id,
                session_id,
                afresh,
                catalog,
                current_model,
            } => self.on_session_ready(&thread_id, &session_id, afresh, catalog, current_model, log),
            HarnessEvent::SessionFailed {
                thread_id,
                turn,
                message,
                no_models,
            } => self.on_session_failed(&thread_id, turn, message, no_models, log),
            HarnessEvent::ModelSet {
                thread_id,
                turn,
                model,
                catalog,
            } => self.on_model_set(&thread_id, turn, &model, catalog, log),
            HarnessEvent::ModelSetFailed {
                thread_id,
                turn,
                message,
            } => self.on_model_set_failed(&thread_id, turn, &message, log),
            HarnessEvent::CatalogUpdated {
                thread_id,
                catalog,
                current_model,
                current_mode,
                effort_changed,
            } => self.on_catalog_updated(&thread_id, catalog, current_model, current_mode, effort_changed, log),
            HarnessEvent::ModeSet { thread_id, turn } => self.on_mode_set(&thread_id, turn, log),
            HarnessEvent::ModeSetFailed {
                thread_id,
                turn,
                message,
            } => self.on_mode_set_failed(&thread_id, turn, &message, log),
            HarnessEvent::EffortSet { thread_id, turn } => self.on_effort_set(&thread_id, turn),
            HarnessEvent::EffortSetFailed {
                thread_id,
                turn,
                message,
            } => self.on_effort_set_failed(&thread_id, turn, &message, log),
            HarnessEvent::EffortsProbed { model, result } => self.on_efforts_probed(model, result),
            HarnessEvent::Commands { thread_id, commands } => self.on_commands(thread_id, commands),
            HarnessEvent::PermissionRequested {
                thread_id,
                harness_subagent_id,
                subagent_title,
                handle,
                harness_call_id,
                kind,
                title,
                detail,
                path,
                answers,
            } => {
                if harness_subagent_id.is_none() && !self.turns.deleting(&thread_id) {
                    self.mark_handoff_delivered(&thread_id, log);
                }
                self.record_request(
                    &thread_id,
                    harness_subagent_id.as_deref(),
                    subagent_title.as_deref(),
                    handle,
                    &harness_call_id,
                    kind.as_deref(),
                    &title,
                    detail.as_deref(),
                    path.as_deref(),
                    &answers,
                    log,
                )
            }
            HarnessEvent::PermissionWithdrawn { handle } => self.on_permission_withdrawn(handle, log),
            HarnessEvent::SubagentUpdated {
                thread_id,
                harness_subagent_id,
                title,
                state,
                error,
                from_launcher,
            } => self.handle_subagent_updated(
                &thread_id,
                &harness_subagent_id,
                title.as_deref(),
                state.as_deref(),
                error.as_deref(),
                from_launcher,
                log,
            ),
            HarnessEvent::SubagentMessage {
                thread_id,
                harness_subagent_id,
                title,
                role,
                text,
            } => self.on_subagent_message(&thread_id, &harness_subagent_id, title.as_deref(), &role, &text, log),
            HarnessEvent::SubagentAct {
                thread_id,
                harness_subagent_id,
                subagent_title,
                harness_call_id,
                kind,
                title,
                state,
                detail,
            } => self.on_subagent_act(
                &thread_id,
                &harness_subagent_id,
                subagent_title.as_deref(),
                &harness_call_id,
                kind.as_deref(),
                title.as_deref(),
                state.as_deref(),
                detail.as_deref(),
                log,
            ),
            HarnessEvent::TextChunk { thread_id, text } => self.on_text_chunk(&thread_id, &text, log),
            HarnessEvent::Act {
                thread_id,
                harness_call_id,
                is_new,
                kind,
                title,
                state,
                detail,
            } => self.on_act(
                &thread_id,
                &harness_call_id,
                is_new,
                kind.as_deref(),
                title.as_deref(),
                state.as_deref(),
                detail.as_deref(),
                log,
            ),
            HarnessEvent::TitleUpdated { thread_id, title } => self.on_title_updated(&thread_id, &title, log),
            HarnessEvent::UnknownUpdate {
                thread_id,
                raw_json,
                plain_text,
            } => {
                let after_turn = !self.turns.working_turns.contains_key(&thread_id);
                Effects {
                    broadcasts: self.record_note(&thread_id, &raw_json, &plain_text, after_turn, log),
                    replies: Vec::new(),
                }
            }
            HarnessEvent::PromptFinished {
                thread_id,
                turn,
                outcome,
            } => self.on_prompt_finished(&thread_id, turn, outcome, log),
            HarnessEvent::HistoryLoaded {
                thread_id,
                history,
                catalog,
            } => self.finish_history_loaded(&thread_id, history, catalog, log),
            HarnessEvent::HistoryLoadFailed { thread_id, message } => self.fail_history_load(&thread_id, &message),
            HarnessEvent::SessionsListed(result) => self.on_sessions_listed(result, log),
            HarnessEvent::Exited => Effects::default(),
        }
    }

    fn on_initialized(&mut self, log: &mut Log) -> Effects {
        self.catalog.steering = self
            .harness
            .as_ref()
            .is_some_and(|h| h.capabilities().supports_steering);
        let waiting: Vec<String> = self
            .turns
            .working_turns
            .iter()
            .filter(|(_, t)| matches!(t.dispatch, TurnDispatch::WaitingForInit))
            .map(|(tid, _)| tid.clone())
            .collect();
        for tid in waiting {
            self.dispatch_turn(&tid);
        }
        let needs_probe = std::mem::take(&mut self.catalog.probe_wanted)
            || !self.catalog.pending_models.is_empty()
            || self.catalog.pending_set_models.iter().any(|p| !p.in_flight_on_session);
        if needs_probe && let Some(harness) = self.harness.as_mut() {
            harness.probe_models(&default_cwd());
        }
        if let Some(harness) = self.harness.as_mut() {
            for (_, model) in &self.catalog.pending_efforts {
                harness.probe_efforts(&default_cwd(), model);
            }
        }
        let mut effects = self.dispatch_history_loads();
        effects.extend(self.dispatch_deletes(log));
        effects
    }

    fn on_initialize_failed(&mut self, message: &str, log: &mut Log) -> Effects {
        // What the harness said goes to the turns it fails, which are the
        // person's, and not into the log.
        let _ = log.write(Event::Error, "the harness could not be initialized");
        let tids: Vec<(String, u64)> = self
            .turns
            .working_turns
            .iter()
            .map(|(tid, t)| (tid.clone(), t.turn))
            .collect();
        let mut effects = Effects::default();
        for (tid, turn) in tids {
            effects.broadcasts.extend(self.fail_turn(&tid, turn, message, log));
        }
        effects.replies.extend(self.fail_pending_peer_requests(message));
        self.stop_harness("after initialize failure", log);
        effects
    }

    fn on_models_probed(&mut self, result: Result<ModelCatalog, String>, log: &mut Log) -> Effects {
        let mut effects = Effects::default();
        if self.harness_env_stale {
            let msg = "Lever is restarting the harness after the free hosted Models setting changed; try again when active turns finish.";
            for peer_id in self.catalog.pending_models.drain(..) {
                effects.replies.push(PeerReply {
                    peer_id,
                    message: error_json("working", msg),
                    subscribe: None,
                });
            }
            let mut remaining = Vec::new();
            for item in std::mem::take(&mut self.catalog.pending_set_models) {
                if item.in_flight_on_session {
                    remaining.push(item);
                } else if let Some(peer_id) = item.peer_id {
                    effects.replies.push(PeerReply {
                        peer_id,
                        message: error_json("working", msg),
                        subscribe: None,
                    });
                }
            }
            self.catalog.pending_set_models = remaining;
            return effects;
        }
        match result {
            Ok(catalog) => {
                self.record_catalog_choice(&catalog, log);
                let free = self.free_hosted_models();
                let steering = self.catalog.steering;
                for peer_id in self.catalog.pending_models.drain(..) {
                    effects.replies.push(PeerReply {
                        peer_id,
                        message: models_reply_json(&catalog, free, steering),
                        subscribe: None,
                    });
                }
                let mut remaining = Vec::new();
                let pending = std::mem::take(&mut self.catalog.pending_set_models);
                for item in pending {
                    if item.in_flight_on_session {
                        remaining.push(item);
                    } else {
                        let outcome = self.finish_set_model_with_catalog(&item.thread_id, &item.model, &catalog, log);
                        effects.broadcasts.extend(outcome.broadcasts);
                        if let (Some(peer_id), Some(reply)) = (item.peer_id, outcome.reply) {
                            effects.replies.push(PeerReply {
                                peer_id,
                                message: reply,
                                subscribe: outcome.subscribe,
                            });
                        }
                    }
                }
                self.catalog.pending_set_models = remaining;
                self.catalog.commands_probed = true;
                self.apply_model_catalog(catalog, true, log, &mut effects);
            }
            Err(msg) => {
                for peer_id in self.catalog.pending_models.drain(..) {
                    effects.replies.push(PeerReply {
                        peer_id,
                        message: error_json("harness-error", &msg),
                        subscribe: None,
                    });
                }
                let mut remaining = Vec::new();
                for item in std::mem::take(&mut self.catalog.pending_set_models) {
                    if item.in_flight_on_session {
                        remaining.push(item);
                    } else if let Some(peer_id) = item.peer_id {
                        effects.replies.push(PeerReply {
                            peer_id,
                            message: error_json("harness-error", &msg),
                            subscribe: None,
                        });
                    }
                }
                self.catalog.pending_set_models = remaining;
            }
        }
        effects
    }

    fn on_session_ready(
        &mut self,
        thread_id: &str,
        session_id: &str,
        afresh: bool,
        catalog: Option<ModelCatalog>,
        current_model: Option<String>,
        log: &mut Log,
    ) -> Effects {
        let mut effects = Effects::default();
        if let Some(cat) = catalog {
            self.apply_model_catalog(cat, false, log, &mut effects);
        }
        // The record must say the session is this thread's before Lever uses
        // it. Otherwise the record could still give its ID to the thread that
        // had it before, to load at a later start.
        if let Err(err) = self.store.set_harness_session_id(thread_id, session_id) {
            store_failed(log, &err);
            if let Some(harness) = self.harness.as_mut() {
                harness.forget_session(thread_id);
            }
            let waiting = self
                .turns
                .working_turns
                .get(thread_id)
                .filter(|active| matches!(active.dispatch, TurnDispatch::WaitingForSession))
                .map(|active| active.turn);
            if let Some(turn) = waiting {
                effects
                    .broadcasts
                    .extend(self.fail_turn(thread_id, turn, UNSAVED_SESSION_MESSAGE, log));
            }
            return effects;
        }
        if afresh {
            let after_turn = !self.turns.working_turns.contains_key(thread_id);
            effects.broadcasts.extend(self.record_note(
                thread_id,
                NEW_SESSION_SOURCE,
                NEW_SESSION_NOTE,
                after_turn,
                log,
            ));
        }
        // The session is the thread's. The turn that waits for it is the one
        // working now: the turn that first asked may have been stopped since.
        let desired_model = self
            .turns
            .working_turns
            .get(thread_id)
            .filter(|active| matches!(active.dispatch, TurnDispatch::WaitingForSession))
            .and_then(|active| active.desired_model.clone());
        if let Some(active) = self.turns.working_turns.get_mut(thread_id)
            && matches!(active.dispatch, TurnDispatch::WaitingForSession)
            && let Some(harness) = self.harness.as_mut()
        {
            if let Some(model) = desired_model
                && current_model.as_deref() != Some(model.as_str())
            {
                harness.set_session_model(thread_id, Some(active.turn), session_id, &model);
                active.dispatch = TurnDispatch::WaitingForModel;
            } else {
                if let Some(ref model) = current_model {
                    self.note_provider_models_seen(true, log);
                    let changes = self.model_changes(thread_id, model);
                    match self.store.set_thread_model(thread_id, model, unix_now()) {
                        Ok(()) => effects.broadcasts.extend(changes),
                        Err(err) => store_failed(log, &err),
                    }
                }
                effects.broadcasts.extend(self.mirror_effort(thread_id, log));
                self.continue_turn(thread_id);
            }
        }
        effects
    }

    fn on_session_failed(
        &mut self,
        thread_id: &str,
        turn: u64,
        message: String,
        no_models: bool,
        log: &mut Log,
    ) -> Effects {
        let mut effects = Effects::default();
        let display = if no_models {
            let empty = ModelCatalog {
                supported: true,
                models: Vec::new(),
                default_model: None,
                modes: Vec::new(),
            };
            self.apply_model_catalog(empty, true, log, &mut effects);
            NO_MODEL_MESSAGE.to_string()
        } else {
            message
        };
        effects
            .broadcasts
            .extend(self.fail_turn(thread_id, turn, &display, log));
        effects
    }

    fn on_model_set(
        &mut self,
        thread_id: &str,
        turn: Option<u64>,
        model: &str,
        catalog: Option<ModelCatalog>,
        log: &mut Log,
    ) -> Effects {
        let mut effects = Effects::default();
        if let Some(cat) = catalog {
            self.apply_model_catalog(cat, false, log, &mut effects);
        }
        if let Some(t) = turn {
            let still_waiting = self
                .turns
                .working_turns
                .get(thread_id)
                .is_some_and(|active| active.turn == t && matches!(active.dispatch, TurnDispatch::WaitingForModel));
            if !still_waiting {
                if let Some(pos) = self
                    .catalog
                    .pending_set_models
                    .iter()
                    .position(|p| p.in_flight_on_session && p.peer_id.is_none() && p.thread_id == thread_id)
                {
                    self.catalog.pending_set_models.remove(pos);
                }
                return effects;
            }
        }
        let changes = self.model_changes(thread_id, model);
        if let Err(err) = self.store.set_thread_model(thread_id, model, unix_now()) {
            store_failed(log, &err);
            if let Some(t) = turn {
                effects
                    .broadcasts
                    .extend(self.fail_turn(thread_id, t, UNSAVED_MESSAGE, log));
            } else if let Some(pos) = self
                .catalog
                .pending_set_models
                .iter()
                .position(|p| p.in_flight_on_session && p.thread_id == thread_id)
            {
                let item = self.catalog.pending_set_models.remove(pos);
                if let Some(peer_id) = item.peer_id {
                    effects.replies.push(PeerReply {
                        peer_id,
                        message: error_json("storage-error", &err),
                        subscribe: None,
                    });
                }
            }
            return effects;
        }
        effects.broadcasts.extend(changes);
        self.catalog.pending_effort_threads.remove(thread_id);
        if let Some(t) = turn {
            if self
                .turns
                .working_turns
                .get(thread_id)
                .is_some_and(|active| active.turn == t && matches!(active.dispatch, TurnDispatch::WaitingForModel))
            {
                effects.broadcasts.extend(self.mirror_effort(thread_id, log));
                self.continue_turn(thread_id);
            }
        } else if let Some(pos) = self
            .catalog
            .pending_set_models
            .iter()
            .position(|p| p.in_flight_on_session && p.thread_id == thread_id)
        {
            let item = self.catalog.pending_set_models.remove(pos);
            effects.broadcasts.extend(self.mirror_effort(thread_id, log));
            if let Some(peer_id) = item.peer_id {
                effects.replies.push(PeerReply {
                    peer_id,
                    message: json!({
                        "reply": "model-set",
                        "thread": thread_id,
                        "model": model,
                    }),
                    subscribe: None,
                });
            }
        }
        effects
    }

    fn on_model_set_failed(&mut self, thread_id: &str, turn: Option<u64>, message: &str, log: &mut Log) -> Effects {
        let mut effects = Effects::default();
        if let Some(t) = turn {
            if self
                .turns
                .working_turns
                .get(thread_id)
                .is_some_and(|active| active.turn == t)
            {
                effects.broadcasts.extend(self.fail_turn(thread_id, t, message, log));
            } else if let Some(pos) = self
                .catalog
                .pending_set_models
                .iter()
                .position(|p| p.in_flight_on_session && p.peer_id.is_none() && p.thread_id == thread_id)
            {
                self.catalog.pending_set_models.remove(pos);
            }
        } else if let Some(pos) = self
            .catalog
            .pending_set_models
            .iter()
            .position(|p| p.in_flight_on_session && p.thread_id == thread_id)
        {
            let item = self.catalog.pending_set_models.remove(pos);
            if let Some(peer_id) = item.peer_id {
                effects.replies.push(PeerReply {
                    peer_id,
                    message: error_json("invalid-model", message),
                    subscribe: None,
                });
            }
        }
        effects
    }

    fn on_catalog_updated(
        &mut self,
        thread_id: &str,
        catalog: Option<ModelCatalog>,
        current_model: Option<String>,
        current_mode: Option<String>,
        effort_changed: bool,
        log: &mut Log,
    ) -> Effects {
        let mut effects = Effects::default();
        if let Some(catalog) = catalog {
            self.apply_model_catalog(catalog, false, log, &mut effects);
        }
        if let Some(harness) = self.harness.as_ref()
            && let Some(session_id) = harness.session_for_thread(thread_id)
            && let Some(model) = harness.session_current_model(session_id)
            && let Some(selector) = harness.session_effort(session_id)
        {
            self.catalog.efforts.insert(model.to_string(), selector.options.clone());
        }
        // What the harness says the session is at is the thread's, once Lever
        // has set the session to what the thread asks. A `set-effort` saved
        // while idle stays pending until the next turn applies it, unless the
        // session switched Models.
        if !self.settling(thread_id) {
            let mut model_changed = false;
            if let Some(model) = current_model {
                let changes = self.model_changes(thread_id, &model);
                model_changed = changes.is_some();
                match self.store.set_thread_model(thread_id, &model, unix_now()) {
                    Ok(()) => effects.broadcasts.extend(changes),
                    Err(err) => store_failed(log, &err),
                }
            }
            if model_changed {
                self.catalog.pending_effort_threads.remove(thread_id);
                effects.broadcasts.extend(self.mirror_effort(thread_id, log));
            } else if effort_changed && !self.catalog.pending_effort_threads.contains(thread_id) {
                effects.broadcasts.extend(self.mirror_effort(thread_id, log));
            }
        }
        // The harness changed the session's mode itself: the thread is in that
        // mode now. A turn that has yet to send its prompt sets its own mode on
        // the session still, and keeps it.
        if let Some(mode) = current_mode {
            let prompt_sent = self.turns.working_turns.get(thread_id).is_none_or(|a| {
                matches!(
                    a.dispatch,
                    TurnDispatch::PromptInFlight { .. } | TurnDispatch::HarnessStarted { .. }
                )
            });
            // While a mode Lever asked for settles, what the harness says the
            // mode is goes by: how the change ends says what the thread's is.
            let settling_mode = self.catalog.has_mode_in_flight(thread_id);
            if prompt_sent && !settling_mode {
                if let Some(active) = self.turns.working_turns.get_mut(thread_id) {
                    active.mode = mode.clone();
                }
                if let Err(err) = self.store.set_thread_mode(thread_id, &mode, unix_now()) {
                    self.could_not_save(thread_id, &err, log);
                }
                effects.broadcasts.push(Broadcast {
                    thread_id: thread_id.to_string(),
                    message: json!({ "event": "thread-updated", "thread": thread_id, "mode": mode }),
                });
            }
        }
        effects
    }

    fn on_mode_set(&mut self, thread_id: &str, turn: Option<u64>, log: &mut Log) -> Effects {
        if turn.is_some()
            && self
                .turns
                .working_turns
                .get(thread_id)
                .is_some_and(|a| Some(a.turn) == turn && matches!(a.dispatch, TurnDispatch::WaitingForMode))
        {
            self.continue_turn(thread_id);
            return Effects::default();
        }
        self.catalog.modes_in_flight.retain(|(t, _)| t != thread_id);
        if turn.is_none() {
            return self.finish_set_mode(thread_id, None, log);
        }
        Effects::default()
    }

    fn on_mode_set_failed(&mut self, thread_id: &str, turn: Option<u64>, message: &str, log: &mut Log) -> Effects {
        self.catalog.modes_in_flight.retain(|(t, _)| t != thread_id);
        match turn {
            Some(turn) => Effects {
                broadcasts: self.fail_turn(thread_id, turn, message, log),
                replies: Vec::new(),
            },
            None => self.finish_set_mode(thread_id, Some(message), log),
        }
    }

    fn on_effort_set(&mut self, thread_id: &str, turn: u64) -> Effects {
        self.catalog.pending_effort_threads.remove(thread_id);
        if self
            .turns
            .working_turns
            .get(thread_id)
            .is_some_and(|a| a.turn == turn && matches!(a.dispatch, TurnDispatch::WaitingForEffort))
        {
            self.continue_turn(thread_id);
        } else {
            self.catalog.modes_in_flight.retain(|(t, _)| t != thread_id);
        }
        Effects::default()
    }

    fn on_effort_set_failed(&mut self, thread_id: &str, turn: u64, message: &str, log: &mut Log) -> Effects {
        self.catalog.pending_effort_threads.remove(thread_id);
        self.catalog.modes_in_flight.retain(|(t, _)| t != thread_id);
        let mut broadcasts = self.fail_turn(thread_id, turn, message, log);
        // The thread is at the effort the harness has it at, not the one it
        // refused, so the next turn does not ask for it again.
        broadcasts.extend(self.mirror_effort(thread_id, log));
        Effects {
            broadcasts,
            replies: Vec::new(),
        }
    }

    fn on_efforts_probed(&mut self, model: String, result: Result<Vec<EffortOption>, String>) -> Effects {
        let mut effects = Effects::default();
        let waiting: Vec<u64> = self
            .catalog
            .pending_efforts
            .iter()
            .filter(|(_, wanted)| *wanted == model)
            .map(|(peer_id, _)| *peer_id)
            .collect();
        self.catalog.pending_efforts.retain(|(_, wanted)| *wanted != model);
        if self.harness_env_stale {
            let msg = "Lever is restarting the harness after the free hosted Models setting changed; try again when active turns finish.";
            for peer_id in waiting {
                effects.replies.push(PeerReply {
                    peer_id,
                    message: error_json("working", msg),
                    subscribe: None,
                });
            }
            return effects;
        }
        let message = match result {
            Ok(levels) => {
                let reply = efforts_reply_json(&model, &levels);
                self.catalog.efforts.insert(model, levels);
                reply
            }
            Err(message) => error_json("harness-error", &message),
        };
        for peer_id in waiting {
            effects.replies.push(PeerReply {
                peer_id,
                message: message.clone(),
                subscribe: None,
            });
        }
        effects
    }

    fn on_commands(&mut self, thread_id: Option<String>, commands: Vec<HarnessCommand>) -> Effects {
        let mut effects = Effects::default();
        match thread_id {
            Some(thread_id) => {
                if self.catalog.thread_commands.get(&thread_id) != Some(&commands) {
                    effects.broadcasts.push(Broadcast {
                        thread_id: thread_id.clone(),
                        message: json!({
                            "event": "commands",
                            "thread": thread_id,
                            "commands": commands_json(&commands),
                        }),
                    });
                    self.catalog.thread_commands.insert(thread_id, commands);
                }
            }
            None => {
                if self.catalog.commands.as_ref() != Some(&commands) {
                    effects.broadcasts.push(Broadcast {
                        thread_id: ACTIVITY.to_string(),
                        message: json!({ "event": "commands", "commands": commands_json(&commands) }),
                    });
                    self.catalog.commands = Some(commands);
                }
            }
        }
        effects
    }

    fn on_permission_withdrawn(&mut self, handle: u64, log: &mut Log) -> Effects {
        let id = self
            .turns
            .waiting
            .iter()
            .find(|(_, w)| w.handle == handle)
            .map(|(id, _)| id.clone());
        let mut effects = Effects::default();
        if let Some(id) = id {
            effects.broadcasts = self.end_request(&id, "withdrawn", None, log);
        }
        effects
    }

    fn on_subagent_message(
        &mut self,
        thread_id: &str,
        harness_subagent_id: &str,
        title: Option<&str>,
        role: &str,
        text: &str,
        log: &mut Log,
    ) -> Effects {
        let mut broadcasts = Vec::new();
        let Some((turn, sub)) = self.ensure_subagent(thread_id, harness_subagent_id, title, &mut broadcasts, log)
        else {
            return Effects::default();
        };
        match self
            .store
            .append_subagent_message(thread_id, &sub.id, role, text, unix_now())
        {
            Ok(true) => {
                broadcasts.push(Broadcast {
                    thread_id: thread_id.to_string(),
                    message: json!({
                        "event": "subagent-text",
                        "thread": thread_id,
                        "turn": turn,
                        "subagent": sub.id,
                        "role": role,
                        "text": text,
                    }),
                });
            }
            Ok(false) => {}
            Err(err) => self.could_not_save(thread_id, &err, log),
        }
        Effects {
            broadcasts,
            replies: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_subagent_act(
        &mut self,
        thread_id: &str,
        harness_subagent_id: &str,
        subagent_title: Option<&str>,
        harness_call_id: &str,
        kind: Option<&str>,
        title: Option<&str>,
        state: Option<&str>,
        detail: Option<&str>,
        log: &mut Log,
    ) -> Effects {
        let mut broadcasts = Vec::new();
        let Some((turn, sub)) =
            self.ensure_subagent(thread_id, harness_subagent_id, subagent_title, &mut broadcasts, log)
        else {
            return Effects::default();
        };
        let patch = SubagentActPatch {
            subagent_id: &sub.id,
            thread_id,
            harness_call_id,
            kind,
            title,
            state,
            detail,
        };
        match self.store.upsert_subagent_act(&patch, unix_now()) {
            Ok(act) => {
                broadcasts.push(Broadcast {
                    thread_id: thread_id.to_string(),
                    message: subagent_act_event_json(thread_id, turn, &sub.id, &act),
                });
            }
            Err(err) => self.could_not_save(thread_id, &err, log),
        }
        Effects {
            broadcasts,
            replies: Vec::new(),
        }
    }

    fn on_text_chunk(&mut self, thread_id: &str, text: &str, log: &mut Log) -> Effects {
        let mut broadcasts = Vec::new();
        let Some(turn) = self.turn_for_session_update(thread_id, true, &mut broadcasts, log) else {
            return Effects::default();
        };
        self.mark_handoff_delivered(thread_id, log);
        // The answer still goes to who is watching. The turn's end says that
        // not all of it was saved.
        if let Err(err) = self.store.append_answer(thread_id, turn, text, unix_now()) {
            self.could_not_save(thread_id, &err, log);
        }
        broadcasts.push(Broadcast {
            thread_id: thread_id.to_string(),
            message: json!({
                "event": "text",
                "thread": thread_id,
                "turn": turn,
                "text": text,
            }),
        });
        Effects {
            broadcasts,
            replies: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_act(
        &mut self,
        thread_id: &str,
        harness_call_id: &str,
        is_new: bool,
        kind: Option<&str>,
        title: Option<&str>,
        state: Option<&str>,
        detail: Option<&str>,
        log: &mut Log,
    ) -> Effects {
        // A new act wakes a thread whose subagents ran; an update of an act
        // goes to the turn the act is in.
        let mut broadcasts = Vec::new();
        let Some(turn) = self.turn_for_session_update(thread_id, is_new, &mut broadcasts, log) else {
            return Effects::default();
        };
        self.mark_handoff_delivered(thread_id, log);
        let patch = ActPatch {
            thread_id,
            turn_number: turn,
            harness_call_id,
            is_new,
            kind,
            title,
            state,
            detail,
        };
        let (recorded_turn, act) = match self.store.upsert_act(&patch, unix_now()) {
            Ok(saved) => saved,
            Err(err) => {
                self.could_not_save(thread_id, &err, log);
                return Effects {
                    broadcasts,
                    replies: Vec::new(),
                };
            }
        };
        self.turns
            .note_harness_act(thread_id, recorded_turn, &act.id, &act.state);
        broadcasts.push(Broadcast {
            thread_id: thread_id.to_string(),
            message: act_event_json(thread_id, recorded_turn, &act),
        });
        Effects {
            broadcasts,
            replies: Vec::new(),
        }
    }

    fn on_title_updated(&mut self, thread_id: &str, title: &str, log: &mut Log) -> Effects {
        if let Err(err) = self.store.update_thread_title(thread_id, title, unix_now()) {
            self.could_not_save(thread_id, &err, log);
            return Effects::default();
        }
        Effects {
            broadcasts: vec![Broadcast {
                thread_id: thread_id.to_string(),
                message: json!({
                    "event": "thread-updated",
                    "thread": thread_id,
                    "title": title,
                }),
            }],
            replies: Vec::new(),
        }
    }

    fn on_prompt_finished(&mut self, thread_id: &str, turn: u64, outcome: TurnOutcome, log: &mut Log) -> Effects {
        // An answer for a turn that is not the one working ends nothing.
        if self
            .turns
            .working_turns
            .get(thread_id)
            .is_none_or(|active| active.turn != turn)
        {
            return Effects::default();
        }
        if matches!(outcome, TurnOutcome::Done) {
            self.mark_handoff_delivered(thread_id, log);
        }
        let unsaved = self
            .turns
            .working_turns
            .remove(thread_id)
            .is_some_and(|active| active.unsaved);
        let outcome = match outcome {
            TurnOutcome::Done if unsaved => TurnOutcome::Failed(UNSAVED_MESSAGE.to_string()),
            other => other,
        };
        let broadcasts = match outcome {
            TurnOutcome::Done => {
                self.end_turn(thread_id, turn, "done", None, log);
                vec![Broadcast {
                    thread_id: thread_id.to_string(),
                    message: json!({
                        "event": "done",
                        "thread": thread_id,
                        "turn": turn,
                    }),
                }]
            }
            TurnOutcome::Stopped => {
                self.end_turn(thread_id, turn, "stopped", None, log);
                vec![Broadcast {
                    thread_id: thread_id.to_string(),
                    message: json!({
                        "event": "stopped",
                        "thread": thread_id,
                        "turn": turn,
                    }),
                }]
            }
            TurnOutcome::Failed(msg) => {
                self.end_turn(thread_id, turn, "failed", Some(&msg), log);
                vec![Broadcast {
                    thread_id: thread_id.to_string(),
                    message: json!({
                        "event": "failed",
                        "thread": thread_id,
                        "turn": turn,
                        "message": msg,
                    }),
                }]
            }
        };
        Effects {
            broadcasts,
            replies: Vec::new(),
        }
    }
}
