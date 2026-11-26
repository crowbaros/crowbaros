//! Lever's core service controller: owns the SQLite thread store, the on-demand
//! ACP harness lifecycle (including the idle timer), active turns across
//! concurrent threads, and Lever's wire messages.

use crate::acp::{self, AcpEvent, AcpHarness, EffortOption, HarnessCommand, HarnessConfig, ModelCatalog, TurnOutcome};
use crate::channel::LEVER_VERSION;
use crate::fulcrum;
use crate::log::{Event, Log};
use crate::store::{
    ActPatch, ActRecord, NewRequest, NewThread, NoteRecord, RequestRecord, Store, SubagentActPatch, SubagentPatch,
    SubagentRecord, ThreadRecord, ThreadSummary, TurnRecord,
};
use crate::sys;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Metadata subscription for the drawer's thread list.
pub const THREAD_LIST_SUBSCRIPTION: &str = "@thread-list";

/// Default time an idle harness stays running after the last working turn ends.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Default time a harness has to end a turn after Lever asks it to stop.
pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the harness says nothing in a turn it began itself before Lever
/// takes that turn as done. ACP gives such a turn no end of its own.
pub const DEFAULT_WAKE_QUIET: Duration = Duration::from_secs(10);
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
    WaitingForEffort,
    WaitingForMode,
    PromptInFlight {
        session_id: String,
    },
    /// A turn the harness began itself, as when a subagent's report wakes its
    /// parent session: no prompt is in flight, and none ends it. It ends when
    /// the harness has been quiet for `wake_quiet`, when the person sends a
    /// message, which the harness takes into the work it is doing, or when
    /// the person stops it.
    HarnessStarted {
        session_id: String,
        last_heard: Instant,
        /// The turn's acts the harness says still work.
        open_acts: HashSet<String>,
    },
}

struct WorkingTurn {
    turn: u64,
    message: String,
    context_prompt: Option<String>,
    cwd: String,
    existing_session_id: Option<String>,
    desired_model: Option<String>,
    /// The thread's effort, which the session is set to before the prompt
    /// when its Model offers it.
    desired_effort: Option<String>,
    /// The thread's mode, which the session is set to before the prompt.
    mode: String,
    dispatch: TurnDispatch,
    /// Something of this turn did not reach the database.
    unsaved: bool,
    /// When the person asked to stop this turn and the harness was told.
    stop_asked: Option<Instant>,
}

/// A request that waits for the person, as Lever holds it: the harness's
/// handle for answering it, and its thread and turn.
struct Waiting {
    handle: u64,
    thread_id: String,
    turn: u64,
    /// The request as the record has it, so how it ends can be said though
    /// the database takes nothing.
    record: RequestRecord,
}

