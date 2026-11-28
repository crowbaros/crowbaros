//! Cached harness catalog state (Models, Modes, Efforts, Commands, steering
//! capability) and the peer requests that query or mutate them.

use super::Service;
use super::turns::TurnDispatch;
use super::wire::{
    ACTIVITY, Broadcast, Effects, NO_MODEL_MESSAGE, PeerReply, RequestOutcome, STARTING_MODES, commands_json,
    default_cwd, efforts_reply_json, error_json, error_outcome, models_event_json, models_reply_json, store_failed,
    unix_now,
};
use crate::harness::{self, EffortOption, EffortSelector, HarnessCommand, ModelCatalog};
use crate::log::Log;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// A mode the person set between turns, sent to the thread's live session:
/// it is the thread's once the session takes it, so a turn the harness begins
/// meanwhile runs, and is recorded, in the mode the session is in.
pub(super) struct PendingSetMode {
    /// Peer waiting for the reply, or `None` if it went away.
    pub(super) peer_id: Option<u64>,
    pub(super) thread_id: String,
    pub(super) mode: String,
    /// The mode the thread had, which the session goes back to if the
    /// record cannot take the new one.
    pub(super) previous: String,
    /// The session is going back to `mode`, the thread's still, after the
    /// record did not take another.
    pub(super) reverting: bool,
}

pub(super) struct PendingSetModel {
    /// Peer waiting for the reply, or `None` if that peer disconnected while
    /// `session/set_config_option` was already in flight on the harness.
    pub(super) peer_id: Option<u64>,
    pub(super) thread_id: String,
    pub(super) model: String,
    /// True when `session/set_config_option` is already in flight on an active
    /// session; false when waiting on the harness's Model catalogue probe.
    pub(super) in_flight_on_session: bool,
}

#[derive(Default)]
pub(super) struct CatalogState {
    pub(super) model_catalog: Option<ModelCatalog>,
    /// True when `models` with `refresh: true` arrived while the harness was
    /// busy, so Lever restarts and re-probes Models as soon as active work
    /// finishes.
    pub(super) models_refresh_pending: bool,
    pub(super) pending_models: Vec<u64>,
    pub(super) pending_set_models: Vec<PendingSetModel>,
    pub(super) pending_set_modes: Vec<PendingSetMode>,
    /// The harness last started says it can take a message into a turn under
    /// way.
    pub(super) steering: bool,
    /// The effort levels of each Model, as the harness now running said them.
    /// A Model with none has an empty list.
    pub(super) efforts: HashMap<String, Vec<EffortOption>>,
    /// Peers that wait for a Model's effort levels.
    pub(super) pending_efforts: Vec<(u64, String)>,
    /// The commands the harness last reported for a session Lever opened to
    /// read what it offers, and those each thread's session last reported.
    pub(super) commands: Option<Vec<HarnessCommand>>,
    pub(super) thread_commands: HashMap<String, Vec<HarnessCommand>>,
    /// Whether the running harness has already completed a probe for what it
    /// offers, so polling `commands` does not re-probe when a harness sends
    /// no `available_commands_update`.
    pub(super) commands_probed: bool,
    /// Threads whose effort was saved via `set-effort` while idle and has not
    /// yet been applied to the harness session on the next turn.
    pub(super) pending_effort_threads: HashSet<String>,
    /// What the harness offers is to be read once it is initialized.
    pub(super) probe_wanted: bool,
    /// Threads whose session the harness is setting to a mode or effort with
    /// no turn waiting on it: a turn stopped while it was set, or a mode the
    /// person set between turns. Each stays busy until the harness answers,
    /// so no turn after it runs in a mode it did not ask for. With when Lever
    /// asked, so a harness that never answers is ended after `stop_timeout`,
    /// as one that ignores a Stop is.
    pub(super) modes_in_flight: Vec<(String, Instant)>,
}

