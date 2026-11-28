//! Subagent lifecycle, nested launcher tracking, and turn attribution.

use super::Service;
use super::turns::WorkingSubagent;
use super::wire::{Broadcast, Effects, UNFOLLOWED_MESSAGE, store_failed, subagent_event_json, unix_now};
use crate::log::Log;
use crate::store::{SubagentPatch, SubagentRecord};
use std::time::Instant;

impl Service {
    pub(super) fn track_working_subagent(
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
    pub(super) fn stop_for_new_subagent(
        &self,
        thread_id: &str,
        harness_subagent_id: &str,
        recorded_turn: u64,
    ) -> Option<Instant> {
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

    /// A subagent Lever could not record, and so does not follow, that belongs
    /// to work the person stopped: with nothing to stop it by, the harness is
    /// stopped.
    pub(super) fn stop_untracked_subagent(&mut self, thread_id: &str, harness_subagent_id: &str, turn: u64) {
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

    /// The working subagent of a thread that launched a nested one.
    pub(super) fn launcher(&self, thread_id: &str, harness_subagent_id: &str) -> Option<&WorkingSubagent> {
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
    pub(super) fn turn_for_subagent(&self, thread_id: &str, harness_subagent_id: &str) -> Option<u64> {
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
}