/// How a request ended, kept until the database has it.
struct RequestEnd {
    request_id: String,
    thread_id: String,
    state: &'static str,
    answer: Option<String>,
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

/// A subagent currently working on a thread, tracked so the thread stays
/// working and the harness stays alive even after the parent turn ends.
struct WorkingSubagent {
    thread_id: String,
    turn: u64,
    subagent_id: String,
    harness_session_id: String,
    title: String,
    started: i64,
    /// When the person asked to stop this subagent's thread and the harness
    /// was told.
    stop_asked: Option<Instant>,
}

/// How a subagent ended, kept until the database has it.
struct SubagentEnding {
    thread_id: String,
    subagent_id: String,
    state: &'static str,
    error: Option<String>,
}

struct PendingDelete {
    peer_id: u64,
    thread: String,
    session: String,
    dispatched: bool,
}

pub struct Service {
    pending_deletes: Vec<PendingDelete>,
    store: Store,
    harness_config: Option<HarnessConfig>,
    fulcrum_socket: Option<PathBuf>,
    harness: Option<AcpHarness>,
    model_catalog: Option<ModelCatalog>,
    /// True when `free_hosted_models` changed while the harness was busy, so
    /// it should be stopped as soon as all active work finishes.
    harness_env_stale: bool,
    /// True when `models` with `refresh: true` arrived while the harness was
    /// busy, so Lever restarts and re-probes Models as soon as active work
    /// finishes.
    models_refresh_pending: bool,
    idle_timeout: Duration,
    stop_timeout: Duration,
    wake_quiet: Duration,
    idle_since: Option<Instant>,
    working_turns: HashMap<String, WorkingTurn>,
    /// Threads that had a subagent in the harness now running: what their
    /// session says after their turn ended is a turn the harness began.
    subagent_threads: HashSet<String>,
    /// Threads whose Stop left nothing of them working, until the person's
    /// next message: a turn the harness begins on one is stopped as it
    /// begins, as what reports to it was.
    held_stops: HashSet<String>,
    /// Subagents currently working, keyed by Lever's `sub-...` ID.
    working_subagents: HashMap<String, WorkingSubagent>,
    /// Requests that wait, by Lever's request ID.
    waiting: HashMap<String, Waiting>,
    /// What `activity` last said: threads working, and threads waiting.
    activity: (Vec<String>, Vec<String>),
    pending_models: Vec<u64>,
    pending_set_models: Vec<PendingSetModel>,
    /// The effort levels of each Model, as the harness now running said them.
    /// A Model with none has an empty list.
    efforts: HashMap<String, Vec<EffortOption>>,
    /// Peers that wait for a Model's effort levels.
    pending_efforts: Vec<(u64, String)>,
    /// The commands the harness last reported for a session Lever opened to
    /// read what it offers, and those each thread's session last reported.
    commands: Option<Vec<HarnessCommand>>,
    thread_commands: HashMap<String, Vec<HarnessCommand>>,
    /// Whether the running harness has already completed a probe for what it
    /// offers, so polling `commands` does not re-probe when a harness sends
    /// no `available_commands_update`.
    commands_probed: bool,
    /// Threads whose effort was saved via `set-effort` while idle and has not
    /// yet been applied to the harness session on the next turn.
    pending_effort_threads: HashSet<String>,
    /// What the harness offers is to be read once it is initialized.
    probe_wanted: bool,
    /// Threads whose turn was stopped while the harness set the session's
    /// mode or effort. Each stays working until the harness answers, so no
    /// turn after it runs in a mode it did not ask for.
    /// When Lever stopped each turn, so a harness that never answers is
    /// ended after `stop_timeout`, as one that ignores a Stop is.
    modes_in_flight: Vec<(String, Instant)>,
    /// Ends the database did not take, tried again on each wake.
    unsaved_endings: Vec<Ending>,
    /// Ends of subagents the database did not take, tried again on each wake.
    unsaved_subagent_endings: Vec<SubagentEnding>,
    /// Ends of requests the database did not take, tried again on each wake.
    unsaved_request_ends: Vec<RequestEnd>,
}

impl Service {
    pub fn new(
        store: Store,
        harness_config: Option<HarnessConfig>,
        fulcrum_socket: Option<PathBuf>,
        idle_timeout: Duration,
        stop_timeout: Duration,
        wake_quiet: Duration,
    ) -> Self {
        Self {
            pending_deletes: Vec::new(),
            store,
            harness_config,
            fulcrum_socket,
            harness: None,
            model_catalog: None,
            harness_env_stale: false,
            models_refresh_pending: false,
            idle_timeout,
            stop_timeout,
            wake_quiet,
            idle_since: None,
            working_turns: HashMap::new(),
            subagent_threads: HashSet::new(),
            held_stops: HashSet::new(),
            working_subagents: HashMap::new(),
            waiting: HashMap::new(),
            activity: (Vec::new(), Vec::new()),
            pending_models: Vec::new(),
            pending_set_models: Vec::new(),
            efforts: HashMap::new(),
            pending_efforts: Vec::new(),
            commands: None,
            thread_commands: HashMap::new(),
            commands_probed: false,
            pending_effort_threads: HashSet::new(),
            probe_wanted: false,
            modes_in_flight: Vec::new(),
            unsaved_endings: Vec::new(),
            unsaved_subagent_endings: Vec::new(),
            unsaved_request_ends: Vec::new(),
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
        !self.pending_deletes.is_empty()
            || !self.working_turns.is_empty()
            || !self.working_subagents.is_empty()
            || !self.waiting.is_empty()
            || !self.pending_models.is_empty()
            || !self.pending_set_models.is_empty()
            || !self.pending_efforts.is_empty()
            || !self.modes_in_flight.is_empty()
            || self.probe_wanted
            || self.harness.as_ref().is_some_and(AcpHarness::probing_models)
    }

    /// Whether Lever is still setting a thread's session to what the thread
    /// asks: until it has, what the harness says the session is at is not
    /// the thread's.
    fn settling(&self, thread_id: &str) -> bool {
        self.working_turns.get(thread_id).is_some_and(|active| {
            !matches!(
                active.dispatch,
                TurnDispatch::PromptInFlight { .. } | TurnDispatch::HarnessStarted { .. }
            )
        }) || self.pending_set_models.iter().any(|p| p.thread_id == thread_id)
            || self.modes_in_flight.iter().any(|(t, _)| t == thread_id)
    }

    /// Whether a thread is busy: a turn works on it, or the harness has yet
    /// to answer a change Lever asked of its session. A subagent that works
    /// on does not make it busy: the person may send a message, or change
    /// the Model, mode, or effort, as the harness takes each in the session
    /// its subagents came from. A turn the harness began itself is taken over
    /// by the next message.
    fn thread_busy(&self, thread_id: &str) -> bool {
        self.working_turns.contains_key(thread_id) || self.session_change_pending(thread_id)
    }

    /// Whether the harness has yet to answer a change Lever asked of a
    /// thread's session, or the thread is being deleted.
    fn session_change_pending(&self, thread_id: &str) -> bool {
        self.pending_deletes.iter().any(|p| p.thread == thread_id)
            || self.pending_set_models.iter().any(|p| p.thread_id == thread_id)
            || self.modes_in_flight.iter().any(|(t, _)| t == thread_id)
    }

    /// Whether a thread is being deleted: what its harness says of it now
    /// goes nowhere.
    fn deleting(&self, thread_id: &str) -> bool {
        self.pending_deletes.iter().any(|p| p.thread == thread_id)
    }

    /// The turn the harness began itself that works on a thread, if one does.
    fn harness_turn(&self, thread_id: &str) -> Option<u64> {
        self.working_turns
            .get(thread_id)
            .filter(|active| matches!(active.dispatch, TurnDispatch::HarnessStarted { .. }))
            .map(|active| active.turn)
    }

    /// The turn the harness began on a thread, when a message may take it
    /// over: not one the person asked to stop, whose Stop must go through.
    fn harness_turn_to_take_over(&self, thread_id: &str) -> Option<u64> {
        self.harness_turn(thread_id).filter(|_| {
            self.working_turns
                .get(thread_id)
                .is_some_and(|t| t.stop_asked.is_none())
        })
    }

    /// Something that held a turn the harness began has let it go: a
    /// subagent or a request of its thread ended. The quiet it waits for
    /// counts from now, so the harness has time to answer.
    /// After a Stop, what ends is the Stop going through, not the harness
    /// going on: the clock is left as it was.
    fn heard_from(&mut self, thread_id: &str) {
        if let Some(active) = self.working_turns.get_mut(thread_id)
            && active.stop_asked.is_none()
            && let TurnDispatch::HarnessStarted { last_heard, .. } = &mut active.dispatch
        {
            *last_heard = Instant::now();
        }
    }

    /// Ends a turn the harness began as done: it went quiet, or the person's
    /// message takes it over.
    fn end_harness_turn(&mut self, thread_id: &str, turn: u64, log: &mut Log) -> Broadcast {
        let unsaved = self
            .working_turns
            .remove(thread_id)
            .is_some_and(|active| active.unsaved);
        if unsaved {
            self.end_turn(thread_id, turn, "failed", Some(UNSAVED_MESSAGE), log);
            return Broadcast {
                thread_id: thread_id.to_string(),
                message: json!({ "event": "failed", "thread": thread_id, "turn": turn, "message": UNSAVED_MESSAGE }),
            };
        }
        self.end_turn(thread_id, turn, "done", None, log);
        Broadcast {
            thread_id: thread_id.to_string(),
            message: json!({ "event": "done", "thread": thread_id, "turn": turn }),
        }
    }

    fn default_mode(&self) -> String {
        self.store
            .default_mode()
            .unwrap_or_else(|_| crate::store::STARTING_MODE.to_string())
    }

    /// Threads working (in their own turn or in a subagent), and threads with
    /// a request that waits, each sorted.
    fn current_activity(&self) -> (Vec<String>, Vec<String>) {
        let mut working: Vec<String> = self
            .working_turns
            .keys()
            .chain(self.working_subagents.values().map(|s| &s.thread_id))
            .cloned()
            .collect();
        working.sort();
        working.dedup();
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

    fn provider_models_seen(&self) -> bool {
        self.store.provider_models_seen().unwrap_or(false)
    }

    /// Whether the person has a Model source ready (free hosted Models enabled,
    /// or a harness/provider known to offer Models), without starting an idle
    /// harness.
    fn models_chosen(&self) -> bool {
        if self.harness_config.is_none() {
            return false;
        }
        if let Some(catalog) = &self.model_catalog {
            return !catalog.supported || !catalog.models.is_empty();
        }
        self.free_hosted_models() || self.provider_models_seen()
    }

    fn note_provider_models_seen(&mut self, seen: bool, log: &mut Log) {
        if self.free_hosted_models() || self.provider_models_seen() == seen {
            return;
        }
        if let Err(err) = self.store.set_provider_models_seen(seen) {
            store_failed(log, &err);
        }
    }

    fn record_catalog_choice(&mut self, catalog: &ModelCatalog, log: &mut Log) {
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
            && let Some(existing) = &self.model_catalog
            && existing.default_model.is_some()
        {
            cat.default_model = existing.default_model.clone();
        }
        // A harness that says only what changed, the Model, still offers the
        // modes it offered.
        if cat.modes.is_empty()
            && self.harness.as_ref().is_some_and(AcpHarness::has_modes)
            && let Some(existing) = &self.model_catalog
        {
            cat.modes = existing.modes.clone();
        }
        let changed = self.model_catalog.as_ref() != Some(&cat);
        self.model_catalog = Some(cat);
        changed
    }

    fn apply_model_catalog(&mut self, cat: ModelCatalog, update_default: bool, log: &mut Log, effects: &mut Effects) {
        self.record_catalog_choice(&cat, log);
        if self.update_model_catalog(cat, update_default)
            && let Some(current) = &self.model_catalog
        {
            effects.broadcasts.push(Broadcast {
                thread_id: ACTIVITY.to_string(),
                message: models_event_json(current, self.free_hosted_models()),
            });
        }
    }

    /// Timeout in milliseconds for `poll(2)`, or `-1` when Lever waits for
    /// nothing but its peers and its harness.
    pub fn poll_timeout_ms(&self) -> libc::c_int {
        let retry = (!self.unsaved_endings.is_empty()
            || !self.unsaved_subagent_endings.is_empty()
            || !self.unsaved_request_ends.is_empty())
        .then_some(UNSAVED_RETRY);
        let stop = self
            .working_turns
            .values()
            .filter_map(|t| t.stop_asked)
            .chain(self.working_subagents.values().filter_map(|s| s.stop_asked))
            .chain(self.modes_in_flight.iter().map(|(_, asked)| *asked))
            .map(|asked| self.stop_timeout.saturating_sub(asked.elapsed()))
            .min();
        let quiet = self
            .working_turns
            .iter()
            .filter_map(|(tid, active)| self.harness_turn_quiet_in(tid, active))
            .min();
        match [self.idle_remaining(), retry, stop, quiet].into_iter().flatten().min() {
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
        sys::reap_orphans(self.harness.as_ref().map(AcpHarness::pid));
        self.save_endings();
        let mut effects = Effects::default();
        // A turn the harness began that it has said nothing in since the
        // person stopped it, with nothing of it still working, has stopped:
        // the harness need not be.
        let mut stopped_quiet: Vec<(String, u64)> = self
            .working_turns
            .iter()
            .filter(|(tid, t)| {
                let TurnDispatch::HarnessStarted {
                    last_heard, open_acts, ..
                } = &t.dispatch
                else {
                    return false;
                };
                // What an older turn still does does not hold this one: a
                // subagent of it stopped on its own is given its own time.
                t.stop_asked
                    .is_some_and(|asked| asked.elapsed() >= self.stop_timeout && *last_heard <= asked)
                    && open_acts.is_empty()
                    && !self.waiting.values().any(|w| &w.thread_id == *tid && w.turn == t.turn)
                    && !self
                        .working_subagents
                        .values()
                        .any(|s| &s.thread_id == *tid && s.turn == t.turn)
            })
            .map(|(tid, t)| (tid.clone(), t.turn))
            .collect();
        stopped_quiet.sort();
        for (thread_id, turn) in stopped_quiet {
            self.working_turns.remove(&thread_id);
            self.end_turn(&thread_id, turn, "stopped", None, log);
            effects.broadcasts.push(Broadcast {
                thread_id: thread_id.clone(),
                message: json!({ "event": "stopped", "thread": thread_id, "turn": turn }),
            });
        }
        let overdue = self
            .working_turns
            .iter()
            .find(|(_, t)| t.stop_asked.is_some_and(|asked| asked.elapsed() >= self.stop_timeout))
            .map(|(tid, t)| (tid.clone(), t.turn));
        if let Some((thread_id, turn)) = overdue {
            effects.extend(self.force_stop(&thread_id, turn, log));
        }
        // A turn the harness began that has gone quiet is done.
        let mut quiet: Vec<(String, u64)> = self
            .working_turns
            .iter()
            .filter(|(tid, active)| {
                self.harness_turn_quiet_in(tid, active)
                    .is_some_and(|left| left.is_zero())
            })
            .map(|(tid, active)| (tid.clone(), active.turn))
            .collect();
        quiet.sort();
        for (thread_id, turn) in quiet {
            effects.broadcasts.push(self.end_harness_turn(&thread_id, turn, log));
        }
        self.refresh_idle_timer();
        let overdue_subagent = self
            .working_subagents
            .values()
            .any(|s| s.stop_asked.is_some_and(|asked| asked.elapsed() >= self.stop_timeout));
        if overdue_subagent {
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
                        "stopped {name} (process {pid}) and every process under it: it did not stop a subagent when asked"
                    ),
                );
            }
            effects.extend(self.after_harness_gone("The harness was stopped.", log));
        }
        // A harness that has not answered a mode change of a stopped turn's
        // in that time is ended too: until it answers, the thread is held.
        let unanswered = self
            .modes_in_flight
            .iter()
            .any(|(_, asked)| asked.elapsed() >= self.stop_timeout);
        if unanswered && let Some(harness) = self.harness.take() {
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
                    "stopped {name} (process {pid}) and every process under it: it did not answer a change of mode"
                ),
            );
            effects.extend(self.after_harness_gone("The harness was stopped.", log));
        }
        if self.harness_env_stale && !self.has_active_work() {
            self.stop_harness("after free hosted Models setting changed", log);
        } else if self.models_refresh_pending && !self.has_active_work() {
            self.models_refresh_pending = false;
            self.stop_harness("to refresh Models after sign-in", log);
            self.read_what_the_harness_offers(log);
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
        self.refresh_idle_timer();
    }

