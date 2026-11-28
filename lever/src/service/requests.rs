//! Permission Request lifecycle (recording, answering, and withdrawal).

use super::Service;
use super::turns::Waiting;
use super::wire::{
    Broadcast, Effects, RequestOutcome, act_event_json, error_json, error_outcome, request_event_json, store_failed,
    subagent_act_event_json, unix_now,
};
use crate::log::Log;
use crate::store::{ActPatch, NewRequest, SubagentActPatch};
use serde_json::{Value, json};

impl Service {
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
}
