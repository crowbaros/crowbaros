//! Lever's core service controller: owns the SQLite thread store, the on-demand
//! ACP harness lifecycle (including the idle timer), active turns across
//! concurrent threads, and Lever's wire messages.

use crate::acp::{self, AcpEvent, AcpHarness, HarnessConfig, ModelCatalog, TurnOutcome};
use crate::channel::LEVER_VERSION;
use crate::fulcrum;
use crate::log::{Event, Log};
use crate::store::{
    ActPatch, ActRecord, NewRequest, NewThread, NoteRecord, RequestRecord, Store, ThreadRecord, ThreadSummary,
    TurnRecord,
};
use crate::sys;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Default time an idle harness stays running after the last working turn ends.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Default time a harness has to end a turn after Lever asks it to stop.
pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// The key peers that sent `watch` are subscribed under. No thread has it.
const ACTIVITY: &str = "*activity";
/// The modes a new thread can start in.
const STARTING_MODES: [&str; 2] = ["ask-first", "full-access"];

/// How long Lever waits before it tries again to save how a turn ended.
const UNSAVED_RETRY: Duration = Duration::from_millis(250);
/// What a turn failed with when the harness finished it and Lever's database
/// did not take all of it.
const UNSAVED_MESSAGE: &str = "Lever could not save all of this turn.";
/// What a turn failed with when the database did not take the session the
/// harness gave its thread.
const UNSAVED_SESSION_MESSAGE: &str = "Lever could not save this thread's session.";
/// What Lever says when free hosted Models are off and no provider is signed in.
const NO_MODEL_MESSAGE: &str = "No Model is available. Run `crowbar-agent free-models on` or `crowbar-agent login`.";
/// The note a thread gets when its harness session could not be picked up
/// again, and what Lever keeps as that note's source.
const NEW_SESSION_NOTE: &str = "The harness could not pick this thread's session up again. It began a new one, which has none of the turns before.";
const NEW_SESSION_SOURCE: &str = r#"{"lever":"new-session"}"#;

/// One broadcast event targeted at peers subscribed to `thread_id`.
pub struct Broadcast {
    pub thread_id: String,
    pub message: Value,
}

/// One deferred reply targeted at a specific peer connection.
pub struct PeerReply {
    pub peer_id: u64,
    pub message: Value,
    pub subscribe: Option<String>,
}

/// Outbound messages produced by harness activity or timers.
#[derive(Default)]
pub struct Effects {
    pub broadcasts: Vec<Broadcast>,
    pub replies: Vec<PeerReply>,
}

impl Effects {
    fn extend(&mut self, other: Effects) {
        self.broadcasts.extend(other.broadcasts);
        self.replies.extend(other.replies);
    }
}

/// Result of handling one request from a peer.
pub struct RequestOutcome {
    /// Immediate reply, or `None` when the reply waits on the harness.
    pub reply: Option<Value>,
    /// Thread ID to subscribe the requesting peer to, if any.
    pub subscribe: Option<String>,
    /// Broadcasts to emit to subscribed peers after the reply is queued.
    pub broadcasts: Vec<Broadcast>,
}

enum TurnDispatch {
    WaitingForInit,
    WaitingForSession,
    WaitingForModel,
    WaitingForMode,
    PromptInFlight { session_id: String },
}

struct WorkingTurn {
    turn: u64,
    message: String,
    context_prompt: Option<String>,
    cwd: String,
    existing_session_id: Option<String>,
    desired_model: Option<String>,
    /// The thread's mode, which the session is set to before the prompt.
    mode: String,
    dispatch: TurnDispatch,
    /// Something of this turn did not reach the database.
    unsaved: bool,
    /// When the person asked to stop this turn and the harness was told.
    stop_asked: Option<Instant>,
}

/// A request that waits for the person, as Lever holds it: the harness's
/// handle for answering it, and its thread.
struct Waiting {
    handle: u64,
    thread_id: String,
}

struct PendingSetModel {
    /// Peer waiting for the reply, or `None` if that peer disconnected while
    /// `session/set_config_option` was already in flight on the harness.
    peer_id: Option<u64>,
    thread_id: String,
    model: String,
    /// True when `session/set_config_option` is already in flight on an active
    /// session; false when waiting on the harness's Model catalogue probe.
    in_flight_on_session: bool,
}

/// How a turn ended, kept until the database has it.
struct Ending {
    thread_id: String,
    turn: u64,
    state: &'static str,
    error: Option<String>,
}

pub struct Service {
    store: Store,
    harness_config: Option<HarnessConfig>,
    fulcrum_socket: Option<PathBuf>,
    harness: Option<AcpHarness>,
    model_catalog: Option<ModelCatalog>,
    /// True when `free_hosted_models` changed while the harness was busy, so
    /// it should be stopped as soon as all active work finishes.
    harness_env_stale: bool,
    idle_timeout: Duration,
    stop_timeout: Duration,
    idle_since: Option<Instant>,
    working_turns: HashMap<String, WorkingTurn>,
    /// Requests that wait, by Lever's request ID.
    waiting: HashMap<String, Waiting>,
    /// What `activity` last said: threads working, and threads waiting.
    activity: (Vec<String>, Vec<String>),
    pending_models: Vec<u64>,
    pending_set_models: Vec<PendingSetModel>,
    /// Ends the database did not take, tried again on each wake.
    unsaved_endings: Vec<Ending>,
}

impl Service {
    pub fn new(
        store: Store,
        harness_config: Option<HarnessConfig>,
        fulcrum_socket: Option<PathBuf>,
        idle_timeout: Duration,
        stop_timeout: Duration,
    ) -> Self {
        Self {
            store,
            harness_config,
            fulcrum_socket,
            harness: None,
            model_catalog: None,
            harness_env_stale: false,
            idle_timeout,
            stop_timeout,
            idle_since: None,
            working_turns: HashMap::new(),
            waiting: HashMap::new(),
            activity: (Vec::new(), Vec::new()),
            pending_models: Vec::new(),
            pending_set_models: Vec::new(),
            unsaved_endings: Vec::new(),
        }
    }

    pub fn harness_stdout_fd(&self) -> Option<RawFd> {
        self.harness.as_ref().map(AcpHarness::stdout_fd)
    }

    pub fn harness_stdin_fd(&self) -> Option<RawFd> {
        self.harness
            .as_ref()
            .filter(|h| h.wants_write())
            .and_then(AcpHarness::stdin_fd)
    }

    fn has_active_work(&self) -> bool {
        !self.working_turns.is_empty()
            || !self.waiting.is_empty()
            || !self.pending_models.is_empty()
            || !self.pending_set_models.is_empty()
    }

    fn default_mode(&self) -> String {
        self.store
            .default_mode()
            .unwrap_or_else(|_| crate::store::STARTING_MODE.to_string())
    }

    /// Threads working, and threads with a request that waits, each sorted.
    fn current_activity(&self) -> (Vec<String>, Vec<String>) {
        let mut working: Vec<String> = self.working_turns.keys().cloned().collect();
        working.sort();
        let mut waiting: Vec<String> = self.waiting.values().map(|w| w.thread_id.clone()).collect();
        waiting.sort();
        waiting.dedup();
        (working, waiting)
    }

    /// An `activity` event for peers that watch, when what is working or
    /// waiting changed since it was last said.
    fn activity_changed(&mut self) -> Option<Broadcast> {
        let now = self.current_activity();
        if now == self.activity {
            return None;
        }
        self.activity = now;
        Some(Broadcast {
            thread_id: ACTIVITY.to_string(),
            message: json!({
                "event": "activity",
                "working": self.activity.0,
                "waiting": self.activity.1,
            }),
        })
    }

    fn free_hosted_models(&self) -> bool {
        self.store.free_hosted_models().unwrap_or(false)
    }

