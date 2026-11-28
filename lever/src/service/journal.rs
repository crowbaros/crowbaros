//! In-memory write-behind journal for terminal turn, subagent, and request
//! state transitions when SQLite rejects a write, retried on every wake and
//! overlaid onto `list`/`resume` reads so peers never observe stale `working`
//! or `waiting` state.

use super::wire::unix_now;
use crate::store::{Store, SubagentRecord, ThreadRecord, ThreadSummary};
use std::time::Duration;

/// How long Lever waits before it tries again to save how a turn ended.
pub(super) const UNSAVED_RETRY: Duration = Duration::from_millis(250);

/// How a turn ended, kept until the database has it.
pub(super) struct Ending {
    pub(super) thread_id: String,
    pub(super) turn: u64,
    pub(super) state: &'static str,
    pub(super) error: Option<String>,
}

/// How a subagent ended, kept until the database has it.
pub(super) struct SubagentEnding {
    pub(super) thread_id: String,
    pub(super) subagent_id: String,
    pub(super) state: &'static str,
    pub(super) error: Option<String>,
}

/// How a request ended, kept until the database has it.
pub(super) struct RequestEnd {
    pub(super) request_id: String,
    pub(super) thread_id: String,
    pub(super) state: &'static str,
    pub(super) answer: Option<String>,
}

#[derive(Default)]
pub(super) struct WriteBehindJournal {
    /// Ends the database did not take, tried again on each wake.
    pub(super) unsaved_endings: Vec<Ending>,
    /// Ends of subagents the database did not take, tried again on each wake.
    pub(super) unsaved_subagent_endings: Vec<SubagentEnding>,
    /// Ends of requests the database did not take, tried again on each wake.
    pub(super) unsaved_request_ends: Vec<RequestEnd>,
}

impl WriteBehindJournal {
    pub(super) fn retry_wait(&self) -> Option<Duration> {
        (!self.unsaved_endings.is_empty()
            || !self.unsaved_subagent_endings.is_empty()
            || !self.unsaved_request_ends.is_empty())
        .then_some(UNSAVED_RETRY)
    }

    pub(super) fn has_unsaved_turn_ending(&self, thread_id: &str) -> bool {
        self.unsaved_endings.iter().any(|e| e.thread_id == thread_id)
    }

    pub(super) fn has_unsaved_for_thread(&self, thread_id: &str) -> bool {
        self.unsaved_endings.iter().any(|e| e.thread_id == thread_id)
            || self.unsaved_subagent_endings.iter().any(|e| e.thread_id == thread_id)
    }

    pub(super) fn push_turn_ending(&mut self, thread_id: &str, turn: u64, state: &'static str, error: Option<&str>) {
        self.unsaved_endings.push(Ending {
            thread_id: thread_id.to_string(),
            turn,
            state,
            error: error.map(str::to_string),
        });
    }

    pub(super) fn push_subagent_ending(
        &mut self,
        thread_id: &str,
        subagent_id: &str,
        state: &'static str,
        error: Option<String>,
    ) {
        self.unsaved_subagent_endings.push(SubagentEnding {
            thread_id: thread_id.to_string(),
            subagent_id: subagent_id.to_string(),
            state,
            error,
        });
    }

    pub(super) fn push_request_end(
        &mut self,
        request_id: &str,
        thread_id: &str,
        state: &'static str,
        answer: Option<&str>,
    ) {
        self.unsaved_request_ends.push(RequestEnd {
            request_id: request_id.to_string(),
            thread_id: thread_id.to_string(),
            state,
            answer: answer.map(str::to_string),
        });
    }

    pub(super) fn flush(&mut self, store: &Store) {
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

    /// Overlays unsaved turn/subagent endings onto a `ThreadSummary` for `list`.
    pub(super) fn overlay_thread_summary(
        &self,
        thread: &mut ThreadSummary,
        has_working_turn: bool,
        has_working_subagent: bool,
        has_waiting_request: bool,
    ) {
        // A turn that has ended is said to have ended, though the database has
        // yet to take its end. A thread's turns are numbered from one, so its
        // newest is its count of them.
        if let Some(ending) = self
            .unsaved_endings
            .iter()
            .rfind(|ending| ending.thread_id == thread.id && ending.turn == thread.turns)
        {
            thread.state = ending.state.to_string();
            thread.working = false;
        }
        if has_working_subagent {
            thread.working = true;
            thread.state = "working".to_string();
        } else if thread.working && !has_working_turn {
            // Every subagent of this thread has ended, though the database may
            // not have taken its end yet.
            if self.unsaved_subagent_endings.iter().any(|e| e.thread_id == thread.id) {
                thread.working = false;
                if thread.state == "working" {
                    thread.state = "done".to_string();
                }
            }
        }
        // What waits is what Lever holds: a request whose end the database has
        // yet to take waits no longer.
        thread.waiting = has_waiting_request;
    }

    /// Overlays unsaved endings onto a full `ThreadRecord` for `resume`.
    pub(super) fn overlay_thread_record(&self, thread_id: &str, record: &mut ThreadRecord) {
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
    }

    /// Gives a subagent's record the end the database has yet to take, if it
    /// has one, so a late word of the subagent does not bring it back to
    /// work. Says whether it had one.
    pub(super) fn overlay_subagent(&self, record: &mut SubagentRecord) -> bool {
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
}