    pub fn handle_request(&mut self, peer_id: u64, is_person: bool, value: &Value, log: &mut Log) -> RequestOutcome {
        let mut outcome = self.handle_op(peer_id, is_person, value, log);
        // An answer or a Stop can leave the harness with nothing to do.
        self.refresh_idle_timer();
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
            "list" => self.handle_list(value),
            "archive" | "delete" => self.handle_lifecycle(peer_id, op, value, log),
            "send" => self.handle_send(is_person, value, log),
            "resume" => self.handle_resume(value),
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
        } else if self.models_refresh_pending && !self.has_active_work() {
            self.models_refresh_pending = false;
            self.stop_harness("to refresh Models after sign-in", log);
            self.read_what_the_harness_offers(log);
        }
        self.refresh_idle_timer();
        effects.broadcasts.extend(self.activity_changed());
        effects
    }

    pub fn shutdown(mut self, log: &mut Log) -> Result<(), String> {
        if self.harness.is_some() {
            self.stop_harness("on Lever stop", log);
        }
        // How requests and turns ended goes in first, if the database takes
        // it now. What was working ends here, and the record says so now,
        // not at the next start.
        self.save_endings();
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
                    if self.working_subagents.values().any(|s| s.thread_id == thread.id) {
                        thread.working = true;
                        thread.state = "working".to_string();
                    } else if thread.working && !self.working_turns.contains_key(&thread.id) {
                        // Every subagent of this thread has ended, though the
                        // database may not have taken its end yet.
                        if self.unsaved_subagent_endings.iter().any(|e| e.thread_id == thread.id) {
                            thread.working = false;
                            if thread.state == "working" {
                                thread.state = "done".to_string();
                            }
                        }
                    }
                    // What waits is what Lever holds: a request whose end
                    // the database has yet to take waits no longer.
                    thread.waiting = self.waiting.values().any(|w| w.thread_id == thread.id);
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

    fn handle_lifecycle(&mut self, peer_id: u64, op: &str, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(thread) = value.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "Expected a non-empty thread ID.");
        };
        let meta = match self.store.thread_meta(thread) {
            Ok(Some(meta)) => meta,
            Ok(None) => return error_outcome("no-thread", "This thread no longer exists."),
            Err(e) => return error_outcome("storage-error", &e),
        };
        if self.pending_deletes.iter().any(|p| p.thread == thread) {
            return error_outcome("working", "This thread is being deleted.");
        }
        if op == "archive" {
            let archived = match value.get("archived") {
                None => true,
                Some(Value::Bool(b)) => *b,
                _ => return error_outcome("invalid-request", "Expected archived to be a boolean."),
            };
            if let Err(e) = self.store.archive_thread(thread, archived) {
                return error_outcome("storage-error", &e);
            }
            return RequestOutcome {
                reply: Some(json!({"reply":"archived", "thread":thread, "archived":archived})),
                subscribe: None,
                broadcasts: vec![Broadcast {
                    thread_id: thread.into(),
                    message: json!({"event":"archived", "thread":thread, "archived":archived}),
                }],
            };
        }
        self.save_endings();
        if self.thread_busy(thread)
            || self.working_subagents.values().any(|s| s.thread_id == thread)
            || self.waiting.values().any(|request| request.thread_id == thread)
            || meta.working_turn.is_some()
            || self.unsaved_endings.iter().any(|p| p.thread_id == thread)
            || self.unsaved_subagent_endings.iter().any(|p| p.thread_id == thread)
            || self.pending_set_models.iter().any(|p| p.thread_id == thread)
        {
            return error_outcome(
                "working",
                "Stop this thread and wait for it to finish before deleting it.",
            );
        }
        if let Some(session) = meta.harness_session_id.filter(|s| !s.is_empty()) {
            if self.harness.is_none()
                && let Err(e) = self.start_harness(log)
            {
                return error_outcome("harness-error", &e);
            }
            self.pending_deletes.push(PendingDelete {
                peer_id,
                thread: thread.into(),
                session,
                dispatched: false,
            });
            let effects = self.dispatch_deletes();
            // An unsupported deletion finishes synchronously; otherwise the peer waits.
            return RequestOutcome {
                reply: effects.replies.into_iter().next().map(|r| r.message),
                subscribe: None,
                broadcasts: effects.broadcasts,
            };
        }
        self.delete_record(thread, None)
    }

    fn delete_record(&mut self, thread: &str, warning: Option<&str>) -> RequestOutcome {
        self.subagent_threads.remove(thread);
        self.held_stops.remove(thread);
        if let Err(e) = self.store.delete_thread(thread) {
            return error_outcome("storage-error", &e);
        }
        if let Some(h) = self.harness.as_mut() {
            h.forget_session(thread);
        }
        self.thread_commands.remove(thread);
        RequestOutcome {
            reply: Some(json!({"reply":"deleted", "thread":thread, "warning":warning})),
            subscribe: None,
            broadcasts: vec![Broadcast {
                thread_id: thread.into(),
                message: json!({"event":"deleted", "thread":thread}),
            }],
        }
    }

    fn finish_delete(&mut self, thread: &str, result: Result<(), String>, warning: Option<&str>) -> Effects {
        let Some(i) = self.pending_deletes.iter().position(|p| p.thread == thread) else {
            return Effects::default();
        };
        let pending = self.pending_deletes.remove(i);
        let outcome = match result {
            Ok(()) => self.delete_record(thread, warning),
            Err(e) => error_outcome("harness-error", &e),
        };
        self.refresh_idle_timer();
        Effects {
            broadcasts: outcome.broadcasts,
            replies: vec![PeerReply {
                peer_id: pending.peer_id,
                message: outcome.reply.unwrap(),
                subscribe: None,
            }],
        }
    }

    fn dispatch_deletes(&mut self) -> Effects {
        let mut unsupported = Vec::new();
        if let Some(h) = self.harness.as_mut().filter(|h| h.is_initialized()) {
            for pending in &mut self.pending_deletes {
                if !pending.dispatched {
                    pending.dispatched = true;
                    if !h.delete_session(&pending.thread, &pending.session) {
                        unsupported.push(pending.thread.clone());
                    }
                }
            }
        }
        let mut effects = Effects::default();
        for thread in unsupported {
            effects.extend(self.finish_delete(
                &thread,
                Ok(()),
                Some("The harness does not support session deletion; its copy may remain."),
            ));
        }
        effects
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
                for end in self.unsaved_request_ends.iter().filter(|e| e.thread_id == thread_id) {
                    for turn in &mut record.turns {
                        if let Some(request) = turn.requests.iter_mut().find(|r| r.id == end.request_id) {
                            request.state = end.state.to_string();
                            request.answer = end.answer.clone();
                        }
                    }
                }
                for end in self
                    .unsaved_subagent_endings
                    .iter()
                    .filter(|e| e.thread_id == thread_id)
                {
                    for turn in &mut record.turns {
                        if let Some(sub) = turn.subagents.iter_mut().find(|s| s.id == end.subagent_id) {
                            sub.state = end.state.to_string();
                            sub.error = end.error.clone();
                        }
                    }
                }
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
        } else if refresh && self.harness.is_some() {
            self.models_refresh_pending = true;
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

    /// Has the harness say what it offers, starting it if need be: its
    /// Models, its modes, and the commands that follow.
    fn read_what_the_harness_offers(&mut self, log: &mut Log) {
        if self.harness.is_none() && self.start_harness(log).is_err() {
            return;
        }
        self.idle_since = None;
        match self.harness.as_mut() {
            Some(harness) if harness.is_initialized() => harness.probe_models(&default_cwd()),
            _ => self.probe_wanted = true,
        }
    }

    fn handle_commands(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
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
            .and_then(|tid| self.thread_commands.get(tid))
            .or(self.commands.as_ref());
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
        let probing = self.probe_wanted || self.harness.as_ref().is_some_and(AcpHarness::probing_models);
        if known.is_none() && !self.commands_probed && !probing && !self.harness_env_stale {
            self.read_what_the_harness_offers(log);
        }
        RequestOutcome {
            reply: Some(reply),
            subscribe: None,
            broadcasts: Vec::new(),
        }
    }

    fn handle_efforts(&mut self, peer_id: u64, value: &Value, log: &mut Log) -> RequestOutcome {
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
        if let Some(levels) = self.efforts.get(model) {
            return RequestOutcome {
                reply: Some(efforts_reply_json(model, levels)),
                subscribe: None,
                broadcasts: Vec::new(),
            };
        }
        if let Some(catalog) = &self.model_catalog
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
        self.pending_efforts.push((peer_id, model.to_string()));
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
    fn check_effort(&self, model: Option<&str>, effort: &str) -> Result<(), RequestOutcome> {
        let model = model.or_else(|| self.model_catalog.as_ref().and_then(|c| c.default_model.as_deref()));
        if let Some(levels) = model.and_then(|m| self.efforts.get(m))
            && !levels.iter().any(|level| level.id == effort)
        {
            return Err(error_outcome(
                "invalid-effort",
                &format!("This thread's Model does not offer the effort {effort}."),
            ));
        }
        Ok(())
    }

    fn handle_set_effort(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
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
        self.pending_effort_threads.insert(thread_id.to_string());
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
    fn model_changes(&self, thread_id: &str, model: &str) -> Option<Broadcast> {
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
    fn save_effort(&mut self, thread_id: &str, effort: Option<&str>, log: &mut Log) -> Vec<Broadcast> {
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

    /// Keeps what the harness says of a thread's session: the effort levels
    /// of its Model, and the effort it is at. That effort is the thread's,
    /// unless the turn that waits has one of its own the Model offers, which
    /// it sets before its prompt.
    fn mirror_effort(&mut self, thread_id: &str, log: &mut Log) -> Vec<Broadcast> {
        let Some(harness) = self.harness.as_ref() else {
            return Vec::new();
        };
        let Some(session_id) = harness.session_for_thread(thread_id) else {
            return Vec::new();
        };
        let selector = harness.session_effort(session_id).cloned();
        if let Some(model) = harness.session_current_model(session_id) {
            self.efforts.insert(
                model.to_string(),
                selector.as_ref().map(|s| s.options.clone()).unwrap_or_default(),
            );
        }
        let wanted = self
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
            self.efforts.clear();
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
                if meta.working_turn.is_some() || self.thread_busy(thread_id) {
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
        let mut broadcasts: Vec<Broadcast> = self.model_changes(thread_id, model).into_iter().collect();
        if let Err(err) = self.store.set_thread_model(thread_id, model, unix_now()) {
            store_failed(log, &err);
            return error_outcome("storage-error", &err);
        }
        // An effort is a level of one Model: another Model begins at its own.
        if !broadcasts.is_empty() {
            self.pending_effort_threads.remove(thread_id);
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
        let requested_effort = match value.get("effort") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.as_str()),
            _ => return error_outcome("invalid-request", "Expected effort to be a non-empty string or null."),
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
            && (self.unsaved_endings.iter().any(|ending| ending.thread_id == tid)
                || self
                    .unsaved_subagent_endings
                    .iter()
                    .any(|ending| ending.thread_id == tid))
        {
            return error_outcome(
                "storage-error",
                "Lever is still saving how this thread's last turn ended.",
            );
        }

        let now = unix_now();
        let title = initial_title(text);
        // A turn the harness began itself is taken over by the person's
        // message: the harness takes the message into the work it is doing.
        let woken = requested_thread.and_then(|tid| self.harness_turn_to_take_over(tid));
        let mut mode_changed = false;
        let mut effort_changed = requested_effort.is_some();
        let (thread_id, cwd, existing_session_id, desired_model, desired_effort, mode, new_thread) =
            if let Some(tid) = requested_thread {
                match self.store.thread_meta(tid) {
                    Ok(Some(meta)) => {
                        let busy = if woken.is_some() {
                            self.session_change_pending(tid)
                        } else {
                            meta.working_turn.is_some() || self.thread_busy(tid)
                        };
                        if busy {
                            return error_outcome("working", "This thread is already working on a turn.");
                        }
                        mode_changed = requested_mode.is_some_and(|m| m != meta.mode);
                        effort_changed = requested_effort.is_some_and(|e| meta.effort.as_deref() != Some(e));
                        let model = requested_model
                            .map(str::to_string)
                            .or(meta.model)
                            .or_else(|| self.model_catalog.as_ref().and_then(|c| c.default_model.clone()));
                        let effort = requested_effort.map(str::to_string).or(meta.effort);
                        let mode = requested_mode.map(str::to_string).unwrap_or(meta.mode);
                        (
                            tid.to_string(),
                            meta.cwd,
                            meta.harness_session_id,
                            model,
                            effort,
                            mode,
                            false,
                        )
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
                (
                    tid,
                    cwd,
                    None,
                    requested_model.map(str::to_string),
                    requested_effort.map(str::to_string),
                    mode,
                    true,
                )
            };
        if let Some(effort) = requested_effort
            && let Err(outcome) = self.check_effort(desired_model.as_deref(), effort)
        {
            return outcome;
        }

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
        let mut broadcasts = Vec::new();
        if let (Some(tid), Some(turn)) = (requested_thread, woken) {
            broadcasts.push(self.end_harness_turn(tid, turn, log));
            // The record must have how that turn ended before the next one
            // begins, as for any turn.
            if self.unsaved_endings.iter().any(|ending| ending.thread_id == tid) {
                let mut outcome = error_outcome(
                    "storage-error",
                    "Lever is still saving how this thread's last turn ended.",
                );
                outcome.broadcasts = broadcasts;
                return outcome;
            }
        }
        let turn = match self.store.begin_turn(
            &thread_id,
            thread,
            text,
            context_json.as_deref(),
            requested_mode.filter(|_| !new_thread),
            now,
        ) {
            Ok(turn) => {
                self.held_stops.remove(&thread_id);
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
            "thread": thread_id,
            "turn": turn,
            "text": text,
        });
        if let Some(ctx) = context_value {
            msg_event["context"] = ctx;
        }
        if mode_changed {
            broadcasts.push(Broadcast {
                thread_id: thread_id.clone(),
                message: json!({ "event": "thread-updated", "thread": thread_id, "mode": mode }),
            });
        }
        // The effort a message asks for is the thread's from this turn on.
        // Where the thread's Model turns out not to offer it, the thread
        // takes the effort the harness has it at.
        if effort_changed {
            broadcasts.extend(self.save_effort(&thread_id, requested_effort, log));
        }
        broadcasts.push(Broadcast {
            thread_id: thread_id.clone(),
            message: msg_event,
        });

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
                desired_effort,
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

        // A thread that works or waits is stopped whether or not the
        // database can be read.
        let live = self.working_turns.contains_key(thread_id)
            || self.working_subagents.values().any(|s| s.thread_id == thread_id)
            || self.waiting.values().any(|w| w.thread_id == thread_id);
        if !live {
            match self.store.thread_meta(thread_id) {
                Ok(None) => return error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
                Err(err) => return error_outcome("storage-error", &err),
                Ok(Some(_)) => {}
            }
        }

        // Stop withdraws what the thread asks, whether or not a turn works:
        // a harness may ask after its turn ended. A Stop for one turn
        // withdraws only what that turn asks.
        let mut broadcasts = self.withdraw_requests(
            |w| w.thread_id == thread_id && requested_turn.is_none_or(|t| w.turn == t),
            log,
        );

        // Cancel any subagents still working on this thread (or requested turn).
        let now = Instant::now();
        let mut child_sessions_to_cancel = Vec::new();
        for sub in self.working_subagents.values_mut() {
            if sub.thread_id == thread_id && requested_turn.is_none_or(|t| sub.turn == t) {
                if sub.stop_asked.is_none() {
                    sub.stop_asked = Some(now);
                }
                child_sessions_to_cancel.push(sub.harness_session_id.clone());
            }
        }
        // A Stop that leaves nothing of the thread working holds against
        // what the harness begins on it after, until the person's next
        // message: OpenCode answers a stopped subagent's report too.
        let leaves_nothing =
            self.working_subagents
                .values()
                .all(|s| s.thread_id != thread_id || s.stop_asked.is_some())
                && self.working_turns.get(thread_id).is_none_or(|active| {
                    active.stop_asked.is_some() || requested_turn.is_none_or(|t| active.turn == t)
                });
        if leaves_nothing {
            self.held_stops.insert(thread_id.to_string());
        }
        if !child_sessions_to_cancel.is_empty()
            && let Some(harness) = self.harness.as_mut()
        {
            for child_sid in child_sessions_to_cancel {
                harness.cancel_session(&child_sid);
            }
            if !self.working_turns.contains_key(thread_id)
                && let Some(parent_sid) = harness.session_for_thread(thread_id).map(str::to_string)
            {
                harness.cancel_session(&parent_sid);
            }
        }

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
            // A turn the harness began has no prompt of Lever's to end: the
            // session is told to stop, and a harness that does not is stopped
            // after `stop_timeout`, as for a prompt it does not stop.
            TurnDispatch::PromptInFlight { session_id } | TurnDispatch::HarnessStarted { session_id, .. } => {
                let sid = session_id.clone();
                if active.stop_asked.is_none() {
                    active.stop_asked = Some(Instant::now());
                }
                if let Some(harness) = self.harness.as_mut() {
                    harness.cancel_session(&sid);
                }
                Vec::new()
            }
            TurnDispatch::WaitingForMode | TurnDispatch::WaitingForEffort => {
                // The mode, or the effort, is being set on the session, and
                // the prompt is not sent: the turn ends now. Its session is the thread's
                // still, and the thread stays busy until the harness answers,
                // so the next turn knows which mode the session is in.
                let turn = active.turn;
                self.working_turns.remove(thread_id);
                self.modes_in_flight.push((thread_id.to_string(), Instant::now()));
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
        self.save_endings();
        match self.store.thread_meta(thread_id) {
            Ok(Some(meta)) => {
                if meta.working_turn.is_some() || self.thread_busy(thread_id) {
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
        // A request Lever holds is answered whether or not the database can
        // be read; the record is asked only to say why one is not held.
        let no_request = || error_outcome("no-request", "That request no longer waits.");
        let Some(waiting) = self.waiting.get(request_id).filter(|w| w.thread_id == thread_id) else {
            return match self.store.thread_meta(thread_id) {
                Ok(Some(_)) => no_request(),
                Ok(None) => error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
                Err(err) => error_outcome("storage-error", &err),
            };
        };
        let handle = waiting.handle;
        let offered = waiting.record.answers.iter().any(|(a, _)| a == answer);
        if !offered {
            return error_outcome("invalid-answer", &format!("This request does not offer {answer}."));
        }
        let answered = self
            .harness
            .as_mut()
            .is_some_and(|h| h.answer_permission(handle, Some(answer)));
        if !answered {
            // The harness has it no longer: it is withdrawn, in the record too.
            let broadcasts = self.end_request(request_id, "withdrawn", None, log);
            return RequestOutcome {
                reply: Some(error_json("no-request", "That request no longer waits.")),
                subscribe: None,
                broadcasts,
            };
        }
        RequestOutcome {
            reply: Some(json!({ "reply": "ok" })),
            subscribe: None,
            broadcasts: self.end_request(request_id, "answered", Some(answer), log),
        }
    }

    /// Ends a request Lever holds, in the record, and tells who watches its
    /// thread. An end the database does not take is kept, said to whoever
    /// asks, and tried again on each wake.
    fn end_request(
        &mut self,
        request_id: &str,
        state: &'static str,
        answer: Option<&str>,
        log: &mut Log,
    ) -> Vec<Broadcast> {
        let Some(waiting) = self.waiting.remove(request_id) else {
            return Vec::new();
        };
        self.heard_from(&waiting.thread_id);
        let (thread_id, turn, record) = match self.store.end_request(request_id, state, answer, unix_now()) {
            Ok(Some(ended)) => ended,
            Ok(None) => return Vec::new(),
            Err(err) => {
                store_failed(log, &err);
                self.unsaved_request_ends.push(RequestEnd {
                    request_id: request_id.to_string(),
                    thread_id: waiting.thread_id.clone(),
                    state,
                    answer: answer.map(str::to_string),
                });
                let mut record = waiting.record;
                record.state = state.to_string();
                record.answer = answer.map(str::to_string);
                (waiting.thread_id, waiting.turn, record)
            }
        };
        vec![Broadcast {
            thread_id: thread_id.clone(),
            message: request_event_json(&thread_id, turn, &record),
        }]
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
            if let Some(w) = self.waiting.get(&id)
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
        self.efforts.clear();
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
        self.efforts.clear();
        self.commands = None;
        self.thread_commands.clear();
        self.commands_probed = false;
        self.pending_effort_threads.clear();
        self.probe_wanted = false;
        self.harness_env_stale = false;
        self.models_refresh_pending = false;
        self.idle_since = None;
        self.subagent_threads.clear();
        self.held_stops.clear();
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
            // thread's acts or subagents, and may still name them.
            if let Ok(ids) = self.store.harness_call_ids(thread_id) {
                harness.remember_tool_ids(thread_id, ids);
            }
            if let Ok(ids) = self.store.subagent_harness_session_ids(thread_id) {
                harness.remember_subagent_ids(thread_id, ids);
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
        // The thread's effort, where the session's Model offers it and the
        // session is at another. One the Model does not offer is not asked.
        let effort = active.desired_effort.as_deref().filter(|wanted| {
            harness
                .session_effort(&session_id)
                .is_some_and(|selector| selector.offers(wanted) && selector.current != *wanted)
        });
        if let Some(effort) = effort {
            harness.set_session_effort(thread_id, active.turn, &session_id, effort);
            active.dispatch = TurnDispatch::WaitingForEffort;
            return;
        }
        if harness.has_modes() && harness.session_current_mode(&session_id) != Some(active.mode.as_str()) {
            harness.set_session_mode(thread_id, active.turn, &session_id, &active.mode);
            active.dispatch = TurnDispatch::WaitingForMode;
        } else {
            self.pending_effort_threads.remove(thread_id);
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
            AcpEvent::SessionDeleted { thread_id, result } => self.finish_delete(&thread_id, result, None),
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
                let needs_probe = std::mem::take(&mut self.probe_wanted)
                    || !self.pending_models.is_empty()
                    || self.pending_set_models.iter().any(|p| !p.in_flight_on_session);
                if needs_probe && let Some(harness) = self.harness.as_mut() {
                    harness.probe_models(&default_cwd());
                }
                if let Some(harness) = self.harness.as_mut() {
                    for (_, model) in &self.pending_efforts {
                        harness.probe_efforts(&default_cwd(), model);
                    }
                }
                self.dispatch_deletes()
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
                        self.record_catalog_choice(&catalog, log);
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
                        self.pending_set_models = remaining;
                        self.commands_probed = true;
                        self.apply_model_catalog(catalog, true, log, &mut effects);
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
                    self.apply_model_catalog(cat, false, log, &mut effects);
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
                        if let Some(ref model) = current_model {
                            self.note_provider_models_seen(true, log);
                            let changes = self.model_changes(&thread_id, model);
                            match self.store.set_thread_model(&thread_id, model, unix_now()) {
                                Ok(()) => effects.broadcasts.extend(changes),
                                Err(err) => store_failed(log, &err),
                            }
                        }
                        effects.broadcasts.extend(self.mirror_effort(&thread_id, log));
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
                let mut effects = Effects::default();
                let display = if no_models {
                    let empty = ModelCatalog {
                        supported: true,
                        models: Vec::new(),
                        default_model: None,
                        config_id: None,
                        modes: Vec::new(),
                    };
                    self.apply_model_catalog(empty, true, log, &mut effects);
                    NO_MODEL_MESSAGE.to_string()
                } else {
                    message
                };
                effects
                    .broadcasts
                    .extend(self.fail_turn(&thread_id, turn, &display, log));
                effects
            }
            AcpEvent::ModelSet {
                thread_id,
                turn,
                model,
                catalog,
            } => {
                let mut effects = Effects::default();
                if let Some(cat) = catalog {
                    self.apply_model_catalog(cat, false, log, &mut effects);
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
                let changes = self.model_changes(&thread_id, &model);
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
                effects.broadcasts.extend(changes);
                self.pending_effort_threads.remove(&thread_id);
                if let Some(t) = turn {
                    if self.working_turns.get(&thread_id).is_some_and(|active| {
                        active.turn == t && matches!(active.dispatch, TurnDispatch::WaitingForModel)
                    }) {
                        effects.broadcasts.extend(self.mirror_effort(&thread_id, log));
                        self.continue_turn(&thread_id);
                    }
                } else if let Some(pos) = self
                    .pending_set_models
                    .iter()
                    .position(|p| p.in_flight_on_session && p.thread_id == thread_id)
                {
                    let item = self.pending_set_models.remove(pos);
                    effects.broadcasts.extend(self.mirror_effort(&thread_id, log));
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
                effort_changed,
            } => {
                let mut effects = Effects::default();
                if let Some(catalog) = catalog {
                    self.apply_model_catalog(catalog, false, log, &mut effects);
                }
                if let Some(harness) = self.harness.as_ref()
                    && let Some(session_id) = harness.session_for_thread(&thread_id)
                    && let Some(model) = harness.session_current_model(session_id)
                    && let Some(selector) = harness.session_effort(session_id)
                {
                    self.efforts.insert(model.to_string(), selector.options.clone());
                }
                // What the harness says the session is at is the thread's,
                // once Lever has set the session to what the thread asks.
                // A `set-effort` saved while idle stays pending until the next
                // turn applies it, unless the session switched Models.
                if !self.settling(&thread_id) {
                    let mut model_changed = false;
                    if let Some(model) = current_model {
                        let changes = self.model_changes(&thread_id, &model);
                        model_changed = changes.is_some();
                        match self.store.set_thread_model(&thread_id, &model, unix_now()) {
                            Ok(()) => effects.broadcasts.extend(changes),
                            Err(err) => store_failed(log, &err),
                        }
                    }
                    if model_changed {
                        self.pending_effort_threads.remove(&thread_id);
                        effects.broadcasts.extend(self.mirror_effort(&thread_id, log));
                    } else if effort_changed && !self.pending_effort_threads.contains(&thread_id) {
                        effects.broadcasts.extend(self.mirror_effort(&thread_id, log));
                    }
                }
                // The harness changed the session's mode itself: the thread
                // is in that mode now. A turn that has yet to send its prompt
                // sets its own mode on the session still, and keeps it.
                if let Some(mode) = current_mode {
                    let prompt_sent = self.working_turns.get(&thread_id).is_none_or(|a| {
                        matches!(
                            a.dispatch,
                            TurnDispatch::PromptInFlight { .. } | TurnDispatch::HarnessStarted { .. }
                        )
                    });
                    if prompt_sent {
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
                } else {
                    self.modes_in_flight.retain(|(t, _)| *t != thread_id);
                }
                Effects::default()
            }
            AcpEvent::ModeSetFailed {
                thread_id,
                turn,
                message,
            } => {
                self.modes_in_flight.retain(|(t, _)| *t != thread_id);
                Effects {
                    broadcasts: self.fail_turn(&thread_id, turn, &message, log),
                    replies: Vec::new(),
                }
            }
            AcpEvent::EffortSet { thread_id, turn } => {
                self.pending_effort_threads.remove(&thread_id);
                if self
                    .working_turns
                    .get(&thread_id)
                    .is_some_and(|a| a.turn == turn && matches!(a.dispatch, TurnDispatch::WaitingForEffort))
                {
                    self.continue_turn(&thread_id);
                } else {
                    self.modes_in_flight.retain(|(t, _)| *t != thread_id);
                }
                Effects::default()
            }
            AcpEvent::EffortSetFailed {
                thread_id,
                turn,
                message,
            } => {
                self.pending_effort_threads.remove(&thread_id);
                self.modes_in_flight.retain(|(t, _)| *t != thread_id);
                let mut broadcasts = self.fail_turn(&thread_id, turn, &message, log);
                // The thread is at the effort the harness has it at, not the
                // one it refused, so the next turn does not ask for it again.
                broadcasts.extend(self.mirror_effort(&thread_id, log));
                Effects {
                    broadcasts,
                    replies: Vec::new(),
                }
            }
            AcpEvent::EffortsProbed { model, result } => {
                let mut effects = Effects::default();
                let waiting: Vec<u64> = self
                    .pending_efforts
                    .iter()
                    .filter(|(_, wanted)| *wanted == model)
                    .map(|(peer_id, _)| *peer_id)
                    .collect();
                self.pending_efforts.retain(|(_, wanted)| *wanted != model);
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
                        self.efforts.insert(model, levels);
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
            AcpEvent::Commands { thread_id, commands } => {
                let mut effects = Effects::default();
                match thread_id {
                    Some(thread_id) => {
                        if self.thread_commands.get(&thread_id) != Some(&commands) {
                            effects.broadcasts.push(Broadcast {
                                thread_id: thread_id.clone(),
                                message: json!({
                                    "event": "commands",
                                    "thread": thread_id,
                                    "commands": commands_json(&commands),
                                }),
                            });
                            self.thread_commands.insert(thread_id, commands);
                        }
                    }
                    None => {
                        if self.commands.as_ref() != Some(&commands) {
                            effects.broadcasts.push(Broadcast {
                                thread_id: ACTIVITY.to_string(),
                                message: json!({ "event": "commands", "commands": commands_json(&commands) }),
                            });
                            self.commands = Some(commands);
                        }
                    }
                }
                effects
            }
            AcpEvent::PermissionRequested {
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
            } => self.record_request(
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
            ),
            AcpEvent::PermissionWithdrawn { handle } => {
                let id = self
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
            AcpEvent::SubagentUpdated {
                thread_id,
                harness_subagent_id,
                title,
                state,
                error,
            } => self.handle_subagent_updated(
                &thread_id,
                &harness_subagent_id,
                title.as_deref(),
                state.as_deref(),
                error.as_deref(),
                log,
            ),
            AcpEvent::SubagentMessage {
                thread_id,
                harness_subagent_id,
                title,
                role,
                text,
            } => {
                let mut broadcasts = Vec::new();
                let Some((turn, sub)) =
                    self.ensure_subagent(&thread_id, &harness_subagent_id, title.as_deref(), &mut broadcasts, log)
                else {
                    return Effects::default();
                };
                match self
                    .store
                    .append_subagent_message(&thread_id, &sub.id, &role, &text, unix_now())
                {
                    Ok(true) => {
                        broadcasts.push(Broadcast {
                            thread_id: thread_id.clone(),
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
                    Err(err) => self.could_not_save(&thread_id, &err, log),
                }
                Effects {
                    broadcasts,
                    replies: Vec::new(),
                }
            }
            AcpEvent::SubagentAct {
                thread_id,
                harness_subagent_id,
                subagent_title,
                harness_call_id,
                kind,
                title,
                state,
                detail,
            } => {
                let mut broadcasts = Vec::new();
                let Some((turn, sub)) = self.ensure_subagent(
                    &thread_id,
                    &harness_subagent_id,
                    subagent_title.as_deref(),
                    &mut broadcasts,
                    log,
                ) else {
                    return Effects::default();
                };
                let patch = SubagentActPatch {
                    subagent_id: &sub.id,
                    thread_id: &thread_id,
                    harness_call_id: &harness_call_id,
                    kind: kind.as_deref(),
                    title: title.as_deref(),
                    state: state.as_deref(),
                    detail: detail.as_deref(),
                };
                match self.store.upsert_subagent_act(&patch, unix_now()) {
                    Ok(act) => {
                        broadcasts.push(Broadcast {
                            thread_id: thread_id.clone(),
                            message: subagent_act_event_json(&thread_id, turn, &sub.id, &act),
                        });
                    }
                    Err(err) => self.could_not_save(&thread_id, &err, log),
                }
                Effects {
                    broadcasts,
                    replies: Vec::new(),
                }
            }
            AcpEvent::TextChunk { thread_id, text } => {
                let mut broadcasts = Vec::new();
                let Some(turn) = self.turn_for_session_update(&thread_id, true, &mut broadcasts, log) else {
                    return Effects::default();
                };
                // The answer still goes to who is watching. The turn's end
                // says that not all of it was saved.
                if let Err(err) = self.store.append_answer(&thread_id, turn, &text, unix_now()) {
                    self.could_not_save(&thread_id, &err, log);
                }
                broadcasts.push(Broadcast {
                    thread_id: thread_id.clone(),
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
            AcpEvent::Act {
                thread_id,
                harness_call_id,
                is_new,
                kind,
                title,
                state,
                detail,
            } => {
                // A new act wakes a thread whose subagents ran; an update of
                // an act goes to the turn the act is in.
                let mut broadcasts = Vec::new();
                let Some(turn) = self.turn_for_session_update(&thread_id, is_new, &mut broadcasts, log) else {
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
                        return Effects {
                            broadcasts,
                            replies: Vec::new(),
                        };
                    }
                };
                self.note_harness_act(&thread_id, recorded_turn, &act.id, &act.state);
                broadcasts.push(Broadcast {
                    thread_id: thread_id.clone(),
                    message: act_event_json(&thread_id, recorded_turn, &act),
                });
                Effects {
                    broadcasts,
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
        for pending in self.pending_deletes.drain(..) {
            replies.push(PeerReply {
                peer_id: pending.peer_id,
                message: error_json("harness-error", message),
                subscribe: None,
            });
        }
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
        for (peer_id, _) in self.pending_efforts.drain(..) {
            replies.push(PeerReply {
                peer_id,
                message: error_json("harness-error", message),
                subscribe: None,
            });
        }
        replies
    }

    fn track_working_subagent(
        &mut self,
        thread_id: &str,
        recorded_turn: u64,
        harness_subagent_id: &str,
        record: &SubagentRecord,
        stop_asked: Option<Instant>,
    ) -> bool {
        let is_new = !self.working_subagents.contains_key(&record.id);
        let title_changed = self
            .working_subagents
            .get(&record.id)
            .is_some_and(|existing| existing.title != record.title);
        // A subagent first heard of after the person stopped the work it
        // belongs to is stopped too: the harness may say of a subagent only
        // after its turn has ended.
        let stop_asked = stop_asked.or_else(|| {
            is_new
                .then(|| self.stop_for_new_subagent(thread_id, harness_subagent_id, recorded_turn))
                .flatten()
        });
        if is_new
            && stop_asked.is_some()
            && let Some(harness) = self.harness.as_mut()
        {
            harness.cancel_session(harness_subagent_id);
        }
        self.idle_since = None;
        self.subagent_threads.insert(thread_id.to_string());
        self.working_subagents.insert(
            record.id.clone(),
            WorkingSubagent {
                thread_id: thread_id.to_string(),
                turn: recorded_turn,
                subagent_id: record.id.clone(),
                harness_session_id: harness_subagent_id.to_string(),
                title: record.title.clone(),
                started: record.started,
                stop_asked,
            },
        );
        is_new || title_changed
    }

    /// When a subagent Lever first hears of belongs to work the person asked
    /// to stop: the thread's turn it is recorded in was asked to stop, or
    /// has ended stopped and is the thread's newest.
    fn stop_for_new_subagent(&self, thread_id: &str, harness_subagent_id: &str, recorded_turn: u64) -> Option<Instant> {
        // A nested subagent is stopped as the subagent that launched it is.
        if let Some(launcher) = self.launcher(thread_id, harness_subagent_id) {
            return launcher.stop_asked;
        }
        // A Stop that holds stops what the harness starts after it, though
        // it starts it before it says anything.
        if self.held_stops.contains(thread_id) {
            return Some(Instant::now());
        }
        if let Some(active) = self.working_turns.get(thread_id) {
            return active.stop_asked.filter(|_| active.turn == recorded_turn);
        }
        match self.store.latest_turn(thread_id) {
            Ok(Some((turn, state, _))) if turn == recorded_turn && state == "stopped" => Some(Instant::now()),
            _ => None,
        }
    }

    /// Ensures a subagent row exists in the store for `(thread_id, harness_subagent_id)`.
    /// If this creates or marks a working subagent for the first time, tracks it
    /// in `working_subagents` and pushes a `subagent` event to `broadcasts`.
    fn ensure_subagent(
        &mut self,
        thread_id: &str,
        harness_subagent_id: &str,
        title: Option<&str>,
        broadcasts: &mut Vec<Broadcast>,
        log: &mut Log,
    ) -> Option<(u64, SubagentRecord)> {
        if self.deleting(thread_id) {
            return None;
        }
        let turn = self.turn_for_subagent(thread_id, harness_subagent_id)?;
        let patch = SubagentPatch {
            thread_id,
            turn_number: turn,
            harness_session_id: harness_subagent_id,
            title,
            state: None,
            error: None,
        };
        let (recorded_turn, mut record) = match self.store.upsert_subagent(&patch, unix_now()) {
            Ok(saved) => saved,
            Err(err) => {
                self.could_not_save(thread_id, &err, log);
                return None;
            }
        };
        self.with_unsaved_end(&mut record);
        if record.state == "working" {
            let prev_stop = self
                .working_subagents
                .get(&record.id)
                .and_then(|existing| existing.stop_asked);
            if self.track_working_subagent(thread_id, recorded_turn, harness_subagent_id, &record, prev_stop) {
                broadcasts.push(Broadcast {
                    thread_id: thread_id.to_string(),
                    message: subagent_event_json(thread_id, recorded_turn, &record),
                });
            }
        }
        Some((recorded_turn, record))
    }

    fn handle_subagent_updated(
        &mut self,
        thread_id: &str,
        harness_subagent_id: &str,
        title: Option<&str>,
        state: Option<&str>,
        error: Option<&str>,
        log: &mut Log,
    ) -> Effects {
        if self.deleting(thread_id) {
            return Effects::default();
        }
        let Some(turn) = self.turn_for_subagent(thread_id, harness_subagent_id) else {
            return Effects::default();
        };
        let existing_id = self
            .working_subagents
            .values()
            .find(|s| s.thread_id == thread_id && s.harness_session_id == harness_subagent_id)
            .map(|s| s.subagent_id.clone());
        // A subagent Lever knows keeps its own Stop: a Stop of a newer turn
        // was not for it. One it has yet to hear of is of the turn it joins.
        let stop_asked = match existing_id.as_deref().and_then(|id| self.working_subagents.get(id)) {
            Some(sub) => sub.stop_asked,
            None => self.stop_for_new_subagent(thread_id, harness_subagent_id, turn),
        };
        let effective_state = match state {
            Some("interrupted") if stop_asked.is_some() => Some("stopped"),
            other => other,
        };
        let now = unix_now();
        let patch = SubagentPatch {
            thread_id,
            turn_number: turn,
            harness_session_id: harness_subagent_id,
            title,
            state: effective_state,
            error,
        };
        // What the record said before, so an update that changes nothing,
        // as a harness that says a subagent ended twice sends, says nothing.
        let before = self
            .store
            .subagent_summary(thread_id, harness_subagent_id)
            .ok()
            .flatten();
        let mut broadcasts = Vec::new();
        let (recorded_turn, mut record) = match self.store.upsert_subagent(&patch, now) {
            Ok(saved) => saved,
            Err(err) => {
                self.could_not_save(thread_id, &err, log);
                // On a terminal state, still release the in-memory subagent,
                // withdraw any requests waiting on it, and queue the ending
                // for retry so the thread and harness do not wedge.
                if let Some(terminal) = match effective_state {
                    Some("done") => Some("done"),
                    Some("stopped") => Some("stopped"),
                    Some("interrupted") => Some("interrupted"),
                    Some("failed") => Some("failed"),
                    _ => None,
                } && let Some(sub_id) = existing_id
                    && let Some(active) = self.working_subagents.remove(&sub_id)
                {
                    let err_owned = error.map(str::to_string);
                    self.unsaved_subagent_endings.push(SubagentEnding {
                        thread_id: active.thread_id.clone(),
                        subagent_id: active.subagent_id.clone(),
                        state: terminal,
                        error: err_owned.clone(),
                    });
                    broadcasts.extend(self.withdraw_requests(
                        |w| {
                            w.thread_id == thread_id
                                && w.record.subagent.as_deref() == Some(active.subagent_id.as_str())
                        },
                        log,
                    ));
                    let synthetic = SubagentRecord {
                        id: active.subagent_id,
                        title: title
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .unwrap_or(active.title),
                        state: terminal.to_string(),
                        error: err_owned,
                        started: active.started,
                        ended: Some(now),
                        messages: Vec::new(),
                        acts: Vec::new(),
                    };
                    broadcasts.push(Broadcast {
                        thread_id: thread_id.to_string(),
                        message: subagent_event_json(thread_id, active.turn, &synthetic),
                    });
                }
                return Effects {
                    broadcasts,
                    replies: Vec::new(),
                };
            }
        };
        // An end the database has yet to take is how the subagent ended,
        // whatever the record still says.
        let before = if self.with_unsaved_end(&mut record) {
            before.map(|(title, _, _)| (title, record.state.clone(), record.error.clone()))
        } else {
            before
        };
        let tracked_changed = if record.state == "working" {
            self.track_working_subagent(thread_id, recorded_turn, harness_subagent_id, &record, stop_asked)
        } else {
            let was_working = self.working_subagents.remove(&record.id).is_some();
            if was_working {
                self.heard_from(thread_id);
            }
            broadcasts.extend(self.withdraw_requests(
                |w| w.thread_id == thread_id && w.record.subagent.as_deref() == Some(record.id.as_str()),
                log,
            ));
            was_working
        };
        let unchanged = before.is_some_and(|(title, state, error)| {
            title == record.title && state == record.state && error == record.error
        });
        if tracked_changed || !unchanged {
            broadcasts.push(Broadcast {
                thread_id: thread_id.to_string(),
                message: subagent_event_json(thread_id, recorded_turn, &record),
            });
        }
        Effects {
            broadcasts,
            replies: Vec::new(),
        }
    }

    /// Keeps a request the harness asked, and tells who watches its thread.
    /// A request the database does not take is answered `cancelled`: what the
    /// record cannot hold, nobody could answer after a restart either.
    #[allow(clippy::too_many_arguments)]
    fn record_request(
        &mut self,
        thread_id: &str,
        harness_subagent_id: Option<&str>,
        subagent_title: Option<&str>,
        handle: u64,
        harness_call_id: &str,
        kind: Option<&str>,
        title: &str,
        detail: Option<&str>,
        path: Option<&str>,
        answers: &[(String, String)],
        log: &mut Log,
    ) -> Effects {
        let mut effects = Effects::default();
        // A turn or subagent the person asked to stop asks nothing more of
        // them, nor does a thread being deleted. A Stop of one turn leaves a
        // subagent of an older turn to ask; one Lever has yet to hear of is
        // stopped with the turn it joins, below.
        let stopped_turn = self
            .working_turns
            .get(thread_id)
            .filter(|active| active.stop_asked.is_some())
            .map(|active| active.turn);
        let stopping = self.deleting(thread_id)
            || match harness_subagent_id {
                None => stopped_turn.is_some(),
                Some(cid) => self
                    .working_subagents
                    .values()
                    .find(|s| s.thread_id == thread_id && s.harness_session_id == cid)
                    .is_some_and(|s| s.stop_asked.is_some() || stopped_turn == Some(s.turn)),
            };
        // What the thread's own session asks wakes a thread whose subagents ran.
        let turn = if stopping {
            None
        } else if harness_subagent_id.is_none() {
            self.turn_for_session_update(thread_id, true, &mut effects.broadcasts, log)
        } else {
            self.turn_for_update(thread_id)
        };
        let Some(turn) = turn else {
            if let Some(harness) = self.harness.as_mut() {
                harness.answer_permission(handle, None);
            }
            return effects;
        };
        // A request that begins a turn under a Stop that holds asks nothing
        // of the person either.
        if harness_subagent_id.is_none()
            && self
                .working_turns
                .get(thread_id)
                .is_some_and(|active| active.turn == turn && active.stop_asked.is_some())
        {
            if let Some(harness) = self.harness.as_mut() {
                harness.answer_permission(handle, None);
            }
            return effects;
        }
        let (recorded_turn, subagent_id, act) = if let Some(child_sid) = harness_subagent_id {
            let Some((sub_turn, sub)) =
                self.ensure_subagent(thread_id, child_sid, subagent_title, &mut effects.broadcasts, log)
            else {
                if let Some(harness) = self.harness.as_mut() {
                    harness.answer_permission(handle, None);
                }
                return effects;
            };
            // A subagent first heard of in what it asks, after the person
            // stopped its work, was just stopped: it asks nothing of them.
            // Nor does one that has ended.
            if sub.state != "working"
                || self
                    .working_subagents
                    .get(&sub.id)
                    .is_some_and(|s| s.stop_asked.is_some())
            {
                if let Some(harness) = self.harness.as_mut() {
                    harness.answer_permission(handle, None);
                }
                return effects;
            }
            let patch = SubagentActPatch {
                subagent_id: &sub.id,
                thread_id,
                harness_call_id,
                kind,
                title: Some(title),
                state: None,
                detail: None,
            };
            let act = match self.store.upsert_subagent_act(&patch, unix_now()) {
                Ok(act) => {
                    effects.broadcasts.push(Broadcast {
                        thread_id: thread_id.to_string(),
                        message: subagent_act_event_json(thread_id, sub_turn, &sub.id, &act),
                    });
                    Some(act.id)
                }
                Err(err) => {
                    self.could_not_save(thread_id, &err, log);
                    None
                }
            };
            (sub_turn, Some(sub.id), act)
        } else {
            // The act it asks about, so the request and the act are one thing in
            // the thread.
            let patch = ActPatch {
                thread_id,
                turn_number: turn,
                harness_call_id,
                is_new: false,
                // A request that does not say what kind of act it is leaves the
                // kind the act has.
                kind,
                title: Some(title),
                state: None,
                detail: None,
            };
            let act = match self.store.upsert_act(&patch, unix_now()) {
                Ok((act_turn, act)) => {
                    // An act a request is the first word of is open as one
                    // the harness told of first is.
                    self.note_harness_act(thread_id, act_turn, &act.id, &act.state);
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
            (turn, None, act)
        };
        let request = NewRequest {
            thread_id,
            turn_number: recorded_turn,
            act: act.as_deref(),
            subagent: subagent_id.as_deref(),
            kind: kind.unwrap_or("other"),
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
                        turn: recorded_turn,
                        record: record.clone(),
                    },
                );
                effects.broadcasts.push(Broadcast {
                    thread_id: thread_id.to_string(),
                    message: request_event_json(thread_id, recorded_turn, &record),
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
            // A turn or subagent the person asked to stop ends with everything
            // it started, though the harness ended before it said so.
            if self.working_turns.values().any(|t| t.stop_asked.is_some())
                || self.working_subagents.values().any(|s| s.stop_asked.is_some())
            {
                harness.kill_tree();
            } else {
                harness.stop();
            }
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
    /// request it asked is withdrawn, and every turn or subagent it was
    /// working on is interrupted.
    fn after_harness_gone(&mut self, why: &str, log: &mut Log) -> Effects {
        self.model_catalog = None;
        self.subagent_threads.clear();
        self.held_stops.clear();
        self.efforts.clear();
        self.commands = None;
        self.thread_commands.clear();
        self.commands_probed = false;
        self.pending_effort_threads.clear();
        self.probe_wanted = false;
        self.harness_env_stale = false;
        self.models_refresh_pending = false;
        self.idle_since = None;

        let replies = self.fail_pending_peer_requests(why);
        self.modes_in_flight.clear();
        let mut broadcasts = self.withdraw_requests(|_| true, log);

        // Every working subagent is interrupted, or stopped when the person
        // had asked to stop it.
        let now = unix_now();
        let mut ended_subs: Vec<WorkingSubagent> = self.working_subagents.drain().map(|(_, s)| s).collect();
        ended_subs.sort_by(|a, b| a.thread_id.cmp(&b.thread_id).then(a.subagent_id.cmp(&b.subagent_id)));
        for sub in ended_subs {
            let state = if sub.stop_asked.is_some() {
                "stopped"
            } else {
                "interrupted"
            };
            self.end_subagent(&sub.thread_id, &sub.subagent_id, state, None, log);
            let record = SubagentRecord {
                id: sub.subagent_id,
                title: sub.title,
                state: state.to_string(),
                error: None,
                started: sub.started,
                ended: Some(now),
                messages: Vec::new(),
                acts: Vec::new(),
            };
            broadcasts.push(Broadcast {
                thread_id: sub.thread_id.clone(),
                message: subagent_event_json(&sub.thread_id, sub.turn, &record),
            });
        }

        // Every turn that was working is interrupted, or stopped when the
        // person had asked to stop it, and each end goes the way every other
        // end does, so one the database does not take now is tried again.
        let mut ended: Vec<(String, u64, &'static str)> = self
            .working_turns
            .drain()
            .map(|(thread_id, active)| {
                let state = if active.stop_asked.is_some() {
                    "stopped"
                } else {
                    "interrupted"
                };
                (thread_id, active.turn, state)
            })
            .collect();
        ended.sort();
        for (thread_id, turn, state) in ended {
            self.end_turn(&thread_id, turn, state, None, log);
            broadcasts.push(Broadcast {
                thread_id: thread_id.clone(),
                message: json!({
                    "event": state,
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

    /// A turn the harness began is not quiet while an act of it works.
    fn note_harness_act(&mut self, thread_id: &str, turn: u64, act_id: &str, state: &str) {
        if let Some(active) = self.working_turns.get_mut(thread_id)
            && active.turn == turn
            && let TurnDispatch::HarnessStarted { open_acts, .. } = &mut active.dispatch
        {
            if matches!(state, "pending" | "working") {
                open_acts.insert(act_id.to_string());
            } else {
                open_acts.remove(act_id);
            }
        }
    }

    /// Gives a subagent's record the end the database has yet to take, if it
    /// has one, so a late word of the subagent does not bring it back to
    /// work. Says whether it had one.
    fn with_unsaved_end(&self, record: &mut SubagentRecord) -> bool {
        let Some(end) = self
            .unsaved_subagent_endings
            .iter()
            .find(|e| e.subagent_id == record.id)
        else {
            return false;
        };
        record.state = end.state.to_string();
        record.error = end.error.clone();
        true
    }

    fn end_subagent(
        &mut self,
        thread_id: &str,
        subagent_id: &str,
        state: &'static str,
        error: Option<&str>,
        log: &mut Log,
    ) {
        if let Err(err) = self
            .store
            .finish_subagent(thread_id, subagent_id, state, error, unix_now())
        {
            store_failed(log, &err);
            self.unsaved_subagent_endings.push(SubagentEnding {
                thread_id: thread_id.to_string(),
                subagent_id: subagent_id.to_string(),
                state,
                error: error.map(str::to_string),
            });
        }
    }

    fn save_endings(&mut self) {
        let store = &self.store;
        let now = unix_now();
        self.unsaved_request_ends.retain(|end| {
            store
                .end_request(&end.request_id, end.state, end.answer.as_deref(), now)
                .is_err()
        });
        self.unsaved_subagent_endings.retain(|ending| {
            store
                .finish_subagent(
                    &ending.thread_id,
                    &ending.subagent_id,
                    ending.state,
                    ending.error.as_deref(),
                    now,
                )
                .is_err()
        });
        if self.unsaved_endings.is_empty() {
            return;
        }
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

    /// The turn that what a thread's own session says goes to. A thread whose
    /// subagents ran in this harness and that has no turn working is woken
    /// by it, when `opens`: the harness began a turn of its own, as OpenCode
    /// does when a subagent reports back. That turn is the thread's newest
    /// when the harness began the newest and Lever ended it as quiet;
    /// otherwise it is a new one. Each heard from keeps its turn working.
    fn turn_for_session_update(
        &mut self,
        thread_id: &str,
        opens: bool,
        broadcasts: &mut Vec<Broadcast>,
        log: &mut Log,
    ) -> Option<u64> {
        if let Some(active) = self.working_turns.get_mut(thread_id) {
            // After a Stop, only what goes on counts as the harness going on:
            // an act it closes is the Stop going through.
            let goes_on = opens || active.stop_asked.is_none();
            if let TurnDispatch::HarnessStarted { last_heard, .. } = &mut active.dispatch
                && goes_on
            {
                *last_heard = Instant::now();
            }
            return Some(active.turn);
        }
        let session_id = self
            .harness
            .as_ref()
            .and_then(|h| h.session_for_thread(thread_id))
            .map(str::to_string);
        // A thread being deleted, or whose last end is still to be saved,
        // begins nothing.
        let settled = !self.pending_deletes.iter().any(|p| p.thread == thread_id)
            && !self.unsaved_endings.iter().any(|e| e.thread_id == thread_id);
        let (true, Some(session_id)) = (
            opens && settled && self.subagent_threads.contains(thread_id),
            session_id,
        ) else {
            return self.turn_for_update(thread_id);
        };
        let meta = match self.store.thread_meta(thread_id) {
            Ok(Some(meta)) => meta,
            Ok(None) => return None,
            Err(err) => {
                store_failed(log, &err);
                return self.turn_for_update(thread_id);
            }
        };
        let now = unix_now();
        let reopened = match self.store.latest_turn(thread_id) {
            Ok(Some((turn, state, true))) if state == "done" => {
                match self.store.reopen_harness_turn(thread_id, turn, now) {
                    Ok(true) => Some(turn),
                    Ok(false) => None,
                    Err(err) => {
                        store_failed(log, &err);
                        None
                    }
                }
            }
            _ => None,
        };
        let turn = match reopened {
            Some(turn) => turn,
            None => match self.store.begin_harness_turn(thread_id, now) {
                Ok(turn) => turn,
                Err(err) => {
                    // What the harness says still goes to the newest turn.
                    store_failed(log, &err);
                    return self.turn_for_update(thread_id);
                }
            },
        };
        self.idle_since = None;
        let held = self.held_stops.contains(thread_id);
        if held && let Some(harness) = self.harness.as_mut() {
            harness.cancel_session(&session_id);
        }
        self.working_turns.insert(
            thread_id.to_string(),
            WorkingTurn {
                turn,
                message: String::new(),
                context_prompt: None,
                cwd: meta.cwd,
                existing_session_id: meta.harness_session_id,
                desired_model: None,
                desired_effort: None,
                mode: meta.mode,
                dispatch: TurnDispatch::HarnessStarted {
                    session_id,
                    last_heard: Instant::now(),
                    open_acts: HashSet::new(),
                },
                unsaved: false,
                // Asked after it was last heard from, so a turn that says no
                // more once told to stop ends stopped and spares the harness.
                stop_asked: held.then(Instant::now),
            },
        );
        broadcasts.push(Broadcast {
            thread_id: thread_id.to_string(),
            message: json!({
                "event": "message",
                "thread": thread_id,
                "turn": turn,
                "text": "",
                "origin": "harness",
            }),
        });
        Some(turn)
    }

    /// When a turn the harness began may be taken as done: it has been quiet
    /// for `wake_quiet`, and none of its acts works, no request of its thread
    /// waits, and no subagent of its thread works. `None` while one of those
    /// still holds it.
    fn harness_turn_quiet_in(&self, thread_id: &str, active: &WorkingTurn) -> Option<Duration> {
        let TurnDispatch::HarnessStarted {
            last_heard, open_acts, ..
        } = &active.dispatch
        else {
            return None;
        };
        if active.stop_asked.is_some()
            || !open_acts.is_empty()
            || self.waiting.values().any(|w| w.thread_id == thread_id)
            || self.working_subagents.values().any(|s| s.thread_id == thread_id)
        {
            return None;
        }
        Some(self.wake_quiet.saturating_sub(last_heard.elapsed()))
    }

    /// The working subagent of a thread that launched a nested one.
    fn launcher(&self, thread_id: &str, harness_subagent_id: &str) -> Option<&WorkingSubagent> {
        let launcher_sid = self.harness.as_ref()?.launcher_of(harness_subagent_id)?;
        self.working_subagents
            .values()
            .find(|s| s.thread_id == thread_id && s.harness_session_id == launcher_sid)
    }

    /// The turn a subagent's update belongs to: a nested subagent's is the
    /// turn of the subagent that launched it, whatever turn works now.
    fn turn_for_subagent(&self, thread_id: &str, harness_subagent_id: &str) -> Option<u64> {
        self.launcher(thread_id, harness_subagent_id)
            .map(|s| s.turn)
            .or_else(|| self.turn_for_update(thread_id))
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

fn models_payload_json(kind_key: &str, catalog: &ModelCatalog, free_hosted_models: bool) -> Value {
    let models: Vec<Value> = catalog
        .models
        .iter()
        .map(|m| {
            let mut model = json!({ "id": m.id, "name": m.name });
            if let Some((id, name)) = &m.provider {
                model["provider"] = json!(id);
                model["provider_name"] = json!(name);
            }
            model
        })
        .collect();
    let mut obj = json!({
        "supported": catalog.supported,
        "models": models,
        "default": catalog.default_model,
        "free_hosted_models": free_hosted_models,
        "modes": catalog
            .modes
            .iter()
            .map(|m| json!({ "id": m, "name": acp::mode_name(m).unwrap_or(m) }))
            .collect::<Vec<_>>(),
    });
    obj[kind_key] = json!("models");
    obj
}

fn models_reply_json(catalog: &ModelCatalog, free_hosted_models: bool) -> Value {
    models_payload_json("reply", catalog, free_hosted_models)
}

fn models_event_json(catalog: &ModelCatalog, free_hosted_models: bool) -> Value {
    models_payload_json("event", catalog, free_hosted_models)
}

fn efforts_reply_json(model: &str, levels: &[EffortOption]) -> Value {
    json!({
        "reply": "efforts",
        "model": model,
        "efforts": levels
            .iter()
            .map(|level| json!({ "id": level.id, "name": level.name }))
            .collect::<Vec<_>>(),
    })
}

fn commands_json(commands: &[HarnessCommand]) -> Vec<Value> {
    commands
        .iter()
        .map(|command| {
            let mut entry = json!({ "name": command.name, "description": command.description });
            if let Some(hint) = &command.hint {
                entry["hint"] = json!(hint);
            }
            entry
        })
        .collect()
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
        "archived": t.archived,
        "subagents": t.subagents,
    });
    if let Some(model) = &t.model {
        obj["model"] = json!(model);
    }
    if let Some(effort) = &t.effort {
        obj["effort"] = json!(effort);
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
    if let Some(effort) = &r.effort {
        obj["effort"] = json!(effort);
    }
    obj
}

fn turn_record_json(t: &TurnRecord) -> Value {
    let mut obj = json!({
        "turn": t.turn,
        "message": t.message,
        "answer": t.answer,
        "state": t.state,
        "started": t.started,
        "acts": t.acts.iter().map(act_record_json).collect::<Vec<_>>(),
        "requests": t.requests.iter().map(request_record_json).collect::<Vec<_>>(),
        "notes": t.notes.iter().map(note_record_json).collect::<Vec<_>>(),
        "subagents": t.subagents.iter().map(subagent_record_json).collect::<Vec<_>>(),
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
    if let Some(ended) = t.ended {
        obj["ended"] = json!(ended);
    }
    if let Some(origin) = &t.origin {
        obj["origin"] = json!(origin);
    }
    obj
}

fn subagent_record_json(s: &SubagentRecord) -> Value {
    let mut obj = json!({
        "subagent": s.id,
        "title": s.title,
        "state": s.state,
        "started": s.started,
        "messages": s
            .messages
            .iter()
            .map(|m| json!({ "role": m.role, "text": m.text }))
            .collect::<Vec<_>>(),
        "acts": s.acts.iter().map(act_record_json).collect::<Vec<_>>(),
    });
    if let Some(err) = &s.error {
        obj["error"] = json!(err);
    }
    if let Some(ended) = s.ended {
        obj["ended"] = json!(ended);
    }
    obj
}

fn subagent_event_json(thread_id: &str, turn: u64, s: &SubagentRecord) -> Value {
    let mut obj = json!({
        "event": "subagent",
        "thread": thread_id,
        "turn": turn,
        "subagent": s.id,
        "title": s.title,
        "state": s.state,
        "started": s.started,
    });
    if let Some(err) = &s.error {
        obj["error"] = json!(err);
    }
    if let Some(ended) = s.ended {
        obj["ended"] = json!(ended);
    }
    obj
}

fn subagent_act_event_json(thread_id: &str, turn: u64, subagent_id: &str, a: &ActRecord) -> Value {
    let mut obj = act_record_json(a);
    obj["event"] = json!("subagent-act");
    obj["thread"] = json!(thread_id);
    obj["turn"] = json!(turn);
    obj["subagent"] = json!(subagent_id);
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
        ("subagent", &r.subagent),
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