    fn update_model_catalog(&mut self, mut cat: ModelCatalog, update_default: bool) {
        if self.harness_env_stale {
            return;
        }
        if !update_default
            && let Some(existing) = &self.model_catalog
            && existing.default_model.is_some()
        {
            cat.default_model = existing.default_model.clone();
        }
        self.model_catalog = Some(cat);
    }

    /// Timeout in milliseconds for `poll(2)`, or `-1` when Lever waits for
    /// nothing but its peers and its harness.
    pub fn poll_timeout_ms(&self) -> libc::c_int {
        let retry = (!self.unsaved_endings.is_empty()).then_some(UNSAVED_RETRY);
        let stop = self
            .working_turns
            .values()
            .filter_map(|t| t.stop_asked)
            .map(|asked| self.stop_timeout.saturating_sub(asked.elapsed()))
            .min();
        match [self.idle_remaining(), retry, stop].into_iter().flatten().min() {
            None => -1,
            // Rounded up: the last part of a millisecond is waited for, not
            // spun through.
            Some(wait) => wait.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as libc::c_int,
        }
    }

    /// How long the harness may still sit idle, when it is idle.
    fn idle_remaining(&self) -> Option<Duration> {
        let since = self.idle_since?;
        if self.harness.is_none() || self.has_active_work() {
            return None;
        }
        Some(self.idle_timeout.saturating_sub(since.elapsed()))
    }

    /// What Lever does each time it wakes: saves the ends of turns the
    /// database did not take before, carries out a Stop the harness did not,
    /// stops a harness whose environment changed once it is idle, and stops
    /// the harness if no thread has worked for `idle_timeout`.
    pub fn on_wake(&mut self, log: &mut Log) -> Effects {
        self.save_endings();
        let mut effects = Effects::default();
        let overdue = self
            .working_turns
            .iter()
            .find(|(_, t)| t.stop_asked.is_some_and(|asked| asked.elapsed() >= self.stop_timeout))
            .map(|(tid, t)| (tid.clone(), t.turn));
        if let Some((thread_id, turn)) = overdue {
            effects.extend(self.force_stop(&thread_id, turn, log));
        }
        if self.harness_env_stale && !self.has_active_work() {
            self.stop_harness("after free hosted Models setting changed", log);
        } else if self.idle_remaining().is_some_and(|left| left.is_zero()) {
            self.stop_harness("after idle timeout", log);
        }
        effects.broadcasts.extend(self.activity_changed());
        effects
    }

    /// A harness did not end a turn it was asked to stop. The turn ends as
    /// stopped, and the harness ends with every process under it, so nothing
    /// the turn started goes on. Every other turn it was working on is
    /// interrupted.
    fn force_stop(&mut self, thread_id: &str, turn: u64, log: &mut Log) -> Effects {
        self.working_turns.remove(thread_id);
        self.end_turn(thread_id, turn, "stopped", None, log);
        let mut effects = Effects::default();
        effects.broadcasts.push(Broadcast {
            thread_id: thread_id.to_string(),
            message: json!({ "event": "stopped", "thread": thread_id, "turn": turn }),
        });
        if let Some(harness) = self.harness.take() {
            let pid = harness.pid();
            harness.kill_tree();
            let name = self
                .harness_config
                .as_ref()
                .map(|c| c.name.as_str())
                .unwrap_or("harness");
            let _ = log.write(
                Event::Harness,
                &format!(
                    "stopped {name} (process {pid}) and every process under it: it did not stop a turn when asked"
                ),
            );
        }
        effects.extend(self.after_harness_gone("The harness was stopped.", log));
        effects
    }

    /// Drops any deferred replies waiting for a peer that has disconnected.
    /// An in-flight `session/set_config_option` RPC keeps its thread locked
    /// until the harness answers, with `peer_id` cleared so no reply is sent.
    pub fn on_peer_gone(&mut self, peer_id: u64) {
        self.pending_models.retain(|&id| id != peer_id);
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
        self.refresh_idle_timer();
    }

    pub fn handle_request(&mut self, peer_id: u64, is_person: bool, value: &Value, log: &mut Log) -> RequestOutcome {
        let mut outcome = self.handle_op(peer_id, is_person, value, log);
        outcome.broadcasts.extend(self.activity_changed());
        outcome
    }

    fn handle_op(&mut self, peer_id: u64, is_person: bool, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(op) = value.as_object().and_then(|o| o.get("op")).and_then(Value::as_str) else {
            return error_outcome("invalid-request", "Expected a JSON object with a string op field.");
        };

        match op {
            "answer" => self.handle_answer(value, log),
            "set-mode" => self.handle_set_mode(value, log),
            "set-default-mode" => self.handle_set_default_mode(value, log),
            "watch" => {
                let (working, waiting) = self.current_activity();
                RequestOutcome {
                    reply: Some(json!({ "reply": "activity", "working": working, "waiting": waiting })),
                    subscribe: Some(ACTIVITY.to_string()),
                    broadcasts: Vec::new(),
                }
            }
            "status" => self.handle_status(),
            "list" => self.handle_list(),
            "send" => self.handle_send(is_person, value, log),
            "resume" => self.handle_resume(value),
            "stop" => self.handle_stop(value, log),
            "models" => self.handle_models(peer_id, value, log),
            "set-free-models" => {
                if !is_person {
                    return error_outcome("unsupported", "Only the person's socket can change free hosted Models.");
                }
                self.handle_set_free_models(value, log)
            }
            "set-model" => self.handle_set_model(peer_id, value, log),
            _ => error_outcome("unsupported", "This request is not supported."),
        }
    }

    pub fn on_harness_writable(&mut self, log: &mut Log) -> Effects {
        let ok = self.harness.as_mut().is_none_or(AcpHarness::on_writable);
        if !ok {
            let mut effects = self.on_harness_exited(log);
            effects.broadcasts.extend(self.activity_changed());
            return effects;
        }
        Effects::default()
    }

    pub fn on_harness_readable(&mut self, log: &mut Log) -> Effects {
        let events = match self.harness.as_mut() {
            Some(h) => h.on_readable(),
            None => return Effects::default(),
        };
        let mut effects = Effects::default();
        for ev in events {
            match ev {
                AcpEvent::Exited => {
                    effects.extend(self.on_harness_exited(log));
                    break;
                }
                other => effects.extend(self.apply_acp_event(other, log)),
            }
        }
        if self.harness_env_stale && !self.has_active_work() {
            self.stop_harness("after free hosted Models setting changed", log);
        }
        self.refresh_idle_timer();
        effects.broadcasts.extend(self.activity_changed());
        effects
    }