impl CatalogState {
    pub(super) fn has_active_work(&self) -> bool {
        !self.pending_models.is_empty()
            || !self.pending_set_models.is_empty()
            || !self.pending_efforts.is_empty()
            || !self.modes_in_flight.is_empty()
            || self.probe_wanted
    }

    pub(super) fn reset_on_harness_start(&mut self) {
        self.model_catalog = None;
        self.efforts.clear();
    }

    pub(super) fn clear_on_harness_stop(&mut self) {
        self.model_catalog = None;
        self.efforts.clear();
        self.commands = None;
        self.thread_commands.clear();
        self.commands_probed = false;
        self.pending_effort_threads.clear();
        self.probe_wanted = false;
        self.models_refresh_pending = false;
    }

    pub(super) fn on_peer_gone(&mut self, peer_id: u64) {
        for pending in &mut self.pending_set_modes {
            if pending.peer_id == Some(peer_id) {
                pending.peer_id = None;
            }
        }
        self.pending_models.retain(|&id| id != peer_id);
        self.pending_efforts.retain(|(id, _)| *id != peer_id);
        self.pending_set_models.retain_mut(|p| {
            if p.peer_id == Some(peer_id) {
                if p.in_flight_on_session {
                    p.peer_id = None;
                    true
                } else {
                    false
                }
            } else {
                true
            }
        });
    }

    pub(super) fn fail_pending(&mut self, message: &str, replies: &mut Vec<PeerReply>) {
        for peer_id in self.pending_models.drain(..) {
            replies.push(PeerReply {
                peer_id,
                message: error_json("harness-error", message),
                subscribe: None,
            });
        }
        for item in self.pending_set_models.drain(..) {
            if let Some(peer_id) = item.peer_id {
                replies.push(PeerReply {
                    peer_id,
                    message: error_json("harness-error", message),
                    subscribe: None,
                });
            }
        }
        for item in self.pending_set_modes.drain(..) {
            if let Some(peer_id) = item.peer_id {
                replies.push(PeerReply {
                    peer_id,
                    message: error_json("harness-error", message),
                    subscribe: None,
                });
            }
        }
        for (peer_id, _) in self.pending_efforts.drain(..) {
            replies.push(PeerReply {
                peer_id,
                message: error_json("harness-error", message),
                subscribe: None,
            });
        }
    }

    pub(super) fn min_mode_wait(&self, stop_timeout: Duration) -> Option<Duration> {
        self.modes_in_flight
            .iter()
            .map(|(_, asked)| stop_timeout.saturating_sub(asked.elapsed()))
            .min()
    }

    pub(super) fn has_overdue_mode(&self, stop_timeout: Duration) -> bool {
        self.modes_in_flight
            .iter()
            .any(|(_, asked)| asked.elapsed() >= stop_timeout)
    }

    pub(super) fn has_pending_set_model(&self, thread_id: &str) -> bool {
        self.pending_set_models.iter().any(|p| p.thread_id == thread_id)
    }

    pub(super) fn has_mode_in_flight(&self, thread_id: &str) -> bool {
        self.modes_in_flight.iter().any(|(t, _)| t == thread_id)
    }
}

impl Service {
    pub(super) fn default_mode(&self) -> String {
        self.store
            .default_mode()
            .unwrap_or_else(|_| crate::harness::STARTING_MODE.to_string())
    }

    pub(super) fn free_hosted_models(&self) -> bool {
        self.store.free_hosted_models().unwrap_or(false)
    }

    fn provider_models_seen(&self) -> bool {
        self.store.provider_models_seen().unwrap_or(false)
    }

    /// Whether the person has a Model source ready (free hosted Models enabled,
    /// or a harness/provider known to offer Models), without starting an idle
    /// harness.
    pub(super) fn models_chosen(&self) -> bool {
        if self.harness_config.is_none() {
            return false;
        }
        if let Some(catalog) = &self.catalog.model_catalog {
            return !catalog.supported || !catalog.models.is_empty();
        }
        self.free_hosted_models() || self.provider_models_seen()
    }

