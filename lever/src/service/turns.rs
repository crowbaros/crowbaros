//! Active turn state, dispatch, Stop handling, update routing, and activity tracking.

use super::Service;
use super::catalog::PendingSetModel;
use super::wire::{
    ACTIVITY, Broadcast, Effects, NOT_SENT_MESSAGE, PeerReply, RequestOutcome, UNSAVED_MESSAGE, error_json,
    error_outcome, store_failed, unix_now,
};
use crate::log::Log;
use crate::store::{QueuedRecord, RequestRecord, ThreadMeta};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

pub(super) enum TurnDispatch {
    WaitingForInit,
    WaitingForSession,
    WaitingForModel,
    WaitingForEffort,
    WaitingForMode,
    /// The prompt is ready, and waits while a subagent of the thread asks:
    /// harnesses with `holds_prompt_during_subagent_request` (e.g. OpenCode
    /// 2.4.1) strand a subagent's request on a new prompt.
    WaitingForSubagentAnswer,
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

pub(super) struct WorkingTurn {
    pub(super) turn: u64,
    pub(super) message: String,
    pub(super) context_prompt: Option<String>,
    /// Bounded prior thread history handed to a fresh session when the thread
    /// moved to a new project folder. Kept apart from `context_prompt` so it
    /// is never recorded as desktop context.
    pub(super) handoff_prompt: Option<String>,
    /// True while this turn carries a pending `<thread-history>` handoff that
    /// the harness has not yet acknowledged; cleared in SQLite only once the
    /// harness answers or finishes the prompt so an early crash retries it.
    pub(super) handoff_pending: bool,
    pub(super) cwd: String,
    pub(super) existing_session_id: Option<String>,
    pub(super) desired_model: Option<String>,
    /// The thread's effort, which the session is set to before the prompt
    /// when its Model offers it.
    pub(super) desired_effort: Option<String>,
    /// The thread's mode, which the session is set to before the prompt.
    pub(super) mode: String,
    pub(super) dispatch: TurnDispatch,
    /// Something of this turn did not reach the database.
    pub(super) unsaved: bool,
    /// When the person asked to stop this turn and the harness was told.
    pub(super) stop_asked: Option<Instant>,
    /// The harness said something in this turn before its prompt went out:
    /// the person's message took over what it was doing.
    pub(super) heard_before_prompt: bool,
}

/// A request that waits for the person, as Lever holds it: the harness's
/// handle for answering it, and its thread and turn.
pub(super) struct Waiting {
    pub(super) handle: u64,
    pub(super) thread_id: String,
    pub(super) turn: u64,
    /// The request as the record has it, so how it ends can be said though
    /// the database takes nothing.
    pub(super) record: RequestRecord,
}

/// A subagent currently working on a thread, tracked so the thread stays
/// working and the harness stays alive even after the parent turn ends.
pub(super) struct WorkingSubagent {
    pub(super) thread_id: String,
    pub(super) turn: u64,
    pub(super) subagent_id: String,
    pub(super) harness_session_id: String,
    pub(super) title: String,
    pub(super) started: i64,
    /// When the person asked to stop this subagent's thread and the harness
    /// was told.
    pub(super) stop_asked: Option<Instant>,
}

/// A message steered into a thread's turn under way, whose taking the
/// harness has yet to answer.
pub(super) struct PendingSteer {
    /// Lever's number for it, which the harness's answer comes with.
    pub(super) steer: u64,
    /// Peer waiting for the reply, or `None` if it went away.
    pub(super) peer_id: Option<u64>,
    pub(super) thread_id: String,
    pub(super) turn: u64,
    pub(super) text: String,
    /// The desktop context it carries, as the record keeps it, and as the
    /// harness is told it.
    pub(super) context: Option<Value>,
    pub(super) context_prompt: Option<String>,
    /// The person stopped its turn before the harness answered: a message
    /// the harness did not take is dropped, not queued.
    pub(super) stopped: bool,
    /// Its place in the thread's queue, kept until the harness answers, so a
    /// message it did not take waits ahead of those sent after it.
    pub(super) queued: QueuedRecord,
}

/// What a message asks of its thread besides its text.
pub(super) struct Wanted<'a> {
    pub(super) mode: Option<&'a str>,
    pub(super) model: Option<&'a str>,
    pub(super) effort: Option<&'a str>,
}