    pub fn shutdown(mut self, log: &mut Log) -> Result<(), String> {
        if self.harness.is_some() {
            self.stop_harness("on Lever stop", log);
        }
        // What was working ends here, and the record says so now, not at the
        // next start.
        if let Err(err) = self.store.interrupt_all_working(unix_now()) {
            store_failed(log, &err);
        }
        if let Err(err) = self.store.withdraw_all_waiting() {
            store_failed(log, &err);
        }
        self.save_endings();
        self.store.close()
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
                "default_mode": self.default_mode(),
            })),
            subscribe: None,
            broadcasts: Vec::new(),
        }
    }

    fn handle_list(&self) -> RequestOutcome {
        match self.store.list_threads() {
            Ok(mut threads) => {
                // A turn that has ended is said to have ended, though the
                // database has yet to take its end. A thread's turns are
                // numbered from one, so its newest is its count of them.
                for thread in &mut threads {
                    if let Some(ending) = self
                        .unsaved_endings
                        .iter()
                        .rfind(|ending| ending.thread_id == thread.id && ending.turn == thread.turns)
                    {
                        thread.state = ending.state.to_string();
                        thread.working = false;
                    }
                }
                RequestOutcome {
                    reply: Some(json!({
                        "reply": "list",
                        "threads": threads.iter().map(thread_summary_json).collect::<Vec<_>>(),
                    })),
                    subscribe: None,
                    broadcasts: Vec::new(),
                }
            }
            Err(err) => error_outcome("storage-error", &err),
        }
    }

    fn handle_resume(&self, value: &Value) -> RequestOutcome {
        let Some(thread_id) = value.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "Expected a non-empty string thread field.");
        };
        match self.store.get_thread(thread_id) {
            Ok(Some(mut record)) => {
                // As in `list`: whoever resumes the thread now was not
                // watching when its turn ended, and no event will say so
                // again.
                for ending in self.unsaved_endings.iter().filter(|e| e.thread_id == thread_id) {
                    if let Some(turn) = record.turns.iter_mut().find(|turn| turn.turn == ending.turn) {
                        turn.state = ending.state.to_string();
                        turn.error = ending.error.clone();
                    }
                }
                record.working = record.turns.last().is_some_and(|turn| turn.state == "working");
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

    fn handle_models(&mut self, peer_id: u64, value: &Value, log: &mut Log) -> RequestOutcome {
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
            .model_catalog
            .as_ref()
            .is_some_and(|c| c.supported && c.models.is_empty());
        if (refresh || empty_cached) && !self.has_active_work() {
            if self.harness.is_some() {
                self.stop_harness("to refresh Models", log);
            }
            self.model_catalog = None;
        }
        if !refresh
            && !empty_cached
            && let Some(catalog) = &self.model_catalog
        {
            return RequestOutcome {
                reply: Some(models_reply_json(catalog, self.free_hosted_models())),
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
        self.pending_models.push(peer_id);
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

    fn handle_set_free_models(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(enabled) = value.get("enabled").and_then(Value::as_bool) else {
            return error_outcome("invalid-request", "Expected a boolean enabled field.");
        };
        let was_enabled = self.free_hosted_models();
        if let Err(err) = self.store.set_free_hosted_models(enabled) {
            store_failed(log, &err);
            return error_outcome("storage-error", &err);
        }
        if enabled != was_enabled {
            self.model_catalog = None;
            if self.harness.is_some() {
                if !self.has_active_work() {
                    self.stop_harness("after free hosted Models setting changed", log);
                } else {
                    self.harness_env_stale = true;
                }
            }
        }
        RequestOutcome {
            reply: Some(json!({
                "reply": "free-models",
                "enabled": enabled,
            })),
            subscribe: None,
            broadcasts: Vec::new(),
        }
    }

    fn handle_set_model(&mut self, peer_id: u64, value: &Value, log: &mut Log) -> RequestOutcome {
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
                if meta.working_turn.is_some()
                    || self.working_turns.contains_key(thread_id)
                    || self.pending_set_models.iter().any(|p| p.thread_id == thread_id)
                {
                    return error_outcome("working", "This thread is already working on a turn.");
                }
            }
            Ok(None) => return error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
            Err(err) => return error_outcome("storage-error", &err),
        }

        if let Some(harness) = self.harness.as_mut()
            && harness.is_initialized()
            && let Some(session_id) = harness.session_for_thread(thread_id).map(str::to_string)
        {
            if let Some(catalog) = &self.model_catalog {
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
            self.pending_set_models.push(PendingSetModel {
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

        if let Some(catalog) = self.model_catalog.clone()
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
        self.pending_set_models.push(PendingSetModel {
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

    fn finish_set_model_with_catalog(
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
        if let Err(err) = self.store.set_thread_model(thread_id, model, unix_now()) {
            store_failed(log, &err);
            return error_outcome("storage-error", &err);
        }
        RequestOutcome {
            reply: Some(json!({
                "reply": "model-set",
                "thread": thread_id,
                "model": model,
            })),
            subscribe: None,
            broadcasts: Vec::new(),
        }
    }

    fn handle_send(&mut self, is_person: bool, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(text) = value
            .get("text")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
        else {
            return error_outcome("invalid-request", "Expected a non-empty string text field.");
        };
        let requested_thread = match value.get("thread") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
            _ => return error_outcome("invalid-request", "Expected thread to be a string or null."),
        };
        let requested_cwd = match value.get("cwd") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if Path::new(s).is_absolute() => Some(s.as_str()),
            _ => return error_outcome("invalid-request", "Expected cwd to be an absolute path."),
        };
        let requested_model = match value.get("model") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.as_str()),
            _ => return error_outcome("invalid-request", "Expected model to be a non-empty string or null."),
        };
        let requested_mode = match value.get("mode") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => match self.check_mode(s) {
                Ok(()) => Some(s.as_str()),
                Err(outcome) => return outcome,
            },
            _ => return error_outcome("invalid-request", "Expected mode to be a string or null."),
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
        if let Some(tid) = requested_thread
            && self.unsaved_endings.iter().any(|ending| ending.thread_id == tid)
        {
            return error_outcome(
                "storage-error",
                "Lever is still saving how this thread's last turn ended.",
            );
        }

        let now = unix_now();
        let title = initial_title(text);
        let (thread_id, cwd, existing_session_id, desired_model, mode, new_thread) = if let Some(tid) = requested_thread
        {
            match self.store.thread_meta(tid) {
                Ok(Some(meta)) => {
                    if meta.working_turn.is_some()
                        || self.working_turns.contains_key(tid)
                        || self.pending_set_models.iter().any(|p| p.thread_id == tid)
                    {
                        return error_outcome("working", "This thread is already working on a turn.");
                    }
                    let model = requested_model
                        .map(str::to_string)
                        .or(meta.model)
                        .or_else(|| self.model_catalog.as_ref().and_then(|c| c.default_model.clone()));
                    let mode = requested_mode.map(str::to_string).unwrap_or(meta.mode);
                    (tid.to_string(), meta.cwd, meta.harness_session_id, model, mode, false)
                }
                Ok(None) => return error_outcome("no-thread", &format!("There is no thread {tid}.")),
                Err(err) => return error_outcome("storage-error", &err),
            }
        } else {
            let tid = format!("t-{}", sys::random_hex(6));
            let cwd = requested_cwd.map(str::to_string).unwrap_or_else(default_cwd);
            let mode = requested_mode
                .map(str::to_string)
                .unwrap_or_else(|| self.default_mode());
            (tid, cwd, None, requested_model.map(str::to_string), mode, true)
        };

        // Messages from the Agent's surface carry desktop context fetched from
        // Fulcrum (active window title/app_id, windows by workspace, displays —
        // never window contents or clipboard). With Fulcrum away, none is sent.
        let desktop_context = if !is_person {
            self.fulcrum_socket
                .as_deref()
                .and_then(|path| fulcrum::fetch_snapshot(path, fulcrum::SNAPSHOT_TIMEOUT))
        } else {
            None
        };
        let context_value = desktop_context.as_ref().and_then(|ctx| serde_json::to_value(ctx).ok());
        let context_json = context_value.as_ref().map(Value::to_string);
        let context_prompt = desktop_context.as_ref().map(fulcrum::format_desktop_context);

        // A new thread's requested Model is persisted only after the harness
        // accepts it (in `SessionReady` or `ModelSet`), so a rejected `--model`
        // does not stick on later turns.
        let thread = new_thread.then_some(NewThread {
            title: &title,
            cwd: &cwd,
            harness_name: Some(harness_name.as_str()),
            mode: &mode,
        });
        let turn = match self.store.begin_turn(
            &thread_id,
            thread,
            text,
            context_json.as_deref(),
            requested_mode.filter(|_| !new_thread),
            now,
        ) {
            Ok(turn) => turn,
            Err(err) => {
                store_failed(log, &err);
                return error_outcome("storage-error", &err);
            }
        };

        let mut msg_event = json!({
            "event": "message",
            "thread": thread_id,
            "turn": turn,
            "text": text,
        });
        if let Some(ctx) = context_value {
            msg_event["context"] = ctx;
        }
        let mut broadcasts = vec![Broadcast {
            thread_id: thread_id.clone(),
            message: msg_event,
        }];

        self.idle_since = None;
        self.working_turns.insert(
            thread_id.clone(),
            WorkingTurn {
                turn,
                message: text.to_string(),
                context_prompt,
                cwd: cwd.clone(),
                existing_session_id: existing_session_id.clone(),
                desired_model,
                mode,
                dispatch: TurnDispatch::WaitingForInit,
                unsaved: false,
                stop_asked: None,
            },
        );

        // Ensure the harness child is running, then dispatch session/prompt.
        if self.harness.is_none()
            && let Err(err) = self.start_harness(log)
        {
            broadcasts.extend(self.fail_turn(&thread_id, turn, &err, log));
            return RequestOutcome {
                reply: Some(json!({ "reply": "sent", "thread": thread_id, "turn": turn })),
                subscribe: Some(thread_id),
                broadcasts,
            };
        }

        if self.harness.as_ref().is_some_and(AcpHarness::is_initialized) {
            self.dispatch_turn(&thread_id);
        }

        RequestOutcome {
            reply: Some(json!({
                "reply": "sent",
                "thread": thread_id,
                "turn": turn,
            })),
            subscribe: Some(thread_id),
            broadcasts,
        }
    }

    fn handle_stop(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(thread_id) = value.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "Expected a non-empty string thread field.");
        };
        let requested_turn = match value.get("turn") {
            None | Some(Value::Null) => None,
            Some(v) => match v.as_u64() {
                Some(t) => Some(t),
                None => return error_outcome("invalid-request", "Expected turn to be a number or null."),
            },
        };

        match self.store.thread_meta(thread_id) {
            Ok(None) => return error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
            Err(err) => return error_outcome("storage-error", &err),
            Ok(Some(_)) => {}
        }

        // Stop withdraws what the thread asks, whether or not a turn works:
        // a harness may ask after its turn ended.
        let mut broadcasts = self.withdraw_requests(|w| w.thread_id == thread_id, log);

        let Some(active) = self.working_turns.get_mut(thread_id) else {
            return RequestOutcome {
                reply: Some(json!({ "reply": "ok" })),
                subscribe: None,
                broadcasts,
            };
        };
        if let Some(t) = requested_turn
            && active.turn != t
        {
            return RequestOutcome {
                reply: Some(json!({ "reply": "ok" })),
                subscribe: None,
                broadcasts,
            };
        }

        let ended = match &active.dispatch {
            TurnDispatch::PromptInFlight { session_id } => {
                let sid = session_id.clone();
                if active.stop_asked.is_none() {
                    active.stop_asked = Some(Instant::now());
                }
                if let Some(harness) = self.harness.as_mut() {
                    harness.cancel_session(&sid);
                }
                Vec::new()
            }
            TurnDispatch::WaitingForMode => {
                // The mode is being set on the session, and the prompt is
                // not sent: the turn ends now. Its session is the thread's
                // still.
                let turn = active.turn;
                self.working_turns.remove(thread_id);
                self.end_turn(thread_id, turn, "stopped", None, log);
                self.refresh_idle_timer();
                vec![Broadcast {
                    thread_id: thread_id.to_string(),
                    message: json!({ "event": "stopped", "thread": thread_id, "turn": turn }),
                }]
            }
            TurnDispatch::WaitingForInit | TurnDispatch::WaitingForSession | TurnDispatch::WaitingForModel => {
                let waiting_for_model = matches!(active.dispatch, TurnDispatch::WaitingForModel);
                let desired_model = active.desired_model.clone().unwrap_or_default();
                let turn = active.turn;
                self.working_turns.remove(thread_id);
                if waiting_for_model {
                    // Keep the thread locked until the in-flight `session/set_config_option`
                    // RPC settles so the next turn does not race it on the live session.
                    self.pending_set_models.push(PendingSetModel {
                        peer_id: None,
                        thread_id: thread_id.to_string(),
                        model: desired_model,
                        in_flight_on_session: true,
                    });
                }
                self.end_turn(thread_id, turn, "stopped", None, log);
                self.refresh_idle_timer();
                vec![Broadcast {
                    thread_id: thread_id.to_string(),
                    message: json!({
                        "event": "stopped",
                        "thread": thread_id,
                        "turn": turn,
                    }),
                }]
            }
        };
        broadcasts.extend(ended);

        RequestOutcome {
            reply: Some(json!({ "reply": "ok" })),
            subscribe: None,
            broadcasts,
        }
    }

    /// Whether `mode` is one Lever offers: one of its own, and, once Lever
    /// knows which the harness reports, one of those.
    fn check_mode(&self, mode: &str) -> Result<(), RequestOutcome> {
        if !acp::is_mode(mode) {
            return Err(error_outcome("invalid-mode", &format!("There is no mode {mode}.")));
        }
        if let Some(catalog) = &self.model_catalog
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

    fn handle_set_mode(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(thread_id) = value.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "Expected a non-empty string thread field.");
        };
        let Some(mode) = value.get("mode").and_then(Value::as_str) else {
            return error_outcome("invalid-request", "Expected a string mode field.");
        };
        if let Err(outcome) = self.check_mode(mode) {
            return outcome;
        }
        match self.store.thread_meta(thread_id) {
            Ok(Some(meta)) => {
                if meta.working_turn.is_some() || self.working_turns.contains_key(thread_id) {
                    return error_outcome("working", "This thread is already working on a turn.");
                }
            }
            Ok(None) => return error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
            Err(err) => return error_outcome("storage-error", &err),
        }
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

    fn handle_set_default_mode(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(mode) = value.get("mode").and_then(Value::as_str) else {
            return error_outcome("invalid-request", "Expected a string mode field.");
        };
        if !STARTING_MODES.contains(&mode) {
            return error_outcome("invalid-mode", "New threads start in ask-first or full-access.");
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

    fn handle_answer(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(thread_id) = value.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "Expected a non-empty string thread field.");
        };
        let Some(request_id) = value.get("request").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "Expected a non-empty string request field.");
        };
        let Some(answer) = value.get("answer").and_then(Value::as_str) else {
            return error_outcome("invalid-request", "Expected a string answer field.");
        };
        match self.store.thread_meta(thread_id) {
            Ok(Some(_)) => {}
            Ok(None) => return error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
            Err(err) => return error_outcome("storage-error", &err),
        }
        let no_request = || error_outcome("no-request", "That request no longer waits.");
        let Some(waiting) = self.waiting.get(request_id).filter(|w| w.thread_id == thread_id) else {
            return no_request();
        };
        let handle = waiting.handle;
        let offered = self
            .store
            .requests_of(thread_id, Some(request_id))
            .ok()
            .and_then(|found| found.into_iter().next())
            .is_some_and(|(_, record)| record.answers.iter().any(|(a, _)| a == answer));
        if !offered {
            return error_outcome("invalid-answer", &format!("This request does not offer {answer}."));
        }
        if !self
            .harness
            .as_mut()
            .is_some_and(|h| h.answer_permission(handle, Some(answer)))
        {
            self.waiting.remove(request_id);
            return no_request();
        }
        self.waiting.remove(request_id);
        RequestOutcome {
            reply: Some(json!({ "reply": "ok" })),
            subscribe: None,
            broadcasts: self.end_request(request_id, "answered", Some(answer), log),
        }
    }

    /// Ends a request in the record and tells who watches its thread.
    fn end_request(&mut self, request_id: &str, state: &str, answer: Option<&str>, log: &mut Log) -> Vec<Broadcast> {
        match self.store.end_request(request_id, state, answer, unix_now()) {
            Ok(Some((thread_id, turn, record))) => vec![Broadcast {
                thread_id: thread_id.clone(),
                message: request_event_json(&thread_id, turn, &record),
            }],
            Ok(None) => Vec::new(),
            Err(err) => {
                store_failed(log, &err);
                Vec::new()
            }
        }
    }

    /// Withdraws the requests that wait and match, answering each
    /// `cancelled` to a harness that still runs.
    fn withdraw_requests(&mut self, which: impl Fn(&Waiting) -> bool, log: &mut Log) -> Vec<Broadcast> {
        let mut ids: Vec<String> = self
            .waiting
            .iter()
            .filter(|(_, w)| which(w))
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        let mut broadcasts = Vec::new();
        for id in ids {
            if let Some(w) = self.waiting.remove(&id)
                && let Some(harness) = self.harness.as_mut()
            {
                harness.answer_permission(w.handle, None);
            }
            broadcasts.extend(self.end_request(&id, "withdrawn", None, log));
        }
        broadcasts
    }

    fn start_harness(&mut self, log: &mut Log) -> Result<(), String> {
        let cfg = self
            .harness_config
            .as_ref()
            .ok_or_else(|| "No harness is available.".to_string())?
            .clone();
        let free_hosted_models = self.free_hosted_models();
        let harness = AcpHarness::spawn(&cfg, free_hosted_models)?;
        let _ = log.write(
            Event::Harness,
            &format!("started {} (process {})", cfg.name, harness.pid()),
        );
        self.harness = Some(harness);
        self.model_catalog = None;
        self.harness_env_stale = false;
        Ok(())
    }

    fn stop_harness(&mut self, reason: &str, log: &mut Log) {
        if let Some(harness) = self.harness.take() {
            let pid = harness.pid();
            harness.stop();
            let name = self
                .harness_config
                .as_ref()
                .map(|c| c.name.as_str())
                .unwrap_or("harness");
            let _ = log.write(Event::Harness, &format!("stopped {name} (process {pid}) {reason}"));
        }
        self.model_catalog = None;
        self.harness_env_stale = false;
        self.idle_since = None;
    }

    fn dispatch_turn(&mut self, thread_id: &str) {
        let Some(harness) = self.harness.as_mut() else {
            return;
        };
        let Some(active) = self.working_turns.get_mut(thread_id) else {
            return;
        };
        if let Some(session_id) = harness.session_for_thread(thread_id).map(str::to_string) {
            if let Some(model) = active.desired_model.clone()
                && harness.session_current_model(&session_id) != Some(model.as_str())
            {
                harness.set_session_model(thread_id, Some(active.turn), &session_id, &model);
                active.dispatch = TurnDispatch::WaitingForModel;
            } else {
                self.continue_turn(thread_id);
            }
        } else {
            // A harness that was started again has seen none of this
            // thread's acts, and may still name them.
            if let Ok(ids) = self.store.harness_call_ids(thread_id) {
                harness.remember_tool_ids(thread_id, ids);
            }
            active.dispatch = TurnDispatch::WaitingForSession;
            harness.ensure_session(
                thread_id,
                active.turn,
                &active.cwd,
                active.existing_session_id.as_deref(),
            );
        }
    }

    /// A turn whose session has its Model: the session is set to the
    /// thread's mode, when the harness has modes and it is in another, and
    /// then the prompt is sent.
    fn continue_turn(&mut self, thread_id: &str) {
        let Some(harness) = self.harness.as_mut() else {
            return;
        };
        let Some(active) = self.working_turns.get_mut(thread_id) else {
            return;
        };
        let Some(session_id) = harness.session_for_thread(thread_id).map(str::to_string) else {
            return;
        };
        if harness.has_modes() && harness.session_current_mode(&session_id) != Some(active.mode.as_str()) {
            harness.set_session_mode(thread_id, active.turn, &session_id, &active.mode);
            active.dispatch = TurnDispatch::WaitingForMode;
        } else {
            harness.send_prompt(
                thread_id,
                active.turn,
                &session_id,
                active.context_prompt.as_deref(),
                &active.message,
            );
            active.dispatch = TurnDispatch::PromptInFlight { session_id };
        }
    }

    fn apply_acp_event(&mut self, event: AcpEvent, log: &mut Log) -> Effects {
        match event {
            AcpEvent::Initialized => {
                let waiting: Vec<String> = self
                    .working_turns
                    .iter()
                    .filter(|(_, t)| matches!(t.dispatch, TurnDispatch::WaitingForInit))
                    .map(|(tid, _)| tid.clone())
                    .collect();
                for tid in waiting {
                    self.dispatch_turn(&tid);
                }
                let needs_probe =
                    !self.pending_models.is_empty() || self.pending_set_models.iter().any(|p| !p.in_flight_on_session);
                if needs_probe && let Some(harness) = self.harness.as_mut() {
                    harness.probe_models(&default_cwd());
                }
                Effects::default()
            }
            AcpEvent::InitializeFailed { message } => {
                // What the harness said goes to the turns it fails, which
                // are the person's, and not into the log.
                let _ = log.write(Event::Error, "the harness could not be initialized");
                let tids: Vec<(String, u64)> = self
                    .working_turns
                    .iter()
                    .map(|(tid, t)| (tid.clone(), t.turn))
                    .collect();
                let mut effects = Effects::default();
                for (tid, turn) in tids {
                    effects.broadcasts.extend(self.fail_turn(&tid, turn, &message, log));
                }
                effects.replies.extend(self.fail_pending_peer_requests(&message));
                self.stop_harness("after initialize failure", log);
                effects
            }
            AcpEvent::ModelsProbed(result) => {
                let mut effects = Effects::default();
                if self.harness_env_stale {
                    let msg = "Lever is restarting the harness after the free hosted Models setting changed; try again when active turns finish.";
                    for peer_id in self.pending_models.drain(..) {
                        effects.replies.push(PeerReply {
                            peer_id,
                            message: error_json("working", msg),
                            subscribe: None,
                        });
                    }
                    let mut remaining = Vec::new();
                    for item in std::mem::take(&mut self.pending_set_models) {
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
                    self.pending_set_models = remaining;
                    return effects;
                }
                match result {
                    Ok(catalog) => {
                        let free = self.free_hosted_models();
                        for peer_id in self.pending_models.drain(..) {
                            effects.replies.push(PeerReply {
                                peer_id,
                                message: models_reply_json(&catalog, free),
                                subscribe: None,
                            });
                        }
                        let mut remaining = Vec::new();
                        let pending = std::mem::take(&mut self.pending_set_models);
                        for item in pending {
                            if item.in_flight_on_session {
                                remaining.push(item);
                            } else {
                                let outcome =
                                    self.finish_set_model_with_catalog(&item.thread_id, &item.model, &catalog, log);
                                if let (Some(peer_id), Some(reply)) = (item.peer_id, outcome.reply) {
                                    effects.replies.push(PeerReply {
                                        peer_id,
                                        message: reply,
                                        subscribe: outcome.subscribe,
                                    });
                                }
                            }
                        }
                        self.pending_set_models = remaining;
                        self.update_model_catalog(catalog, true);
                    }
                    Err(msg) => {
                        for peer_id in self.pending_models.drain(..) {
                            effects.replies.push(PeerReply {
                                peer_id,
                                message: error_json("harness-error", &msg),
                                subscribe: None,
                            });
                        }
                        let mut remaining = Vec::new();
                        for item in std::mem::take(&mut self.pending_set_models) {
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
                        self.pending_set_models = remaining;
                    }
                }
                effects
            }
            AcpEvent::SessionReady {
                thread_id,
                session_id,
                afresh,
                catalog,
                current_model,
            } => {
                let mut effects = Effects::default();
                if let Some(cat) = catalog {
                    self.update_model_catalog(cat, false);
                }
                // The record must say the session is this thread's before
                // Lever uses it. Otherwise the record could still give its ID
                // to the thread that had it before, to load at a later start.
                if let Err(err) = self.store.set_harness_session_id(&thread_id, &session_id) {
                    store_failed(log, &err);
                    if let Some(harness) = self.harness.as_mut() {
                        harness.forget_session(&thread_id);
                    }
                    let waiting = self
                        .working_turns
                        .get(&thread_id)
                        .filter(|active| matches!(active.dispatch, TurnDispatch::WaitingForSession))
                        .map(|active| active.turn);
                    if let Some(turn) = waiting {
                        effects
                            .broadcasts
                            .extend(self.fail_turn(&thread_id, turn, UNSAVED_SESSION_MESSAGE, log));
                    }
                    return effects;
                }
                if afresh {
                    let after_turn = !self.working_turns.contains_key(&thread_id);
                    effects.broadcasts.extend(self.record_note(
                        &thread_id,
                        NEW_SESSION_SOURCE,
                        NEW_SESSION_NOTE,
                        after_turn,
                        log,
                    ));
                }
                // The session is the thread's. The turn that waits for it is
                // the one working now: the turn that first asked may have
                // been stopped since.
                let desired_model = self
                    .working_turns
                    .get(&thread_id)
                    .filter(|active| matches!(active.dispatch, TurnDispatch::WaitingForSession))
                    .and_then(|active| active.desired_model.clone());
                if let Some(active) = self.working_turns.get_mut(&thread_id)
                    && matches!(active.dispatch, TurnDispatch::WaitingForSession)
                    && let Some(harness) = self.harness.as_mut()
                {
                    if let Some(model) = desired_model
                        && current_model.as_deref() != Some(model.as_str())
                    {
                        harness.set_session_model(&thread_id, Some(active.turn), &session_id, &model);
                        active.dispatch = TurnDispatch::WaitingForModel;
                    } else {
                        if let Some(ref model) = current_model
                            && let Err(err) = self.store.set_thread_model(&thread_id, model, unix_now())
                        {
                            store_failed(log, &err);
                        }
                        self.continue_turn(&thread_id);
                    }
                }
                effects
            }
            AcpEvent::SessionFailed {
                thread_id,
                turn,
                message,
                no_models,
            } => {
                let display = if no_models {
                    self.model_catalog = None;
                    NO_MODEL_MESSAGE.to_string()
                } else {
                    message
                };
                Effects {
                    broadcasts: self.fail_turn(&thread_id, turn, &display, log),
                    replies: Vec::new(),
                }
            }
            AcpEvent::ModelSet {
                thread_id,
                turn,
                model,
                catalog,
            } => {
                let mut effects = Effects::default();
                if let Some(cat) = catalog {
                    self.update_model_catalog(cat, false);
                }
                if let Some(t) = turn {
                    let still_waiting = self.working_turns.get(&thread_id).is_some_and(|active| {
                        active.turn == t && matches!(active.dispatch, TurnDispatch::WaitingForModel)
                    });
                    if !still_waiting {
                        if let Some(pos) = self
                            .pending_set_models
                            .iter()
                            .position(|p| p.in_flight_on_session && p.peer_id.is_none() && p.thread_id == thread_id)
                        {
                            self.pending_set_models.remove(pos);
                        }
                        return effects;
                    }
                }
                if let Err(err) = self.store.set_thread_model(&thread_id, &model, unix_now()) {
                    store_failed(log, &err);
                    if let Some(t) = turn {
                        effects
                            .broadcasts
                            .extend(self.fail_turn(&thread_id, t, UNSAVED_MESSAGE, log));
                    } else if let Some(pos) = self
                        .pending_set_models
                        .iter()
                        .position(|p| p.in_flight_on_session && p.thread_id == thread_id)
                    {
                        let item = self.pending_set_models.remove(pos);
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
                if let Some(t) = turn {
                    if self.working_turns.get(&thread_id).is_some_and(|active| {
                        active.turn == t && matches!(active.dispatch, TurnDispatch::WaitingForModel)
                    }) {
                        self.continue_turn(&thread_id);
                    }
                } else if let Some(pos) = self
                    .pending_set_models
                    .iter()
                    .position(|p| p.in_flight_on_session && p.thread_id == thread_id)
                {
                    let item = self.pending_set_models.remove(pos);
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
            AcpEvent::ModelSetFailed {
                thread_id,
                turn,
                message,
            } => {
                let mut effects = Effects::default();
                if let Some(t) = turn {
                    if self
                        .working_turns
                        .get(&thread_id)
                        .is_some_and(|active| active.turn == t)
                    {
                        effects.broadcasts.extend(self.fail_turn(&thread_id, t, &message, log));
                    } else if let Some(pos) = self
                        .pending_set_models
                        .iter()
                        .position(|p| p.in_flight_on_session && p.peer_id.is_none() && p.thread_id == thread_id)
                    {
                        self.pending_set_models.remove(pos);
                    }
                } else if let Some(pos) = self
                    .pending_set_models
                    .iter()
                    .position(|p| p.in_flight_on_session && p.thread_id == thread_id)
                {
                    let item = self.pending_set_models.remove(pos);
                    if let Some(peer_id) = item.peer_id {
                        effects.replies.push(PeerReply {
                            peer_id,
                            message: error_json("invalid-model", &message),
                            subscribe: None,
                        });
                    }
                }
                effects
            }
            AcpEvent::CatalogUpdated {
                thread_id,
                catalog,
                current_model,
                current_mode,
            } => {
                if let Some(catalog) = catalog {
                    self.update_model_catalog(catalog, false);
                }
                if let Some(model) = current_model
                    && let Err(err) = self.store.set_thread_model(&thread_id, &model, unix_now())
                {
                    store_failed(log, &err);
                }
                let mut effects = Effects::default();
                // The harness changed the session's mode itself: the thread
                // is in that mode now, and a turn that waits to set its mode
                // sets it again.
                if let Some(mode) = current_mode {
                    let waiting_for_mode = self
                        .working_turns
                        .get(&thread_id)
                        .is_some_and(|a| matches!(a.dispatch, TurnDispatch::WaitingForMode));
                    if !waiting_for_mode {
                        if let Some(active) = self.working_turns.get_mut(&thread_id) {
                            active.mode = mode.clone();
                        }
                        if let Err(err) = self.store.set_thread_mode(&thread_id, &mode, unix_now()) {
                            self.could_not_save(&thread_id, &err, log);
                        }
                        effects.broadcasts.push(Broadcast {
                            thread_id: thread_id.clone(),
                            message: json!({ "event": "thread-updated", "thread": thread_id, "mode": mode }),
                        });
                    }
                }
                effects
            }
            AcpEvent::ModeSet { thread_id, turn } => {
                if self
                    .working_turns
                    .get(&thread_id)
                    .is_some_and(|a| a.turn == turn && matches!(a.dispatch, TurnDispatch::WaitingForMode))
                {
                    self.continue_turn(&thread_id);
                }
                Effects::default()
            }
            AcpEvent::ModeSetFailed {
                thread_id,
                turn,
                message,
            } => Effects {
                broadcasts: self.fail_turn(&thread_id, turn, &message, log),
                replies: Vec::new(),
            },
            AcpEvent::PermissionRequested {
                thread_id,
                handle,
                harness_call_id,
                kind,
                title,
                detail,
                path,
                answers,
            } => self.record_request(
                &thread_id,
                handle,
                &harness_call_id,
                &kind,
                &title,
                detail.as_deref(),
                path.as_deref(),
                &answers,
                log,
            ),
            AcpEvent::PermissionWithdrawn { handle } => {
                let id = self
                    .waiting
                    .iter()
                    .find(|(_, w)| w.handle == handle)
                    .map(|(id, _)| id.clone());
                let mut effects = Effects::default();
                if let Some(id) = id {
                    self.waiting.remove(&id);
                    effects.broadcasts = self.end_request(&id, "withdrawn", None, log);
                }
                effects
            }
            AcpEvent::TextChunk { thread_id, text } => {
                let Some(turn) = self.turn_for_update(&thread_id) else {
                    return Effects::default();
                };
                // The answer still goes to who is watching. The turn's end
                // says that not all of it was saved.
                if let Err(err) = self.store.append_answer(&thread_id, turn, &text, unix_now()) {
                    self.could_not_save(&thread_id, &err, log);
                }
                Effects {
                    broadcasts: vec![Broadcast {
                        thread_id: thread_id.clone(),
                        message: json!({
                            "event": "text",
                            "thread": thread_id,
                            "turn": turn,
                            "text": text,
                        }),
                    }],
                    replies: Vec::new(),
                }
            }
            AcpEvent::Act {
                thread_id,
                harness_call_id,
                is_new,
                kind,
                title,
                state,
                detail,
            } => {
                let Some(turn) = self.turn_for_update(&thread_id) else {
                    return Effects::default();
                };
                let patch = ActPatch {
                    thread_id: &thread_id,
                    turn_number: turn,
                    harness_call_id: &harness_call_id,
                    is_new,
                    kind: kind.as_deref(),
                    title: title.as_deref(),
                    state: state.as_deref(),
                    detail: detail.as_deref(),
                };
                let (recorded_turn, act) = match self.store.upsert_act(&patch, unix_now()) {
                    Ok(saved) => saved,
                    Err(err) => {
                        self.could_not_save(&thread_id, &err, log);
                        return Effects::default();
                    }
                };
                Effects {
                    broadcasts: vec![Broadcast {
                        thread_id: thread_id.clone(),
                        message: act_event_json(&thread_id, recorded_turn, &act),
                    }],
                    replies: Vec::new(),
                }
            }
            AcpEvent::TitleUpdated { thread_id, title } => {
                if let Err(err) = self.store.update_thread_title(&thread_id, &title, unix_now()) {
                    self.could_not_save(&thread_id, &err, log);
                    return Effects::default();
                }
                Effects {
                    broadcasts: vec![Broadcast {
                        thread_id: thread_id.clone(),
                        message: json!({
                            "event": "thread-updated",
                            "thread": thread_id,
                            "title": title,
                        }),
                    }],
                    replies: Vec::new(),
                }
            }
            AcpEvent::UnknownUpdate {
                thread_id,
                raw_json,
                plain_text,
            } => {
                let after_turn = !self.working_turns.contains_key(&thread_id);
                Effects {
                    broadcasts: self.record_note(&thread_id, &raw_json, &plain_text, after_turn, log),
                    replies: Vec::new(),
                }
            }
            AcpEvent::PromptFinished {
                thread_id,
                turn,
                outcome,
            } => {
                // An answer for a turn that is not the one working ends nothing.
                if self
                    .working_turns
                    .get(&thread_id)
                    .is_none_or(|active| active.turn != turn)
                {
                    return Effects::default();
                }
                let unsaved = self
                    .working_turns
                    .remove(&thread_id)
                    .is_some_and(|active| active.unsaved);
                let outcome = match outcome {
                    TurnOutcome::Done if unsaved => TurnOutcome::Failed(UNSAVED_MESSAGE.to_string()),
                    other => other,
                };
                let broadcasts = match outcome {
                    TurnOutcome::Done => {
                        self.end_turn(&thread_id, turn, "done", None, log);
                        vec![Broadcast {
                            thread_id: thread_id.clone(),
                            message: json!({
                                "event": "done",
                                "thread": thread_id,
                                "turn": turn,
                            }),
                        }]
                    }
                    TurnOutcome::Stopped => {
                        self.end_turn(&thread_id, turn, "stopped", None, log);
                        vec![Broadcast {
                            thread_id: thread_id.clone(),
                            message: json!({
                                "event": "stopped",
                                "thread": thread_id,
                                "turn": turn,
                            }),
                        }]
                    }
                    TurnOutcome::Failed(msg) => {
                        self.end_turn(&thread_id, turn, "failed", Some(&msg), log);
                        vec![Broadcast {
                            thread_id: thread_id.clone(),
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
            AcpEvent::Exited => Effects::default(),
        }
    }

    fn fail_pending_peer_requests(&mut self, message: &str) -> Vec<PeerReply> {
        let mut replies = Vec::new();
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
        replies
    }

    /// Keeps a request the harness asked, and tells who watches its thread.
    /// A request the database does not take is answered `cancelled`: what the
    /// record cannot hold, nobody could answer after a restart either.
    #[allow(clippy::too_many_arguments)]
    fn record_request(
        &mut self,
        thread_id: &str,
        handle: u64,
        harness_call_id: &str,
        kind: &str,
        title: &str,
        detail: Option<&str>,
        path: Option<&str>,
        answers: &[(String, String)],
        log: &mut Log,
    ) -> Effects {
        let mut effects = Effects::default();
        let Some(turn) = self.turn_for_update(thread_id) else {
            if let Some(harness) = self.harness.as_mut() {
                harness.answer_permission(handle, None);
            }
            return effects;
        };
        // The act it asks about, so the request and the act are one thing in
        // the thread.
        let patch = ActPatch {
            thread_id,
            turn_number: turn,
            harness_call_id,
            is_new: false,
            kind: Some(kind),
            title: Some(title),
            state: None,
            detail: None,
        };
        let act = match self.store.upsert_act(&patch, unix_now()) {
            Ok((act_turn, act)) => {
                effects.broadcasts.push(Broadcast {
                    thread_id: thread_id.to_string(),
                    message: act_event_json(thread_id, act_turn, &act),
                });
                Some(act.id)
            }
            Err(err) => {
                self.could_not_save(thread_id, &err, log);
                None
            }
        };
        let request = NewRequest {
            thread_id,
            turn_number: turn,
            act: act.as_deref(),
            kind,
            title,
            detail,
            path,
            answers,
        };
        match self.store.insert_request(&request, unix_now()) {
            Ok(record) => {
                self.waiting.insert(
                    record.id.clone(),
                    Waiting {
                        handle,
                        thread_id: thread_id.to_string(),
                    },
                );
                effects.broadcasts.push(Broadcast {
                    thread_id: thread_id.to_string(),
                    message: request_event_json(thread_id, turn, &record),
                });
            }
            Err(err) => {
                self.could_not_save(thread_id, &err, log);
                if let Some(harness) = self.harness.as_mut() {
                    harness.answer_permission(handle, None);
                }
            }
        }
        effects
    }

    fn on_harness_exited(&mut self, log: &mut Log) -> Effects {
        if let Some(harness) = self.harness.take() {
            let pid = harness.pid();
            let fault = harness.fault();
            harness.stop();
            let name = self
                .harness_config
                .as_ref()
                .map(|c| c.name.as_str())
                .unwrap_or("harness");
            let what = match fault {
                Some(fault) => format!("{name} (process {pid}) was stopped: it {fault}"),
                None => format!("{name} (process {pid}) exited unexpectedly"),
            };
            let _ = log.write(Event::Harness, &what);
        }
        self.after_harness_gone("The harness exited unexpectedly.", log)
    }

    /// What follows a harness that is gone: what waited on it fails, every
    /// request it asked is withdrawn, and every turn it was working on is
    /// interrupted.
    fn after_harness_gone(&mut self, why: &str, log: &mut Log) -> Effects {
        self.model_catalog = None;
        self.harness_env_stale = false;
        self.idle_since = None;

        let replies = self.fail_pending_peer_requests(why);
        let mut broadcasts = self.withdraw_requests(|_| true, log);

        // Every turn that was working is interrupted, and each end goes the
        // way every other end does, so one the database does not take now is
        // tried again.
        let mut interrupted: Vec<(String, u64)> = self
            .working_turns
            .drain()
            .map(|(thread_id, active)| (thread_id, active.turn))
            .collect();
        interrupted.sort();
        for (thread_id, turn) in interrupted {
            self.end_turn(&thread_id, turn, "interrupted", None, log);
            broadcasts.push(Broadcast {
                thread_id: thread_id.clone(),
                message: json!({
                    "event": "interrupted",
                    "thread": thread_id,
                    "turn": turn,
                }),
            });
        }
        Effects { broadcasts, replies }
    }

    /// Fails the working turn of a thread. A failure that comes for a turn
    /// that has ended already, or for one before the turn now working,
    /// changes nothing: that turn's end is in the record.
    fn fail_turn(&mut self, thread_id: &str, turn: u64, message: &str, log: &mut Log) -> Vec<Broadcast> {
        if self
            .working_turns
            .get(thread_id)
            .is_none_or(|active| active.turn != turn)
        {
            return Vec::new();
        }
        self.working_turns.remove(thread_id);
        self.end_turn(thread_id, turn, "failed", Some(message), log);
        self.refresh_idle_timer();
        vec![Broadcast {
            thread_id: thread_id.to_string(),
            message: json!({
                "event": "failed",
                "thread": thread_id,
                "turn": turn,
                "message": message,
            }),
        }]
    }

    /// Records how a turn ended. An end the database does not take is kept
    /// and tried again on each wake, so the thread is not left working in the
    /// record after its turn is over.
    fn end_turn(&mut self, thread_id: &str, turn: u64, state: &'static str, error: Option<&str>, log: &mut Log) {
        if let Err(err) = self.store.finish_turn(thread_id, turn, state, error, unix_now()) {
            store_failed(log, &err);
            self.unsaved_endings.push(Ending {
                thread_id: thread_id.to_string(),
                turn,
                state,
                error: error.map(str::to_string),
            });
        }
    }

    fn save_endings(&mut self) {
        if self.unsaved_endings.is_empty() {
            return;
        }
        let now = unix_now();
        let store = &self.store;
        self.unsaved_endings.retain(|ending| {
            store
                .finish_turn(
                    &ending.thread_id,
                    ending.turn,
                    ending.state,
                    ending.error.as_deref(),
                    now,
                )
                .is_err()
        });
    }

    /// The database did not take something of a thread. The log says so, and
    /// the turn the thread is working on ends as failed, not as done.
    fn could_not_save(&mut self, thread_id: &str, err: &str, log: &mut Log) {
        store_failed(log, err);
        if let Some(active) = self.working_turns.get_mut(thread_id) {
            active.unsaved = true;
        }
    }

    /// Keeps a note on a thread's newest turn and tells who is watching.
    fn record_note(
        &mut self,
        thread_id: &str,
        raw_json: &str,
        plain_text: &str,
        after_turn: bool,
        log: &mut Log,
    ) -> Vec<Broadcast> {
        let Some(turn) = self.turn_for_update(thread_id) else {
            return Vec::new();
        };
        let note = match self
            .store
            .insert_note(thread_id, turn, raw_json, plain_text, after_turn, unix_now())
        {
            Ok(note) => note,
            Err(err) => {
                self.could_not_save(thread_id, &err, log);
                return Vec::new();
            }
        };
        vec![Broadcast {
            thread_id: thread_id.to_string(),
            message: json!({
                "event": "note",
                "thread": thread_id,
                "turn": turn,
                "note": note.id,
                "text": note.text,
            }),
        }]
    }

    fn turn_for_update(&self, thread_id: &str) -> Option<u64> {
        if let Some(active) = self.working_turns.get(thread_id) {
            return Some(active.turn);
        }
        self.store
            .thread_meta(thread_id)
            .ok()
            .flatten()
            .and_then(|m| m.latest_turn)
    }

    fn refresh_idle_timer(&mut self) {
        if self.harness.is_some() && !self.has_active_work() {
            if self.idle_since.is_none() {
                self.idle_since = Some(Instant::now());
            }
        } else {
            self.idle_since = None;
        }
    }
}

/// Says in the log that the database did not take a write. The database's
/// error names what it could not do, and nothing a thread said.
fn store_failed(log: &mut Log, err: &str) {
    let _ = log.write(Event::Error, &format!("storage: {err}"));
}

fn error_json(code: &str, message: &str) -> Value {
    json!({
        "reply": "error",
        "code": code,
        "message": message,
    })
}

fn error_outcome(code: &str, message: &str) -> RequestOutcome {
    RequestOutcome {
        reply: Some(error_json(code, message)),
        subscribe: None,
        broadcasts: Vec::new(),
    }
}

fn models_reply_json(catalog: &ModelCatalog, free_hosted_models: bool) -> Value {
    let models: Vec<Value> = catalog
        .models
        .iter()
        .map(|m| json!({ "id": m.id, "name": m.name }))
        .collect();
    json!({
        "reply": "models",
        "supported": catalog.supported,
        "models": models,
        "default": catalog.default_model,
        "free_hosted_models": free_hosted_models,
        "modes": catalog
            .modes
            .iter()
            .map(|m| json!({ "id": m, "name": acp::mode_name(m).unwrap_or(m) }))
            .collect::<Vec<_>>(),
    })
}

fn thread_summary_json(t: &ThreadSummary) -> Value {
    let mut obj = json!({
        "thread": t.id,
        "title": t.title,
        "cwd": t.cwd,
        "started": t.started,
        "updated": t.updated,
        "turns": t.turns,
        "state": t.state,
        "working": t.working,
        "waiting": t.waiting,
        "mode": t.mode,
    });
    if let Some(model) = &t.model {
        obj["model"] = json!(model);
    }
    obj
}

fn thread_record_json(r: &ThreadRecord) -> Value {
    let mut obj = json!({
        "reply": "resumed",
        "thread": r.id,
        "title": r.title,
        "cwd": r.cwd,
        "started": r.started,
        "updated": r.updated,
        "working": r.working,
        "mode": r.mode,
        "turns": r.turns.iter().map(turn_record_json).collect::<Vec<_>>(),
    });
    if let Some(model) = &r.model {
        obj["model"] = json!(model);
    }
    obj
}

fn turn_record_json(t: &TurnRecord) -> Value {
    let mut obj = json!({
        "turn": t.turn,
        "message": t.message,
        "answer": t.answer,
        "state": t.state,
        "acts": t.acts.iter().map(act_record_json).collect::<Vec<_>>(),
        "requests": t.requests.iter().map(request_record_json).collect::<Vec<_>>(),
        "notes": t.notes.iter().map(note_record_json).collect::<Vec<_>>(),
    });
    if let Some(mode) = &t.mode {
        obj["mode"] = json!(mode);
    }
    if let Some(context) = &t.context {
        obj["context"] = context.clone();
    }
    if let Some(err) = &t.error {
        obj["error"] = json!(err);
    }
    obj
}

fn act_record_json(a: &ActRecord) -> Value {
    let mut obj = json!({
        "act": a.id,
        "kind": a.kind,
        "title": a.title,
        "state": a.state,
    });
    if let Some(detail) = &a.detail {
        obj["detail"] = json!(detail);
    }
    obj
}

fn act_event_json(thread_id: &str, turn: u64, a: &ActRecord) -> Value {
    let mut obj = act_record_json(a);
    obj["event"] = json!("act");
    obj["thread"] = json!(thread_id);
    obj["turn"] = json!(turn);
    obj
}

fn request_record_json(r: &RequestRecord) -> Value {
    let mut obj = json!({
        "request": r.id,
        "kind": r.kind,
        "title": r.title,
        "answers": r
            .answers
            .iter()
            .map(|(answer, name)| json!({ "answer": answer, "name": name }))
            .collect::<Vec<_>>(),
        "state": r.state,
    });
    for (field, value) in [
        ("act", &r.act),
        ("detail", &r.detail),
        ("path", &r.path),
        ("answer", &r.answer),
    ] {
        if let Some(value) = value {
            obj[field] = json!(value);
        }
    }
    obj
}

fn request_event_json(thread_id: &str, turn: u64, r: &RequestRecord) -> Value {
    let mut obj = request_record_json(r);
    obj["event"] = json!("request");
    obj["thread"] = json!(thread_id);
    obj["turn"] = json!(turn);
    obj
}

fn note_record_json(n: &NoteRecord) -> Value {
    json!({
        "note": n.id,
        "text": n.text,
    })
}

fn initial_title(text: &str) -> String {
    let first_line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Thread");
    let mut title = String::new();
    for ch in first_line.chars().take(80) {
        title.push(ch);
    }
    title
}

fn default_cwd() -> String {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty() && Path::new(h).is_absolute())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .and_then(|p| p.to_str().map(str::to_string))
        })
        .unwrap_or_else(|| "/".to_string())
}

pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