    pub(super) fn note_provider_models_seen(&mut self, seen: bool, log: &mut Log) {
        if self.free_hosted_models() || self.provider_models_seen() == seen {
            return;
        }
        if let Err(err) = self.store.set_provider_models_seen(seen) {
            store_failed(log, &err);
        }
    }

    pub(super) fn record_catalog_choice(&mut self, catalog: &ModelCatalog, log: &mut Log) {
        if !catalog.supported {
            return;
        }
        self.note_provider_models_seen(!catalog.models.is_empty(), log);
    }

    fn update_model_catalog(&mut self, mut cat: ModelCatalog, update_default: bool) -> bool {
        if self.harness_env_stale {
            return false;
        }
        if !update_default
            && let Some(existing) = &self.catalog.model_catalog
            && existing.default_model.is_some()
        {
            cat.default_model = existing.default_model.clone();
        }
        // A harness that says only what changed, the Model, still offers the
        // modes it offered.
        if cat.modes.is_empty()
            && self.harness.as_ref().is_some_and(|h| h.capabilities().supports_modes)
            && let Some(existing) = &self.catalog.model_catalog
        {
            cat.modes = existing.modes.clone();
        }
        let changed = self.catalog.model_catalog.as_ref() != Some(&cat);
        self.catalog.model_catalog = Some(cat);
        changed
    }

    pub(super) fn apply_model_catalog(
        &mut self,
        cat: ModelCatalog,
        update_default: bool,
        log: &mut Log,
        effects: &mut Effects,
    ) {
        self.record_catalog_choice(&cat, log);
        if self.update_model_catalog(cat, update_default)
            && let Some(current) = &self.catalog.model_catalog
        {
            effects.broadcasts.push(Broadcast {
                thread_id: ACTIVITY.to_string(),
                message: models_event_json(current, self.free_hosted_models(), self.catalog.steering),
            });
        }
    }