impl Wanted<'_> {
    /// Whether it asks for a mode, Model, or effort the thread is not at.
    pub(super) fn changes(&self, meta: &ThreadMeta) -> bool {
        self.mode.is_some_and(|m| m != meta.mode)
            || self.model.is_some_and(|m| meta.model.as_deref() != Some(m))
            || self.effort.is_some_and(|e| meta.effort.as_deref() != Some(e))
    }
}

pub(super) struct DeleteTarget {
    pub(super) session_id: String,
    pub(super) retired: bool,
}

pub(super) struct PendingDelete {
    pub(super) peer_id: u64,
    pub(super) thread: String,
    pub(super) remaining: VecDeque<DeleteTarget>,
    pub(super) in_flight: Option<DeleteTarget>,
    pub(super) warning: Option<String>,
}

#[derive(Default)]
pub(super) struct TurnTracker {
    pub(super) working_turns: HashMap<String, WorkingTurn>,
    /// Subagents currently working, keyed by Lever's `sub-...` ID.
    pub(super) working_subagents: HashMap<String, WorkingSubagent>,
    /// Requests that wait, by Lever's request ID.
    pub(super) waiting: HashMap<String, Waiting>,
    pub(super) pending_deletes: Vec<PendingDelete>,
    /// Threads that moved to a new project folder while a turn was in flight,
    /// mapped to the last turn number that ran on the pre-move session. The
    /// old harness session stays mapped while idle so late outliving subagents
    /// or harness continuations still route to the thread, and is detached
    /// before the next turn or session-level control runs.
    pub(super) pending_session_resets: HashMap<String, Option<u64>>,
    /// Messages steered into turns under way, which the harness has yet to
    /// answer, and the number the next one gets.
    pub(super) pending_steers: Vec<PendingSteer>,
    pub(super) next_steer: u64,
    /// Threads whose next queued message may go as its own turn once the
    /// thread is free: its turn ended done or failed, or the person sent
    /// another message.
    pub(super) drain_wanted: HashSet<String>,
    /// The desktop context each queued message carries, as the harness is
    /// told it, by its ID. One queued before Lever started is told it again
    /// from the record.
    pub(super) queued_prompts: HashMap<String, (String, String)>,
    /// Threads whose next queued message Lever could not begin, logged once.
    pub(super) drain_stuck: HashSet<String>,
    /// Threads that had a subagent in the harness now running: what their
    /// session says after their turn ended is a turn the harness began.
    pub(super) subagent_threads: HashSet<String>,
    /// Threads whose Stop left nothing of them working, until the person's
    /// next message: a turn the harness begins on one is stopped as it
    /// begins, as what reports to it was.
    pub(super) held_stops: HashSet<String>,
    /// The harness session IDs of the subagents heard of since the harness
    /// last started.
    pub(super) harness_children: HashSet<String>,
    /// What `activity` last said: threads working, and threads waiting.
    pub(super) activity: (Vec<String>, Vec<String>),
}

impl TurnTracker {
    pub(super) fn has_active_work(&self) -> bool {
        !self.pending_deletes.is_empty()
            || !self.working_turns.is_empty()
            || !self.working_subagents.is_empty()
            || !self.waiting.is_empty()
            || !self.pending_steers.is_empty()
    }

    /// Whether a thread has active work on its current harness session that
    /// prevents detaching or switching sessions right now.
    pub(super) fn has_live_session_work(&self, thread_id: &str) -> bool {
        self.working_turns.contains_key(thread_id)
            || self.working_subagents.values().any(|s| s.thread_id == thread_id)
            || self.waiting.values().any(|w| w.thread_id == thread_id)
            || self.pending_steers.iter().any(|p| p.thread_id == thread_id)
    }

    pub(super) fn has_waiting_subagent_request(&self, thread_id: &str) -> bool {
        self.waiting
            .values()
            .any(|w| w.thread_id == thread_id && w.record.subagent.is_some())
    }

    pub(super) fn has_working_subagent(&self, thread_id: &str) -> bool {
        self.working_subagents.values().any(|s| s.thread_id == thread_id)
    }

