//! Active turn state, steering, queued-message drain (ADR 0037), subagents,
//! permission requests, thread deletion, and activity tracking.

use super::Service;
use super::catalog::PendingSetModel;
use super::wire::{
    ACTIVITY, Broadcast, Effects, NOT_SENT_MESSAGE, PeerReply, RequestOutcome, STEER_CHANGES_MESSAGE,
    UNFOLLOWED_MESSAGE, UNSAVED_MESSAGE, act_event_json, error_json, error_outcome, queued_event_json,
    request_event_json, store_failed, subagent_act_event_json, subagent_event_json, unix_now,
};
use crate::fulcrum;
use crate::log::{Event, Log};
use crate::store::{
    ActPatch, NewRequest, QueuedRecord, RequestRecord, SubagentActPatch, SubagentPatch, SubagentRecord, ThreadMeta,
};
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

    pub(super) fn handle_lifecycle(&mut self, peer_id: u64, op: &str, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(thread) = value.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "Expected a non-empty thread ID.");
        };
        let meta = match self.store.thread_meta(thread) {
            Ok(Some(meta)) => meta,
            Ok(None) => return error_outcome("no-thread", "This thread no longer exists."),
            Err(e) => return error_outcome("storage-error", &e),
        };
        if self.turns.deleting(thread) {
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
            self.flush_pending_session_reset(thread);
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
            || self.turns.has_working_subagent(thread)
            || self.turns.has_waiting_request(thread)
            || meta.working_turn.is_some()
            || self.journal.has_unsaved_for_thread(thread)
            || self.catalog.has_pending_set_model(thread)
        {
            return error_outcome(
                "working",
                "Stop this thread and wait for it to finish before deleting it.",
            );
        }
        // Delete retired (pre-move) sessions first, then the thread's current
        // session last, skipping any session ID currently owned by another live
        // thread in the harness. Each succeeded delete is persisted in
        // `finish_delete` so a partial failure never retries an already-deleted
        // session.
        let mut sessions: VecDeque<DeleteTarget> = VecDeque::new();
        match self.store.retired_sessions_for_thread(thread) {
            Ok(retired) => {
                for sid in retired {
                    let owned_by_other = self
                        .harness
                        .as_ref()
                        .and_then(|h| h.thread_for_session(&sid))
                        .is_some_and(|owner| owner != thread);
                    if !owned_by_other && !sessions.iter().any(|t| t.session_id == sid) {
                        sessions.push_back(DeleteTarget {
                            session_id: sid,
                            retired: true,
                        });
                    }
                }
            }
            Err(e) => {
                store_failed(log, &e);
                return error_outcome("storage-error", &e);
            }
        }
        if let Some(session) = meta.harness_session_id.filter(|s| !s.is_empty()) {
            let owned_by_other = self
                .harness
                .as_ref()
                .and_then(|h| h.thread_for_session(&session))
                .is_some_and(|owner| owner != thread);
            if !owned_by_other && !sessions.iter().any(|t| t.session_id == session) {
                sessions.push_back(DeleteTarget {
                    session_id: session,
                    retired: false,
                });
            }
        }
        if !sessions.is_empty() {
            if self.harness.is_none()
                && let Err(e) = self.start_harness(log)
            {
                return error_outcome("harness-error", &e);
            }
            self.turns.pending_deletes.push(PendingDelete {
                peer_id,
                thread: thread.into(),
                remaining: sessions,
                in_flight: None,
                warning: None,
            });
            let effects = self.dispatch_deletes(log);
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
        if let Err(e) = self.store.delete_thread(thread, unix_now()) {
            return error_outcome("storage-error", &e);
        }
        self.turns.subagent_threads.remove(thread);
        self.turns.held_stops.remove(thread);
        self.turns.drain_wanted.remove(thread);
        self.turns.pending_session_resets.remove(thread);
        self.mcp.forget_thread(thread);
        self.turns.queued_prompts.retain(|_, (t, _)| t != thread);
        if let Some(h) = self.harness.as_mut() {
            h.forget_session(thread);
        }
        self.catalog.thread_commands.remove(thread);
        RequestOutcome {
            reply: Some(json!({"reply":"deleted", "thread":thread, "warning":warning})),
            subscribe: None,
            broadcasts: vec![Broadcast {
                thread_id: thread.into(),
                message: json!({"event":"deleted", "thread":thread}),
            }],
        }
    }

    pub(super) fn finish_delete(
        &mut self,
        thread: &str,
        result: Result<(), String>,
        warning: Option<&str>,
        log: &mut Log,
    ) -> Effects {
        let Some(i) = self.turns.pending_deletes.iter().position(|p| p.thread == thread) else {
            return Effects::default();
        };
        if result.is_ok() && warning.is_none() {
            if let Some(deleted) = self.turns.pending_deletes[i].in_flight.take()
                && let Err(err) = self.store.mark_session_deleted(thread, &deleted.session_id, unix_now())
            {
                store_failed(log, &err);
                let pending = self.turns.pending_deletes.remove(i);
                let outcome = error_outcome("storage-error", &err);
                self.refresh_idle_timer();
                return Effects {
                    broadcasts: outcome.broadcasts,
                    replies: vec![PeerReply {
                        peer_id: pending.peer_id,
                        message: outcome.reply.unwrap(),
                        subscribe: None,
                    }],
                };
            }
            if !self.turns.pending_deletes[i].remaining.is_empty() {
                return self.dispatch_deletes(log);
            }
        } else if result.is_err()
            && self.turns.pending_deletes[i]
                .in_flight
                .as_ref()
                .is_some_and(|t| t.retired)
        {
            // A retired pre-move session that the harness cannot delete (for
            // instance already removed) warns rather than making the thread
            // impossible to delete. Leave its retirement row intact until
            // `delete_record` commits so if deleting the current session also
            // fails, a retry tries this session again and preserves the warning.
            let _ = self.turns.pending_deletes[i].in_flight.take();
            let _ = log.write(Event::Error, "could not delete a retired harness session");
            self.turns.pending_deletes[i].warning =
                Some("Could not delete an earlier session on the harness; its copy may remain.".to_string());
            if !self.turns.pending_deletes[i].remaining.is_empty() {
                return self.dispatch_deletes(log);
            }
            let pending = self.turns.pending_deletes.remove(i);
            let outcome = self.delete_record(thread, pending.warning.as_deref());
            self.refresh_idle_timer();
            return Effects {
                broadcasts: outcome.broadcasts,
                replies: vec![PeerReply {
                    peer_id: pending.peer_id,
                    message: outcome.reply.unwrap(),
                    subscribe: None,
                }],
            };
        }
        let pending = self.turns.pending_deletes.remove(i);
        let combined_warning = warning.or(pending.warning.as_deref());
        let outcome = match result {
            Ok(()) => self.delete_record(thread, combined_warning),
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

    pub(super) fn dispatch_deletes(&mut self, log: &mut Log) -> Effects {
        let mut unsupported = Vec::new();
        let mut finished = Vec::new();
        if let Some(h) = self.harness.as_mut().filter(|h| h.is_initialized()) {
            for pending in &mut self.turns.pending_deletes {
                if pending.in_flight.is_none() {
                    let mut dispatched = false;
                    while let Some(target) = pending.remaining.pop_front() {
                        // Never delete a session ID that another thread now owns.
                        if h.thread_for_session(&target.session_id)
                            .is_some_and(|owner| owner != pending.thread)
                        {
                            continue;
                        }
                        if !h.delete_session(&pending.thread, &target.session_id) {
                            unsupported.push(pending.thread.clone());
                        } else {
                            pending.in_flight = Some(target);
                        }
                        dispatched = true;
                        break;
                    }
                    if !dispatched && pending.remaining.is_empty() {
                        finished.push(pending.thread.clone());
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
                log,
            ));
        }
        for thread in finished {
            effects.extend(self.finish_delete(&thread, Ok(()), None, log));
        }
        effects
    }

    /// The person's turn a message sent now could be steered into, with its
    /// session: one whose prompt the harness is working on, that the person
    /// has not asked to stop, on a session Lever is changing nothing of.
    pub(super) fn steerable_turn(&self, thread_id: &str) -> Option<(u64, String)> {
        let active = self.turns.working_turns.get(thread_id)?;
        if active.stop_asked.is_some() || self.session_change_pending(thread_id) {
            return None;
        }
        match &active.dispatch {
            TurnDispatch::PromptInFlight { session_id } => Some((active.turn, session_id.clone())),
            _ => None,
        }
    }

    /// Steers a message into a thread's turn under way, when its harness can
    /// take one. The reply waits on the harness's answer: `steered`, or
    /// `not-steered` with why.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn steer_into_turn(
        &mut self,
        peer_id: u64,
        is_person: bool,
        thread_id: &str,
        turn: u64,
        session_id: &str,
        meta: &ThreadMeta,
        text: &str,
        wanted: Wanted<'_>,
        log: &mut Log,
    ) -> RequestOutcome {
        // A harness that cannot steer has the message wait behind the turn.
        // So does a thread whose subagent asks when the harness strands a
        // subagent's request on a new prompt.
        if !self.harness.as_ref().is_some_and(|h| h.capabilities().supports_steering)
            || self.subagent_request_holds_prompt(thread_id)
        {
            return self.queue_message(peer_id, is_person, thread_id, meta, text, wanted, log);
        }
        // The turn runs in the mode, Model, and effort it began with.
        if wanted.changes(meta) {
            return error_outcome("working", STEER_CHANGES_MESSAGE);
        }
        let (context, context_prompt) = self.desktop_context(is_person);
        let context_json = context.as_ref().map(Value::to_string);
        let queued =
            match self
                .store
                .insert_queued(thread_id, turn, text, context_json.as_deref(), "steering", unix_now())
            {
                Ok(queued) => queued,
                Err(err) => {
                    store_failed(log, &err);
                    return error_outcome("storage-error", &err);
                }
            };
        let steer = self.turns.next_steer;
        self.turns.next_steer += 1;
        if let Some(harness) = self.harness.as_mut() {
            harness.steer(thread_id, steer, session_id, context_prompt.as_deref(), text);
        }
        self.turns.pending_steers.push(PendingSteer {
            steer,
            peer_id: Some(peer_id),
            thread_id: thread_id.to_string(),
            turn,
            text: text.to_string(),
            context,
            context_prompt,
            stopped: false,
            queued,
        });
        RequestOutcome {
            reply: None,
            subscribe: None,
            broadcasts: Vec::new(),
        }
    }

    /// Whether a thread has messages that wait behind a turn.
    pub(super) fn has_queued(&self, thread_id: &str) -> bool {
        self.store.queued_of(thread_id, false).is_ok_and(|q| !q.is_empty())
    }

    /// Whether a message sent now can wait behind the thread's turn: one of
    /// the person's works, they have not asked to stop it, and Lever is
    /// changing nothing of its session.
    pub(super) fn queueable_turn(&self, thread_id: &str) -> bool {
        self.turns.working_turns.get(thread_id).is_some_and(|active| {
            active.stop_asked.is_none() && !matches!(active.dispatch, TurnDispatch::HarnessStarted { .. })
        }) && !self.session_change_pending(thread_id)
    }

    /// Keeps a message to wait behind the thread's newest turn, and replies
    /// `queued`. It goes as its own turn once the turns before it end. A
    /// thread that is free has its queue go now.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn queue_message(
        &mut self,
        peer_id: u64,
        is_person: bool,
        thread_id: &str,
        meta: &ThreadMeta,
        text: &str,
        wanted: Wanted<'_>,
        log: &mut Log,
    ) -> RequestOutcome {
        // A queued message goes in the thread's mode, Model, and effort as
        // they are when it goes.
        if wanted.changes(meta) {
            return error_outcome("working", STEER_CHANGES_MESSAGE);
        }
        let turn = match self.store.latest_turn(thread_id) {
            Ok(Some((turn, _, _))) => turn,
            Ok(None) => 0,
            Err(err) => return error_outcome("storage-error", &err),
        };
        let (context, context_prompt) = self.desktop_context(is_person);
        let effects = self.queue(Some(peer_id), thread_id, turn, text, context, context_prompt, log);
        let mut replies = effects.replies.into_iter();
        let reply = replies.next();
        RequestOutcome {
            reply: reply.as_ref().map(|r| r.message.clone()),
            subscribe: reply.and_then(|r| r.subscribe),
            broadcasts: effects.broadcasts,
        }
    }

    /// A message the harness did not take into its turn, or had yet to say
    /// it took when it ended, waits behind that turn. Its turn stopped, it is
    /// dropped.
    pub(super) fn queue_untaken(&mut self, pending: PendingSteer, log: &mut Log) -> Effects {
        let PendingSteer {
            peer_id,
            thread_id,
            turn,
            context_prompt,
            stopped,
            mut queued,
            ..
        } = pending;
        let to = if stopped { "dropped" } else { "queued" };
        if let Err(err) = self.store.set_queued_state(&thread_id, &queued.id, "steering", to) {
            store_failed(log, &err);
            return Effects {
                broadcasts: Vec::new(),
                replies: peer_id
                    .map(|peer_id| PeerReply {
                        peer_id,
                        message: error_json("storage-error", &err),
                        subscribe: None,
                    })
                    .into_iter()
                    .collect(),
            };
        }
        queued.state = to.to_string();
        self.announce_queued(peer_id, &thread_id, turn, &queued, context_prompt.filter(|_| !stopped))
    }

    /// Keeps a message behind `turn`, tells who watches the thread, and
    /// answers `peer_id` with `queued`.
    #[allow(clippy::too_many_arguments)]
    fn queue(
        &mut self,
        peer_id: Option<u64>,
        thread_id: &str,
        turn: u64,
        text: &str,
        context: Option<Value>,
        context_prompt: Option<String>,
        log: &mut Log,
    ) -> Effects {
        let context_json = context.as_ref().map(Value::to_string);
        let queued =
            match self
                .store
                .insert_queued(thread_id, turn, text, context_json.as_deref(), "queued", unix_now())
            {
                Ok(queued) => queued,
                Err(err) => {
                    store_failed(log, &err);
                    return Effects {
                        broadcasts: Vec::new(),
                        replies: peer_id
                            .map(|peer_id| PeerReply {
                                peer_id,
                                message: error_json("storage-error", &err),
                                subscribe: None,
                            })
                            .into_iter()
                            .collect(),
                    };
                }
            };
        self.announce_queued(peer_id, thread_id, turn, &queued, context_prompt)
    }

    /// Tells who watches a thread of a message that now waits behind `turn`,
    /// or was dropped there, and answers `peer_id` with `queued`.
    fn announce_queued(
        &mut self,
        peer_id: Option<u64>,
        thread_id: &str,
        turn: u64,
        queued: &QueuedRecord,
        context_prompt: Option<String>,
    ) -> Effects {
        if let Some(prompt) = context_prompt {
            self.turns
                .queued_prompts
                .insert(queued.id.clone(), (thread_id.to_string(), prompt));
        }
        // A thread with no turn working goes on with what waits as soon as
        // nothing else holds it.
        if queued.state == "queued" && !self.turns.working_turns.contains_key(thread_id) {
            self.turns.drain_wanted.insert(thread_id.to_string());
        }
        Effects {
            broadcasts: vec![Broadcast {
                thread_id: thread_id.to_string(),
                message: queued_event_json(thread_id, turn, queued),
            }],
            replies: peer_id
                .map(|peer_id| PeerReply {
                    peer_id,
                    message: json!({ "reply": "queued", "thread": thread_id, "turn": turn, "queued": queued.id }),
                    subscribe: Some(thread_id.to_string()),
                })
                .into_iter()
                .collect(),
        }
    }

    /// Drops what waits behind a thread's turn, or all it has waiting, and
    /// tells who watches the thread.
    fn drop_queued(&mut self, thread_id: &str, turn: Option<u64>, log: &mut Log) -> Vec<Broadcast> {
        self.turns.drain_wanted.remove(thread_id);
        match self.store.drop_queued(thread_id, turn) {
            Ok(dropped) => dropped
                .into_iter()
                .map(|(turn, q)| {
                    self.turns.queued_prompts.remove(&q.id);
                    Broadcast {
                        thread_id: thread_id.to_string(),
                        message: queued_event_json(thread_id, turn, &q),
                    }
                })
                .collect(),
            Err(err) => {
                store_failed(log, &err);
                Vec::new()
            }
        }
    }

    /// `unqueue`: the person takes back a message that waits.
    pub(super) fn handle_unqueue(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let (Some(thread_id), Some(id)) = (
            value.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()),
            value.get("queued").and_then(Value::as_str).filter(|s| !s.is_empty()),
        ) else {
            return error_outcome("invalid-request", "Expected non-empty string thread and queued fields.");
        };
        let waiting = match self.store.queued_of(thread_id, false) {
            Ok(waiting) => waiting,
            Err(err) => return error_outcome("storage-error", &err),
        };
        let Some((turn, mut queued)) = waiting.into_iter().find(|(_, q)| q.id == id) else {
            return match self.store.thread_meta(thread_id) {
                Ok(None) => error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
                Err(err) => error_outcome("storage-error", &err),
                Ok(Some(_)) => error_outcome("no-queued", "That message no longer waits."),
            };
        };
        match self.store.remove_queued(thread_id, id) {
            Ok(true) => {}
            Ok(false) => return error_outcome("no-queued", "That message no longer waits."),
            Err(err) => {
                store_failed(log, &err);
                return error_outcome("storage-error", &err);
            }
        }
        self.turns.queued_prompts.remove(id);
        queued.state = "removed".to_string();
        RequestOutcome {
            reply: Some(json!({ "reply": "ok" })),
            subscribe: None,
            broadcasts: vec![Broadcast {
                thread_id: thread_id.to_string(),
                message: queued_event_json(thread_id, turn, &queued),
            }],
        }
    }

    /// Sends the next message that waits on each thread that may go on, as
    /// its own turn: one whose turn ended done or failed, or to which the
    /// person sent another message, once nothing of the thread holds it. One
    /// Lever cannot begin now is tried again at the next pass.
    pub(super) fn drain_queues(&mut self, log: &mut Log) -> Vec<Broadcast> {
        let mut broadcasts = Vec::new();
        let wanted: Vec<String> = self.turns.drain_wanted.iter().cloned().collect();
        for thread_id in wanted {
            // A turn works: its end says whether what waits goes.
            if self.turns.working_turns.contains_key(&thread_id) {
                self.turns.drain_wanted.remove(&thread_id);
                continue;
            }
            // A change of the session, a delete, a message the harness has yet
            // to say it took, and (for harnesses that strand subagent requests)
            // a subagent's request each hold what waits until they end.
            if self.session_change_pending(&thread_id)
                || self.turns.has_pending_steer(&thread_id)
                || self.subagent_request_holds_prompt(&thread_id)
            {
                continue;
            }
            let next = match self.store.queued_of(&thread_id, false) {
                Ok(waiting) => waiting.into_iter().next(),
                Err(err) => {
                    store_failed(log, &err);
                    continue;
                }
            };
            let Some((behind, queued)) = next else {
                self.turns.drain_wanted.remove(&thread_id);
                continue;
            };
            // Marked as going first, so a record that takes no more writes
            // after the turn begins never sends it twice.
            match self.store.set_queued_state(&thread_id, &queued.id, "queued", "going") {
                Ok(true) => {}
                Ok(false) => continue,
                Err(err) => {
                    store_failed(log, &err);
                    continue;
                }
            }
            let prompt = self
                .turns
                .queued_prompts
                .get(&queued.id)
                .map(|(_, p)| p.clone())
                .or_else(|| {
                    queued
                        .context
                        .clone()
                        .and_then(|c| serde_json::from_value::<fulcrum::DesktopContext>(c).ok())
                        .map(|c| fulcrum::format_desktop_context(&c))
                });
            let value = json!({ "op": "send", "thread": thread_id, "text": queued.text });
            let outcome = self.send_message(0, true, &value, Some((queued.context.clone(), prompt)), log);
            if outcome.reply.as_ref().is_none_or(|r| r["reply"] != "sent") {
                // It waits still, and is tried again at the next pass.
                if let Err(err) = self.store.set_queued_state(&thread_id, &queued.id, "going", "queued") {
                    store_failed(log, &err);
                }
                if self.turns.drain_stuck.insert(thread_id.clone()) {
                    let _ = log.write(Event::Error, "a queued message could not begin its turn yet");
                }
                continue;
            }
            self.turns.drain_wanted.remove(&thread_id);
            self.turns.drain_stuck.remove(&thread_id);
            self.turns.queued_prompts.remove(&queued.id);
            if let Err(err) = self.store.forget_queued(&queued.id) {
                store_failed(log, &err);
            }
            let mut sent = queued.clone();
            sent.state = "sent".to_string();
            broadcasts.push(Broadcast {
                thread_id: thread_id.clone(),
                message: queued_event_json(&thread_id, behind, &sent),
            });
            broadcasts.extend(outcome.broadcasts);
        }
        broadcasts
    }

    /// The harness answered a message steered into a turn. One it took is
    /// kept with its turn, at the place in the answer it came to, and told
    /// to who watches the thread; one it did not take is queued behind it.
    pub(super) fn steer_answered(&mut self, steer: u64, taken: Result<(), String>, log: &mut Log) -> Effects {
        let Some(at) = self.turns.pending_steers.iter().position(|p| p.steer == steer) else {
            return Effects::default();
        };
        let pending = self.turns.pending_steers.remove(at);
        if taken.is_err() {
            // One the harness did not take waits behind the turn.
            return self.queue_untaken(pending, log);
        }
        if let Err(err) = self.store.forget_queued(&pending.queued.id) {
            store_failed(log, &err);
        }
        let thread_id = pending.thread_id;
        // The message still goes to who is watching. The turn's end says
        // that not all of it was saved.
        let context_json = pending.context.as_ref().map(Value::to_string);
        if let Err(err) = self.store.insert_steer(
            &thread_id,
            pending.turn,
            &pending.text,
            context_json.as_deref(),
            unix_now(),
        ) {
            self.could_not_save(&thread_id, &err, log);
        }
        let mut event = json!({
            "event": "steered",
            "thread": thread_id,
            "turn": pending.turn,
            "text": pending.text,
        });
        if let Some(context) = pending.context {
            event["context"] = context;
        }
        Effects {
            broadcasts: vec![Broadcast {
                thread_id: thread_id.clone(),
                message: event,
            }],
            replies: pending
                .peer_id
                .map(|peer_id| PeerReply {
                    peer_id,
                    message: json!({ "reply": "steered", "thread": thread_id, "turn": pending.turn }),
                    subscribe: Some(thread_id.clone()),
                })
                .into_iter()
                .collect(),
        }
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
        let leaves_nothing = self
            .turns
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

    pub(super) fn handle_answer(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
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
        let Some(waiting) = self.turns.waiting.get(request_id).filter(|w| w.thread_id == thread_id) else {
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
    pub(super) fn end_request(
        &mut self,
        request_id: &str,
        state: &'static str,
        answer: Option<&str>,
        log: &mut Log,
    ) -> Vec<Broadcast> {
        let Some(waiting) = self.turns.waiting.remove(request_id) else {
            return Vec::new();
        };
        self.turns.heard_from(&waiting.thread_id);
        let (thread_id, turn, record) = match self.store.end_request(request_id, state, answer, unix_now()) {
            Ok(Some(ended)) => ended,
            Ok(None) => return Vec::new(),
            Err(err) => {
                store_failed(log, &err);
                self.journal
                    .push_request_end(request_id, &waiting.thread_id, state, answer);
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
    pub(super) fn withdraw_requests(&mut self, which: impl Fn(&Waiting) -> bool, log: &mut Log) -> Vec<Broadcast> {
        let mut ids: Vec<String> = self
            .turns
            .waiting
            .iter()
            .filter(|(_, w)| which(w))
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        let mut broadcasts = Vec::new();
        for id in ids {
            if let Some(w) = self.turns.waiting.get(&id)
                && let Some(harness) = self.harness.as_mut()
            {
                harness.answer_permission(w.handle, None);
            }
            broadcasts.extend(self.end_request(&id, "withdrawn", None, log));
        }
        broadcasts
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
        if harness.capabilities().supports_modes && harness.session_current_mode(&session_id) != Some(active.mode.as_str()) {
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

    fn track_working_subagent(
        &mut self,
        thread_id: &str,
        recorded_turn: u64,
        harness_subagent_id: &str,
        record: &SubagentRecord,
        stop_asked: Option<Instant>,
    ) -> bool {
        let is_new = !self.turns.working_subagents.contains_key(&record.id);
        let title_changed = self
            .turns
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
        self.turns.subagent_threads.insert(thread_id.to_string());
        self.turns.working_subagents.insert(
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
        if self.turns.held_stops.contains(thread_id) {
            return Some(Instant::now());
        }
        if let Some(active) = self.turns.working_turns.get(thread_id) {
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
    pub(super) fn ensure_subagent(
        &mut self,
        thread_id: &str,
        harness_subagent_id: &str,
        title: Option<&str>,
        broadcasts: &mut Vec<Broadcast>,
        log: &mut Log,
    ) -> Option<(u64, SubagentRecord)> {
        if self.turns.deleting(thread_id) {
            return None;
        }
        let turn = self.turn_for_subagent(thread_id, harness_subagent_id)?;
        self.turns.harness_children.insert(harness_subagent_id.to_string());
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
                self.stop_untracked_subagent(thread_id, harness_subagent_id, turn);
                return None;
            }
        };
        self.journal.overlay_subagent(&mut record);
        if record.state == "working" {
            let prev_stop = self
                .turns
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_subagent_updated(
        &mut self,
        thread_id: &str,
        harness_subagent_id: &str,
        title: Option<&str>,
        state: Option<&str>,
        error: Option<&str>,
        from_launcher: bool,
        log: &mut Log,
    ) -> Effects {
        if self.turns.deleting(thread_id) {
            return Effects::default();
        }
        // What the record said before, so an update that changes nothing,
        // as a harness that says a subagent ended twice sends, says nothing.
        let before = self
            .store
            .subagent_summary(thread_id, harness_subagent_id)
            .ok()
            .flatten();
        // OpenCode follows a subagent it saw begin. One from before the
        // harness last started, that the thread's session takes up again, is
        // named by its launcher alone: nothing it does or asks comes to
        // Lever, so it is not shown as working on.
        let first_heard = self.turns.harness_children.insert(harness_subagent_id.to_string());
        let (state, error) = if from_launcher && first_heard && before.is_some() && state == Some("working") {
            // It may go on, and report to the thread's session as any does.
            self.turns.subagent_threads.insert(thread_id.to_string());
            (Some("failed"), Some(UNFOLLOWED_MESSAGE))
        } else {
            (state, error)
        };
        let Some(turn) = self.turn_for_subagent(thread_id, harness_subagent_id) else {
            return Effects::default();
        };
        let existing_id = self
            .turns
            .working_subagents
            .values()
            .find(|s| s.thread_id == thread_id && s.harness_session_id == harness_subagent_id)
            .map(|s| s.subagent_id.clone());
        // A subagent Lever knows keeps its own Stop: a Stop of a newer turn
        // was not for it. One it has yet to hear of is of the turn it joins.
        let stop_asked = match existing_id
            .as_deref()
            .and_then(|id| self.turns.working_subagents.get(id))
        {
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
        let mut broadcasts = Vec::new();
        let (recorded_turn, mut record) = match self.store.upsert_subagent(&patch, now) {
            Ok(saved) => saved,
            Err(err) => {
                self.could_not_save(thread_id, &err, log);
                if existing_id.is_none()
                    && !matches!(effective_state, Some("done" | "stopped" | "interrupted" | "failed"))
                {
                    self.stop_untracked_subagent(thread_id, harness_subagent_id, turn);
                }
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
                    && let Some(active) = self.turns.working_subagents.remove(&sub_id)
                {
                    let err_owned = error.map(str::to_string);
                    self.journal.push_subagent_ending(
                        &active.thread_id,
                        &active.subagent_id,
                        terminal,
                        err_owned.clone(),
                    );
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
        let before = if self.journal.overlay_subagent(&mut record) {
            before.map(|(title, _, _)| (title, record.state.clone(), record.error.clone()))
        } else {
            before
        };
        let tracked_changed = if record.state == "working" {
            self.track_working_subagent(thread_id, recorded_turn, harness_subagent_id, &record, stop_asked)
        } else {
            let was_working = self.turns.working_subagents.remove(&record.id).is_some();
            if was_working {
                self.turns.heard_from(thread_id);
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
    pub(super) fn record_request(
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
            .turns
            .working_turns
            .get(thread_id)
            .filter(|active| active.stop_asked.is_some())
            .map(|active| active.turn);
        let stopping = self.turns.deleting(thread_id)
            || match harness_subagent_id {
                None => stopped_turn.is_some(),
                Some(cid) => self
                    .turns
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
        // of the person either, nor one that comes under it to a turn that
        // has ended.
        if harness_subagent_id.is_none()
            && (self.turns.held_stops.contains(thread_id)
                || self
                    .turns
                    .working_turns
                    .get(thread_id)
                    .is_some_and(|active| active.turn == turn && active.stop_asked.is_some()))
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
                    .turns
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
                    self.turns.note_harness_act(thread_id, act_turn, &act.id, &act.state);
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
                self.turns.waiting.insert(
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

    /// A subagent Lever could not record, and so does not follow, that belongs
    /// to work the person stopped: with nothing to stop it by, the harness is
    /// stopped.
    fn stop_untracked_subagent(&mut self, thread_id: &str, harness_subagent_id: &str, turn: u64) {
        let tracked = self
            .turns
            .working_subagents
            .values()
            .any(|s| s.thread_id == thread_id && s.harness_session_id == harness_subagent_id);
        if !tracked
            && self
                .stop_for_new_subagent(thread_id, harness_subagent_id, turn)
                .is_some()
        {
            self.kill_wanted = Some("a subagent went on under a Stop, and Lever could not record it");
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

    pub(super) fn end_subagent(
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
            self.journal
                .push_subagent_ending(thread_id, subagent_id, state, error.map(str::to_string));
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

    /// The working subagent of a thread that launched a nested one.
    fn launcher(&self, thread_id: &str, harness_subagent_id: &str) -> Option<&WorkingSubagent> {
        let launcher_sid = self.harness.as_ref()?.launcher_of(harness_subagent_id)?;
        self.turns
            .working_subagents
            .values()
            .find(|s| s.thread_id == thread_id && s.harness_session_id == launcher_sid)
    }

    /// The turn a subagent's update belongs to: a nested subagent's is the
    /// turn of the subagent that launched it (whether still working or already
    /// recorded in SQLite), a delayed direct subagent from a retired pre-move
    /// session keeps that session's turn, and otherwise the thread's current or
    /// latest turn.
    fn turn_for_subagent(&self, thread_id: &str, harness_subagent_id: &str) -> Option<u64> {
        self.launcher(thread_id, harness_subagent_id)
            .map(|s| s.turn)
            .or_else(|| {
                let launcher_sid = self.harness.as_ref()?.launcher_of(harness_subagent_id)?;
                self.store.subagent_turn(thread_id, launcher_sid).ok().flatten()
            })
            .or_else(|| {
                self.harness
                    .as_ref()
                    .and_then(|h| h.retired_turn_of(harness_subagent_id))
            })
            .or_else(|| self.turn_for_update(thread_id))
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