    pub(super) fn handle_models(&mut self, peer_id: u64, value: &Value, log: &mut Log) -> RequestOutcome {
        let refresh = match value.get("refresh") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(b)) => *b,
            _ => return error_outcome("invalid-request", "Expected refresh to be a boolean."),
        };
        if self.harness_config.is_none() {
            return error_outcome("no-harness", "No harness is available.");
        }
        if self.harness_env_stale {
            return error_outcome(
                "working",
                "Lever is restarting the harness after the free hosted Models setting changed; try again when active turns finish.",
            );
        }
        let empty_cached = self
            .catalog
            .model_catalog
            .as_ref()
            .is_some_and(|c| c.supported && c.models.is_empty());
        if (refresh || empty_cached) && !self.has_active_work() {
            if self.harness.is_some() {
                self.stop_harness("to refresh Models", log);
            }
            self.catalog.model_catalog = None;
        } else if refresh && self.harness.is_some() {
            self.catalog.models_refresh_pending = true;
        }
        if !refresh
            && !empty_cached
            && let Some(catalog) = &self.catalog.model_catalog
        {
            return RequestOutcome {
                reply: Some(models_reply_json(
                    catalog,
                    self.free_hosted_models(),
                    self.catalog.steering,
                )),
                subscribe: None,
                broadcasts: Vec::new(),
            };
        }
        if self.harness.is_none()
            && let Err(err) = self.start_harness(log)
        {
            return error_outcome("harness-error", &err);
        }
        self.idle_since = None;
        self.catalog.pending_models.push(peer_id);
        if let Some(harness) = self.harness.as_mut()
            && harness.is_initialized()
        {
            harness.probe_models(&default_cwd());
        }
        RequestOutcome {
            reply: None,
            subscribe: None,
            broadcasts: Vec::new(),
        }
    }

    /// Has the harness say what it offers, starting it if need be: its
    /// Models, its modes, and the commands that follow.
    pub(super) fn read_what_the_harness_offers(&mut self, log: &mut Log) {
        if self.harness.is_none() && self.start_harness(log).is_err() {
            return;
        }
        self.idle_since = None;
        match self.harness.as_mut() {
            Some(harness) if harness.is_initialized() => harness.probe_models(&default_cwd()),
            _ => self.catalog.probe_wanted = true,
        }
    }

    pub(super) fn handle_commands(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let thread_id = match value.get("thread") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
            _ => return error_outcome("invalid-request", "Expected thread to be a string or null."),
        };
        if self.harness_config.is_none() {
            return error_outcome("no-harness", "No harness is available.");
        }
        if let Some(tid) = thread_id {
            match self.store.thread_meta(tid) {
                Ok(Some(_)) => {}
                Ok(None) => return error_outcome("no-thread", &format!("There is no thread {tid}.")),
                Err(err) => return error_outcome("storage-error", &err),
            }
        }
        let known = thread_id
            .and_then(|tid| self.catalog.thread_commands.get(tid))
            .or(self.catalog.commands.as_ref());
        let mut reply = json!({
            "reply": "commands",
            "known": known.is_some(),
            "commands": commands_json(known.map(Vec::as_slice).unwrap_or(&[])),
        });
        if let Some(tid) = thread_id {
            reply["thread"] = json!(tid);
        }
        // What the harness offers is not known yet: it is asked once, and the
        // answer comes as a `commands` event to whoever watches.
        let probing = self.catalog.probe_wanted || self.harness.as_ref().is_some_and(|h| h.probing_models());
        if known.is_none() && !self.catalog.commands_probed && !probing && !self.harness_env_stale {
            self.read_what_the_harness_offers(log);
        }
        RequestOutcome {
            reply: Some(reply),
            subscribe: None,
            broadcasts: Vec::new(),
        }
    }

    pub(super) fn handle_efforts(&mut self, peer_id: u64, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(model) = value
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        else {
            return error_outcome("invalid-request", "Expected a non-empty string model field.");
        };
        if self.harness_config.is_none() {
            return error_outcome("no-harness", "No harness is available.");
        }
        if self.harness_env_stale {
            return error_outcome(
                "working",
                "Lever is restarting the harness after the free hosted Models setting changed; try again when active turns finish.",
            );
        }
        if let Some(levels) = self.catalog.efforts.get(model) {
            return RequestOutcome {
                reply: Some(efforts_reply_json(model, levels)),
                subscribe: None,
                broadcasts: Vec::new(),
            };
        }
        if let Some(catalog) = &self.catalog.model_catalog
            && catalog.supported
            && !catalog.models.is_empty()
            && !catalog.models.iter().any(|m| m.id == model)
        {
            return error_outcome("invalid-model", &format!("The harness does not offer Model {model}."));
        }
        if self.harness.is_none()
            && let Err(err) = self.start_harness(log)
        {
            return error_outcome("harness-error", &err);
        }
        self.idle_since = None;
        self.catalog.pending_efforts.push((peer_id, model.to_string()));
        if let Some(harness) = self.harness.as_mut()
            && harness.is_initialized()
        {
            harness.probe_efforts(&default_cwd(), model);
        }
        RequestOutcome {
            reply: None,
            subscribe: None,
            broadcasts: Vec::new(),
        }
    }

    /// Whether `effort` is one a thread on `model` can take: a level of the
    /// Model, where Lever knows the Model's levels. One it does not know is
    /// let through, and the harness decides when the turn sets it.
    pub(super) fn check_effort(&self, model: Option<&str>, effort: &str) -> Result<(), RequestOutcome> {
        let model = model.or_else(|| {
            self.catalog
                .model_catalog
                .as_ref()
                .and_then(|c| c.default_model.as_deref())
        });
        if let Some(levels) = model.and_then(|m| self.catalog.efforts.get(m))
            && !levels.iter().any(|level| level.id == effort)
        {
            return Err(error_outcome(
                "invalid-effort",
                &format!("This thread's Model does not offer the effort {effort}."),
            ));
        }
        Ok(())
    }

    pub(super) fn handle_set_effort(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(thread_id) = value.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "Expected a non-empty string thread field.");
        };
        let effort = match value.get("effort") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.as_str()),
            _ => return error_outcome("invalid-request", "Expected effort to be a non-empty string or null."),
        };
        self.save_endings();
        let model = match self.store.thread_meta(thread_id) {
            Ok(Some(meta)) => {
                if meta.working_turn.is_some() || self.thread_busy(thread_id) {
                    return error_outcome("working", "This thread is already working on a turn.");
                }
                meta.model
            }
            Ok(None) => return error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
            Err(err) => return error_outcome("storage-error", &err),
        };
        if let Some(effort) = effort
            && let Err(outcome) = self.check_effort(model.as_deref(), effort)
        {
            return outcome;
        }
        if let Err(err) = self.store.set_thread_effort(thread_id, effort, unix_now()) {
            store_failed(log, &err);
            return error_outcome("storage-error", &err);
        }
        self.catalog.pending_effort_threads.insert(thread_id.to_string());
        RequestOutcome {
            reply: Some(json!({ "reply": "effort-set", "thread": thread_id, "effort": effort })),
            subscribe: None,
            broadcasts: vec![Broadcast {
                thread_id: thread_id.to_string(),
                message: json!({ "event": "thread-updated", "thread": thread_id, "effort": effort }),
            }],
        }
    }

    /// The event that tells who watches a thread its Model, when the record
    /// has another. Asked before the Model is saved.
    pub(super) fn model_changes(&self, thread_id: &str, model: &str) -> Option<Broadcast> {
        match self.store.thread_meta(thread_id) {
            Ok(Some(meta)) if meta.model.as_deref() != Some(model) => Some(Broadcast {
                thread_id: thread_id.to_string(),
                message: json!({ "event": "thread-updated", "thread": thread_id, "model": model }),
            }),
            _ => None,
        }
    }

    /// Saves a thread's effort where the record has another, and tells who
    /// watches the thread.
    pub(super) fn save_effort(&mut self, thread_id: &str, effort: Option<&str>, log: &mut Log) -> Vec<Broadcast> {
        match self.store.thread_meta(thread_id) {
            Ok(Some(meta)) if meta.effort.as_deref() != effort => {}
            _ => return Vec::new(),
        }
        if let Err(err) = self.store.set_thread_effort(thread_id, effort, unix_now()) {
            self.could_not_save(thread_id, &err, log);
            return Vec::new();
        }
        vec![Broadcast {
            thread_id: thread_id.to_string(),
            message: json!({ "event": "thread-updated", "thread": thread_id, "effort": effort }),
        }]
    }

    /// Keeps the effort levels a thread's session offers for its Model, and
    /// returns the session's effort selector.
    pub(super) fn note_session_efforts(&mut self, thread_id: &str) -> Option<Option<EffortSelector>> {
        let harness = self.harness.as_ref()?;
        let session_id = harness.session_for_thread(thread_id)?;
        let selector = harness.session_effort(session_id).cloned();
        if let Some(model) = harness.session_current_model(session_id) {
            self.catalog.efforts.insert(
                model.to_string(),
                selector.as_ref().map(|s| s.options.clone()).unwrap_or_default(),
            );
        }
        Some(selector)
    }

    /// Keeps what the harness says of a thread's session: the effort levels
    /// of its Model, and the effort it is at. That effort is the thread's,
    /// unless the turn that waits has one of its own the Model offers, which
    /// it sets before its prompt.
    pub(super) fn mirror_effort(&mut self, thread_id: &str, log: &mut Log) -> Vec<Broadcast> {
        let Some(selector) = self.note_session_efforts(thread_id) else {
            return Vec::new();
        };
        let wanted = self
            .turns
            .working_turns
            .get(thread_id)
            .filter(|active| !matches!(active.dispatch, TurnDispatch::PromptInFlight { .. }))
            .and_then(|active| active.desired_effort.clone());
        let effort = match (&selector, wanted) {
            (Some(selector), Some(wanted)) if selector.offers(&wanted) => Some(wanted),
            (Some(selector), _) => Some(selector.current.clone()),
            (None, _) => None,
        };
        self.save_effort(thread_id, effort.as_deref(), log)
    }

    pub(super) fn handle_set_free_models(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(enabled) = value.get("enabled").and_then(Value::as_bool) else {
            return error_outcome("invalid-request", "Expected a boolean enabled field.");
        };
        let was_enabled = self.free_hosted_models();
        if let Err(err) = self.store.set_free_hosted_models(enabled) {
            store_failed(log, &err);
            return error_outcome("storage-error", &err);
        }
        if enabled != was_enabled {
            self.catalog.model_catalog = None;
            self.catalog.efforts.clear();
            if self.harness.is_some() {
                if !self.has_active_work() {
                    self.stop_harness("after free hosted Models setting changed", log);
                } else {
                    self.harness_env_stale = true;
                }
            }
        }
        let models_chosen = self.models_chosen();
        let broadcasts = if enabled != was_enabled {
            vec![Broadcast {
                thread_id: ACTIVITY.to_string(),
                message: json!({
                    "event": "free-models",
                    "enabled": enabled,
                    "models_chosen": models_chosen,
                }),
            }]
        } else {
            Vec::new()
        };
        RequestOutcome {
            reply: Some(json!({
                "reply": "free-models",
                "enabled": enabled,
                "models_chosen": models_chosen,
            })),
            subscribe: None,
            broadcasts,
        }
    }

    pub(super) fn handle_set_model(&mut self, peer_id: u64, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(thread_id) = value.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "Expected a non-empty string thread field.");
        };
        let Some(model) = value
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        else {
            return error_outcome("invalid-request", "Expected a non-empty string model field.");
        };
        if self.harness_config.is_none() {
            return error_outcome("no-harness", "No harness is available.");
        }
        if self.harness_env_stale {
            return error_outcome(
                "working",
                "Lever is restarting the harness after the free hosted Models setting changed; try again when active turns finish.",
            );
        }
        match self.store.thread_meta(thread_id) {
            Ok(Some(meta)) => {
                if meta.working_turn.is_some() || self.thread_busy(thread_id) {
                    return error_outcome("working", "This thread is already working on a turn.");
                }
            }
            Ok(None) => return error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
            Err(err) => return error_outcome("storage-error", &err),
        }

        if !self.turns.pending_session_resets.contains_key(thread_id)
            && let Some(harness) = self.harness.as_mut()
            && harness.is_initialized()
            && let Some(session_id) = harness.session_for_thread(thread_id).map(str::to_string)
        {
            if let Some(catalog) = &self.catalog.model_catalog {
                if !catalog.supported {
                    return error_outcome("unsupported", "This harness does not offer Model selection.");
                }
                if catalog.models.is_empty() {
                    return error_outcome("no-model", NO_MODEL_MESSAGE);
                }
                if !catalog.models.iter().any(|m| m.id == model) {
                    return error_outcome("invalid-model", &format!("The harness does not offer Model {model}."));
                }
            }
            self.idle_since = None;
            harness.set_session_model(thread_id, None, &session_id, model);
            self.catalog.pending_set_models.push(PendingSetModel {
                peer_id: Some(peer_id),
                thread_id: thread_id.to_string(),
                model: model.to_string(),
                in_flight_on_session: true,
            });
            return RequestOutcome {
                reply: None,
                subscribe: None,
                broadcasts: Vec::new(),
            };
        }

        if let Some(catalog) = self.catalog.model_catalog.clone()
            && !(catalog.supported && catalog.models.is_empty())
        {
            return self.finish_set_model_with_catalog(thread_id, model, &catalog, log);
        }

        if self.harness.is_none()
            && let Err(err) = self.start_harness(log)
        {
            return error_outcome("harness-error", &err);
        }
        self.idle_since = None;
        self.catalog.pending_set_models.push(PendingSetModel {
            peer_id: Some(peer_id),
            thread_id: thread_id.to_string(),
            model: model.to_string(),
            in_flight_on_session: false,
        });
        if let Some(harness) = self.harness.as_mut()
            && harness.is_initialized()
        {
            harness.probe_models(&default_cwd());
        }
        RequestOutcome {
            reply: None,
            subscribe: None,
            broadcasts: Vec::new(),
        }
    }

    pub(super) fn finish_set_model_with_catalog(
        &mut self,
        thread_id: &str,
        model: &str,
        catalog: &ModelCatalog,
        log: &mut Log,
    ) -> RequestOutcome {
        if !catalog.supported {
            return error_outcome("unsupported", "This harness does not offer Model selection.");
        }
        if catalog.models.is_empty() {
            return error_outcome("no-model", NO_MODEL_MESSAGE);
        }
        if !catalog.models.iter().any(|m| m.id == model) {
            return error_outcome("invalid-model", &format!("The harness does not offer Model {model}."));
        }
        let mut broadcasts: Vec<Broadcast> = self.model_changes(thread_id, model).into_iter().collect();
        if let Err(err) = self.store.set_thread_model(thread_id, model, unix_now()) {
            store_failed(log, &err);
            return error_outcome("storage-error", &err);
        }
        self.flush_pending_session_reset(thread_id);
        // An effort is a level of one Model: another Model begins at its own.
        if !broadcasts.is_empty() {
            self.catalog.pending_effort_threads.remove(thread_id);
            broadcasts.extend(self.save_effort(thread_id, None, log));
        }
        RequestOutcome {
            reply: Some(json!({
                "reply": "model-set",
                "thread": thread_id,
                "model": model,
            })),
            subscribe: None,
            broadcasts,
        }
    }

    /// Whether `mode` is one Lever offers: one of its own, and, once Lever
    /// knows which the harness reports, one of those.
    pub(super) fn check_mode(&self, mode: &str) -> Result<(), RequestOutcome> {
        if !harness::is_mode(mode) {
            return Err(error_outcome("invalid-mode", &format!("There is no mode {mode}.")));
        }
        if let Some(catalog) = &self.catalog.model_catalog
            && !catalog.modes.is_empty()
            && !catalog.modes.iter().any(|m| m == mode)
        {
            return Err(error_outcome(
                "invalid-mode",
                &format!("The harness does not offer the mode {mode}."),
            ));
        }
        Ok(())
    }

    pub(super) fn handle_set_mode(&mut self, peer_id: u64, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(thread_id) = value.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "Expected a non-empty string thread field.");
        };
        let Some(mode) = value.get("mode").and_then(Value::as_str) else {
            return error_outcome("invalid-request", "Expected a string mode field.");
        };
        if let Err(outcome) = self.check_mode(mode) {
            return outcome;
        }
        self.save_endings();
        let previous = match self.store.thread_meta(thread_id) {
            Ok(Some(meta)) => {
                if meta.working_turn.is_some() || self.thread_busy(thread_id) {
                    return error_outcome("working", "This thread is already working on a turn.");
                }
                meta.mode
            }
            Ok(None) => return error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
            Err(err) => return error_outcome("storage-error", &err),
        };
        // A thread whose session is live may have work going on in it, a
        // subagent's or a turn the harness begins: the session takes the mode
        // first, and the thread has it once the session does. If the thread
        // moved to a new project, detach the retired session first so the mode
        // is saved for the fresh session instead.
        self.flush_pending_session_reset(thread_id);
        if let Some(harness) = self.harness.as_mut()
            && harness.capabilities().supports_modes
            && let Some(session_id) = harness.session_for_thread(thread_id).map(str::to_string)
            && harness.session_current_mode(&session_id) != Some(mode)
        {
            harness.set_session_mode(thread_id, None, &session_id, mode);
            self.catalog
                .modes_in_flight
                .push((thread_id.to_string(), Instant::now()));
            self.catalog.pending_set_modes.push(PendingSetMode {
                peer_id: Some(peer_id),
                thread_id: thread_id.to_string(),
                mode: mode.to_string(),
                previous,
                reverting: false,
            });
            return RequestOutcome {
                reply: None,
                subscribe: None,
                broadcasts: Vec::new(),
            };
        }
        self.save_mode(thread_id, mode, log)
    }

    /// Ends a mode change the person asked between turns, once the session
    /// has taken it, or with `failed` when it has not.
    pub(super) fn finish_set_mode(&mut self, thread_id: &str, failed: Option<&str>, log: &mut Log) -> Effects {
        let Some(pos) = self
            .catalog
            .pending_set_modes
            .iter()
            .position(|p| p.thread_id == thread_id)
        else {
            return Effects::default();
        };
        let item = self.catalog.pending_set_modes.remove(pos);
        if item.reverting {
            // The session is in a mode the thread's record does not have:
            // with nothing else to go on, the harness is stopped.
            return match failed {
                Some(_) => self.kill_harness("it did not take back a mode Lever could not save", log),
                None => Effects::default(),
            };
        }
        let outcome = match failed {
            Some(message) => error_outcome("harness-error", message),
            None => self.save_mode(thread_id, &item.mode, log),
        };
        // The session took a mode the record did not: it goes back to the
        // thread's, so nothing goes on in it in a mode the person was told
        // it is not in.
        let unsaved = outcome
            .reply
            .as_ref()
            .is_some_and(|reply| reply["code"] == "storage-error");
        if unsaved
            && let Some(harness) = self.harness.as_mut()
            && let Some(session_id) = harness.session_for_thread(thread_id).map(str::to_string)
        {
            harness.set_session_mode(thread_id, None, &session_id, &item.previous);
            self.catalog
                .modes_in_flight
                .push((thread_id.to_string(), Instant::now()));
            self.catalog.pending_set_modes.push(PendingSetMode {
                peer_id: None,
                thread_id: thread_id.to_string(),
                mode: item.previous.clone(),
                previous: item.previous.clone(),
                reverting: true,
            });
        }
        Effects {
            broadcasts: outcome.broadcasts,
            replies: item
                .peer_id
                .zip(outcome.reply)
                .map(|(peer_id, message)| PeerReply {
                    peer_id,
                    message,
                    subscribe: None,
                })
                .into_iter()
                .collect(),
        }
    }

    /// Makes `mode` the thread's in the record, and tells who watches it.
    fn save_mode(&mut self, thread_id: &str, mode: &str, log: &mut Log) -> RequestOutcome {
        if let Err(err) = self.store.set_thread_mode(thread_id, mode, unix_now()) {
            store_failed(log, &err);
            return error_outcome("storage-error", &err);
        }
        RequestOutcome {
            reply: Some(json!({ "reply": "mode-set", "thread": thread_id, "mode": mode })),
            subscribe: None,
            broadcasts: vec![Broadcast {
                thread_id: thread_id.to_string(),
                message: json!({ "event": "thread-updated", "thread": thread_id, "mode": mode }),
            }],
        }
    }

    pub(super) fn handle_set_default_mode(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(mode) = value.get("mode").and_then(Value::as_str) else {
            return error_outcome("invalid-request", "Expected a string mode field.");
        };
        if !STARTING_MODES.contains(&mode) {
            return error_outcome("invalid-mode", "New threads start in ask-first or full-access.");
        }
        if let Err(outcome) = self.check_mode(mode) {
            return outcome;
        }
        if let Err(err) = self.store.set_default_mode(mode) {
            store_failed(log, &err);
            return error_outcome("storage-error", &err);
        }
        RequestOutcome {
            reply: Some(json!({ "reply": "default-mode", "mode": mode })),
            subscribe: None,
            broadcasts: Vec::new(),
        }
    }
}