    pub(super) fn has_waiting_request(&self, thread_id: &str) -> bool {
        self.waiting.values().any(|w| w.thread_id == thread_id)
    }

    pub(super) fn has_pending_steer(&self, thread_id: &str) -> bool {
        self.pending_steers.iter().any(|p| p.thread_id == thread_id)
    }

    /// Whether a thread is being deleted: what its harness says of it now
    /// goes nowhere.
    pub(super) fn deleting(&self, thread_id: &str) -> bool {
        self.pending_deletes.iter().any(|p| p.thread == thread_id)
    }

    pub(super) fn on_peer_gone(&mut self, peer_id: u64) {
        // A message steered into a turn is the turn's once the harness takes
        // it, whether or not who sent it waits to hear so.
        for pending in &mut self.pending_steers {
            if pending.peer_id == Some(peer_id) {
                pending.peer_id = None;
            }
        }
    }

    pub(super) fn clear_harness_ephemeral(&mut self) {
        self.pending_session_resets.clear();
        self.subagent_threads.clear();
        self.held_stops.clear();
        self.harness_children.clear();
    }

    pub(super) fn fail_pending_deletes(&mut self, message: &str, replies: &mut Vec<PeerReply>) {
        for pending in self.pending_deletes.drain(..) {
            replies.push(PeerReply {
                peer_id: pending.peer_id,
                message: error_json("harness-error", message),
                subscribe: None,
            });
        }
    }

    /// Threads working (in their own turn or in a subagent), and threads with
    /// a request that waits, each sorted.
    pub(super) fn current_activity(&self) -> (Vec<String>, Vec<String>) {
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
    pub(super) fn activity_changed(&mut self) -> Option<Broadcast> {
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

    /// The turn the harness began itself that works on a thread, if one does.
    pub(super) fn harness_turn(&self, thread_id: &str) -> Option<u64> {
        self.working_turns
            .get(thread_id)
            .filter(|active| matches!(active.dispatch, TurnDispatch::HarnessStarted { .. }))
            .map(|active| active.turn)
    }

    /// The turn the harness began on a thread, when a message may take it
    /// over: not one the person asked to stop, whose Stop must go through, and
    /// not one running on a pre-move session awaiting detach.
    pub(super) fn harness_turn_to_take_over(&self, thread_id: &str) -> Option<u64> {
        if self.pending_session_resets.contains_key(thread_id) {
            return None;
        }
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
    pub(super) fn heard_from(&mut self, thread_id: &str) {
        if let Some(active) = self.working_turns.get_mut(thread_id)
            && active.stop_asked.is_none()
            && let TurnDispatch::HarnessStarted { last_heard, .. } = &mut active.dispatch
        {
            *last_heard = Instant::now();
        }
    }

    /// A turn the harness began is not quiet while an act of it works.
    pub(super) fn note_harness_act(&mut self, thread_id: &str, turn: u64, act_id: &str, state: &str) {
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

    /// When a turn the harness began may be taken as done: it has been quiet
    /// for `wake_quiet`, and none of its acts works, no request of its thread
    /// waits, and no subagent of its thread works. `None` while one of those
    /// still holds it.
    pub(super) fn harness_turn_quiet_in(
        &self,
        thread_id: &str,
        active: &WorkingTurn,
        wake_quiet: Duration,
    ) -> Option<Duration> {
        let TurnDispatch::HarnessStarted {
            last_heard, open_acts, ..
        } = &active.dispatch
        else {
            return None;
        };
        if active.stop_asked.is_some()
            || !open_acts.is_empty()
            || self.has_waiting_request(thread_id)
            || self.has_working_subagent(thread_id)
        {
            return None;
        }
        Some(wake_quiet.saturating_sub(last_heard.elapsed()))
    }
}

impl Service {
    /// Whether Lever is still setting a thread's session to what the thread
    /// asks: until it has, what the harness says the session is at is not
    /// the thread's.
    pub(super) fn settling(&self, thread_id: &str) -> bool {
        self.turns.working_turns.get(thread_id).is_some_and(|active| {
            !matches!(
                active.dispatch,
                TurnDispatch::PromptInFlight { .. } | TurnDispatch::HarnessStarted { .. }
            )
        }) || self.catalog.has_pending_set_model(thread_id)
            || self.catalog.has_mode_in_flight(thread_id)
    }

    /// Whether a thread is busy: a turn works on it, or the harness has yet
    /// to answer a change Lever asked of its session. A subagent that works
    /// on does not make it busy: the person may send a message, or change
    /// the Model, mode, or effort, as the harness takes each in the session
    /// its subagents came from. A turn the harness began itself is taken over
    /// by the next message.
    pub(super) fn thread_busy(&self, thread_id: &str) -> bool {
        self.turns.working_turns.contains_key(thread_id) || self.session_change_pending(thread_id)
    }

    /// Whether the harness has yet to answer a change Lever asked of a
    /// thread's session, or the thread is being deleted or waiting on
    /// outliving subagents/requests/steers before detaching a pre-move session.
    pub(super) fn session_change_pending(&self, thread_id: &str) -> bool {
        self.turns.deleting(thread_id)
            || self.catalog.has_pending_set_model(thread_id)
            || self.catalog.has_mode_in_flight(thread_id)
            || self.discovery.is_loading_history(thread_id)
            || (!self.turns.working_turns.contains_key(thread_id)
                && self.turns.pending_session_resets.contains_key(thread_id)
                && self.turns.has_live_session_work(thread_id))
    }

    /// Ends a turn the harness began as done: it went quiet, or the person's
    /// message takes it over.
    pub(super) fn end_harness_turn(&mut self, thread_id: &str, turn: u64, log: &mut Log) -> Broadcast {
        let unsaved = self
            .turns
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

    /// A harness did not end a turn it was asked to stop. The turn ends as
    /// stopped, and the harness ends with every process under it, so nothing
    /// the turn started goes on. Every other turn it was working on is
    /// interrupted.
    pub(super) fn force_stop(&mut self, thread_id: &str, turn: u64, log: &mut Log) -> Effects {
        self.turns.working_turns.remove(thread_id);
        self.end_turn(thread_id, turn, "stopped", None, log);
        let mut effects = Effects::default();
        effects.broadcasts.push(Broadcast {
            thread_id: thread_id.to_string(),
            message: json!({ "event": "stopped", "thread": thread_id, "turn": turn }),
        });
        effects.extend(self.kill_harness("it did not stop a turn when asked", log));
        effects
    }

    pub(super) fn handle_stop(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
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
        let live = self.turns.working_turns.contains_key(thread_id)
            || self.turns.has_working_subagent(thread_id)
            || self.turns.has_waiting_request(thread_id);
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

        // Stop drops what waits on the thread, behind the turn it stops or
        // the ones before it, and what the harness has yet to say it took:
        // nothing runs after the person said stop.
        broadcasts.extend(self.drop_queued(thread_id, None, log));
        for pending in &mut self.turns.pending_steers {
            if pending.thread_id == thread_id {
                pending.stopped = true;
            }
        }
        // So is a message that waits for the thread's history to be read.
        self.discovery.mark_pending_sends_stopped(thread_id);

        // Cancel any subagents still working on this thread (or requested turn).
        let now = Instant::now();
        let mut child_sessions_to_cancel = Vec::new();
        for sub in self.turns.working_subagents.values_mut() {
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
            self.turns
                .working_subagents
                .values()
                .all(|s| s.thread_id != thread_id || s.stop_asked.is_some())
                && self.turns.working_turns.get(thread_id).is_none_or(|active| {
                    active.stop_asked.is_some() || requested_turn.is_none_or(|t| active.turn == t)
                });
        if leaves_nothing {
            self.turns.held_stops.insert(thread_id.to_string());
        }
        if !child_sessions_to_cancel.is_empty()
            && let Some(harness) = self.harness.as_mut()
        {
            for child_sid in child_sessions_to_cancel {
                harness.cancel_session(&child_sid);
            }
            if !self.turns.working_turns.contains_key(thread_id)
                && let Some(parent_sid) = harness.session_for_thread(thread_id).map(str::to_string)
            {
                harness.cancel_session(&parent_sid);
            }
        }

        let Some(active) = self.turns.working_turns.get_mut(thread_id) else {
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
                // the prompt is not sent: the turn ends now. Its session is the
                // thread's still, and the thread stays busy until the harness
                // answers, so the next turn knows which mode the session is in.
                let turn = active.turn;
                self.turns.working_turns.remove(thread_id);
                self.catalog
                    .modes_in_flight
                    .push((thread_id.to_string(), Instant::now()));
                self.end_turn(thread_id, turn, "stopped", None, log);
                self.refresh_idle_timer();
                vec![Broadcast {
                    thread_id: thread_id.to_string(),
                    message: json!({ "event": "stopped", "thread": thread_id, "turn": turn }),
                }]
            }
            TurnDispatch::WaitingForInit
            | TurnDispatch::WaitingForSession
            | TurnDispatch::WaitingForModel
            | TurnDispatch::WaitingForSubagentAnswer => {
                let waiting_for_model = matches!(active.dispatch, TurnDispatch::WaitingForModel);
                let desired_model = active.desired_model.clone().unwrap_or_default();
                let turn = active.turn;
                self.turns.working_turns.remove(thread_id);
                if waiting_for_model {
                    // Keep the thread locked until the in-flight `session/set_config_option`
                    // RPC settles so the next turn does not race it on the live session.
                    self.catalog.pending_set_models.push(PendingSetModel {
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

    pub(super) fn dispatch_turn(&mut self, thread_id: &str) {
        // Defensive guard: `send_message` flushes any idle pre-move session
        // reset before inserting the new turn, and `session_change_pending`
        // prevents starting a turn while pre-move subagents/requests/steers
        // remain live.
        if self.turns.pending_session_resets.contains_key(thread_id) {
            let blocked = self.turns.has_working_subagent(thread_id)
                || self.turns.has_waiting_request(thread_id)
                || self.turns.has_pending_steer(thread_id);
            debug_assert!(
                !blocked,
                "dispatch_turn reached with live work still holding pre-move session on {thread_id}"
            );
            if !blocked {
                let retired_turn = self.turns.pending_session_resets.remove(thread_id).flatten();
                if let Some(harness) = self.harness.as_mut() {
                    harness.detach_session(thread_id, retired_turn);
                }
                self.mcp.forget_thread(thread_id);
            }
        }
        let has_session = self
            .harness
            .as_ref()
            .and_then(|h| h.session_for_thread(thread_id))
            .is_some();
        if has_session {
            let Some(harness) = self.harness.as_mut() else {
                return;
            };
            let Some(active) = self.turns.working_turns.get_mut(thread_id) else {
                return;
            };
            let Some(session_id) = harness.session_for_thread(thread_id).map(str::to_string) else {
                return;
            };
            if let Some(model) = active.desired_model.clone()
                && harness.session_current_model(&session_id) != Some(model.as_str())
            {
                harness.set_session_model(thread_id, Some(active.turn), &session_id, &model);
                active.dispatch = TurnDispatch::WaitingForModel;
            } else {
                self.continue_turn(thread_id);
            }
        } else {
            let mcp_servers = self.mcp_servers_for_thread(thread_id);
            let Some(harness) = self.harness.as_mut() else {
                return;
            };
            let Some(active) = self.turns.working_turns.get_mut(thread_id) else {
                return;
            };
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
                &mcp_servers,
            );
        }
    }

    /// A turn whose session has its Model: the session is set to the
    /// thread's mode, when the harness has modes and it is in another, and
    /// then the prompt is sent.
    pub(super) fn continue_turn(&mut self, thread_id: &str) {
        let holds_prompt = self.holds_prompt_during_subagent_request();
        let has_subagent_wait = self.turns.has_waiting_subagent_request(thread_id);
        let in_batch = self.in_harness_batch;
        let Some(harness) = self.harness.as_mut() else {
            return;
        };
        let Some(active) = self.turns.working_turns.get_mut(thread_id) else {
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
        if harness.capabilities().supports_modes
            && harness.session_current_mode(&session_id) != Some(active.mode.as_str())
        {
            harness.set_session_mode(thread_id, Some(active.turn), &session_id, &active.mode);
            active.dispatch = TurnDispatch::WaitingForMode;
        } else if holds_prompt && (in_batch || has_subagent_wait) {
            // Within a batch the harness sent, a subagent's request later in
            // it is not yet known: the prompt goes after the batch, once none
            // asks.
            active.dispatch = TurnDispatch::WaitingForSubagentAnswer;
        } else {
            self.catalog.pending_effort_threads.remove(thread_id);
            let combined_context = match (&active.handoff_prompt, &active.context_prompt) {
                (Some(handoff), Some(ctx)) => Some(format!("{handoff}\n\n{ctx}")),
                (Some(handoff), None) => Some(handoff.clone()),
                (None, Some(ctx)) => Some(ctx.clone()),
                (None, None) => None,
            };
            harness.send_prompt(
                thread_id,
                active.turn,
                &session_id,
                combined_context.as_deref(),
                &active.message,
            );
            active.dispatch = TurnDispatch::PromptInFlight { session_id };
        }
    }

    /// Fails the working turn of a thread. A failure that comes for a turn
    /// that has ended already, or for one before the turn now working,
    /// changes nothing: that turn's end is in the record.
    pub(super) fn fail_turn(&mut self, thread_id: &str, turn: u64, message: &str, log: &mut Log) -> Vec<Broadcast> {
        if self
            .turns
            .working_turns
            .get(thread_id)
            .is_none_or(|active| active.turn != turn)
        {
            return Vec::new();
        }
        self.turns.working_turns.remove(thread_id);
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
    pub(super) fn end_turn(
        &mut self,
        thread_id: &str,
        turn: u64,
        state: &'static str,
        error: Option<&str>,
        log: &mut Log,
    ) {
        // What waits behind a turn that ended done or failed goes next. After
        // a Stop or an interruption it waits for the person.
        if matches!(state, "done" | "failed") && self.has_queued(thread_id) {
            self.turns.drain_wanted.insert(thread_id.to_string());
        }
        if let Err(err) = self.store.finish_turn(thread_id, turn, state, error, unix_now()) {
            store_failed(log, &err);
            self.journal.push_turn_ending(thread_id, turn, state, error);
        }
    }

    /// Sends the prompts that waited on a subagent's request, once no
    /// subagent of their thread asks.
    pub(super) fn send_held_prompts(&mut self) {
        let holds = self.holds_prompt_during_subagent_request();
        let ready: Vec<String> = self
            .turns
            .working_turns
            .iter()
            .filter(|(tid, active)| {
                matches!(active.dispatch, TurnDispatch::WaitingForSubagentAnswer)
                    && (!holds || !self.turns.has_waiting_subagent_request(tid))
            })
            .map(|(tid, _)| tid.clone())
            .collect();
        for thread_id in ready {
            self.continue_turn(&thread_id);
        }
    }

    pub(super) fn save_endings(&mut self) {
        self.journal.flush(&self.store);
    }

    /// The database did not take something of a thread. The log says so, and
    /// the turn the thread is working on ends as failed, not as done.
    pub(super) fn could_not_save(&mut self, thread_id: &str, err: &str, log: &mut Log) {
        store_failed(log, err);
        if let Some(active) = self.turns.working_turns.get_mut(thread_id) {
            active.unsaved = true;
        }
    }

    /// Keeps a note on a thread's newest turn and tells who is watching.
    pub(super) fn record_note(
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
    pub(super) fn turn_for_session_update(
        &mut self,
        thread_id: &str,
        opens: bool,
        broadcasts: &mut Vec<Broadcast>,
        log: &mut Log,
    ) -> Option<u64> {
        // A message whose prompt has not gone out, while a subagent's request
        // waits, cannot go until the request ends: what the harness begins
        // meanwhile is not its answer. It ends unsent, and the harness's
        // words are a turn of their own, unless the harness already spoke in
        // this turn, which the message took over. With no request waiting, the
        // message goes as soon as it is ready, and the harness takes it into
        // the work it is doing, as when it takes over a turn the harness began.
        if opens
            && self.turns.subagent_threads.contains(thread_id)
            && self.subagent_request_holds_prompt(thread_id)
            && let Some(active) = self.turns.working_turns.get(thread_id)
            && !active.heard_before_prompt
            && !matches!(
                active.dispatch,
                TurnDispatch::PromptInFlight { .. } | TurnDispatch::HarnessStarted { .. }
            )
        {
            let turn = active.turn;
            let active = self.turns.working_turns.remove(thread_id)?;
            // What Lever asked of the session for it is still to be
            // answered: until it is, the thread is held, as for a Stop.
            match active.dispatch {
                TurnDispatch::WaitingForMode | TurnDispatch::WaitingForEffort => {
                    self.catalog
                        .modes_in_flight
                        .push((thread_id.to_string(), Instant::now()));
                }
                TurnDispatch::WaitingForModel => self.catalog.pending_set_models.push(PendingSetModel {
                    peer_id: None,
                    thread_id: thread_id.to_string(),
                    model: active.desired_model.clone().unwrap_or_default(),
                    in_flight_on_session: true,
                }),
                _ => {}
            }
            self.end_turn(thread_id, turn, "failed", Some(NOT_SENT_MESSAGE), log);
            broadcasts.push(Broadcast {
                thread_id: thread_id.to_string(),
                message: json!({ "event": "failed", "thread": thread_id, "turn": turn, "message": NOT_SENT_MESSAGE }),
            });
        }
        if let Some(active) = self.turns.working_turns.get_mut(thread_id) {
            // What the harness says before the turn's prompt went out is the
            // work the person's message took over: the turn keeps it, and is
            // not ended unsent after.
            if opens
                && !matches!(
                    active.dispatch,
                    TurnDispatch::PromptInFlight { .. } | TurnDispatch::HarnessStarted { .. }
                )
            {
                active.heard_before_prompt = true;
            }
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
        let settled = !self.turns.deleting(thread_id) && !self.journal.has_unsaved_turn_ending(thread_id);
        // Under a Stop that holds, what the harness begins while Lever cannot
        // begin a turn for it, as the last end waits to be saved, is stopped
        // with the harness: there is no turn to stop it with.
        if opens && self.turns.held_stops.contains(thread_id) && self.journal.has_unsaved_turn_ending(thread_id) {
            self.kill_wanted = Some("it went on under a Stop while Lever could not record a turn for it");
            return None;
        }
        let (true, Some(session_id)) = (
            opens && settled && self.turns.subagent_threads.contains(thread_id),
            session_id,
        ) else {
            return self.turn_for_update(thread_id);
        };
        let meta = match self.store.thread_meta(thread_id) {
            Ok(Some(meta)) => meta,
            Ok(None) => return None,
            Err(err) => {
                store_failed(log, &err);
                if self.turns.held_stops.contains(thread_id) {
                    self.kill_wanted = Some("it went on under a Stop, and Lever could not read its thread");
                    return None;
                }
                return self.turn_for_update(thread_id);
            }
        };
        let now = unix_now();
        // What a held Stop catches begins a turn of its own: a turn that
        // ended before the Stop keeps how it ended.
        let held = self.turns.held_stops.contains(thread_id);
        let reopened = match self.store.latest_turn(thread_id) {
            Ok(Some((turn, state, true))) if state == "done" && !held => {
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
                    store_failed(log, &err);
                    // Under a Stop that holds, what the harness goes on with
                    // is stopped though it cannot be recorded: with no turn to
                    // stop, the harness is.
                    if held {
                        self.kill_wanted = Some("it went on under a Stop, and Lever could not record a turn for it");
                        return None;
                    }
                    // What the harness says still goes to the newest turn.
                    return self.turn_for_update(thread_id);
                }
            },
        };
        self.idle_since = None;
        if let Some(slot) = self.turns.pending_session_resets.get_mut(thread_id) {
            *slot = Some(turn);
        }
        if held && let Some(harness) = self.harness.as_mut() {
            harness.cancel_session(&session_id);
        }
        self.turns.working_turns.insert(
            thread_id.to_string(),
            WorkingTurn {
                turn,
                message: String::new(),
                context_prompt: None,
                handoff_prompt: None,
                handoff_pending: false,
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
                heard_before_prompt: false,
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

    pub(super) fn turn_for_update(&self, thread_id: &str) -> Option<u64> {
        if let Some(active) = self.turns.working_turns.get(thread_id) {
            return Some(active.turn);
        }
        self.store
            .thread_meta(thread_id)
            .ok()
            .flatten()
            .and_then(|m| m.latest_turn)
    }
}
