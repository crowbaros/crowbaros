//! Active-turn steering and queued-message drain (ADR 0037).

use super::Service;
use super::turns::{PendingSteer, TurnDispatch, Wanted};
use super::wire::{
    Broadcast, Effects, PeerReply, RequestOutcome, STEER_CHANGES_MESSAGE, error_json, error_outcome, queued_event_json,
    store_failed, unix_now,
};
use crate::fulcrum;
use crate::log::{Event, Log};
use crate::store::{QueuedRecord, ThreadMeta};
use serde_json::{Value, json};

impl Service {
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
        // A harness that cannot steer this session has the message wait behind
        // the turn. So does a thread whose subagent asks, for every harness (ADR 0037).
        if !self.harness.as_ref().is_some_and(|h| h.can_steer(session_id))
            || self.turns.has_waiting_subagent_request(thread_id)
        {
            return self.queue_message(peer_id, is_person, thread_id, meta, text, wanted, log);
        }
        // The turn runs in the mode, Model, and effort it began with.
        if wanted.changes(meta) {
            return error_outcome("working", STEER_CHANGES_MESSAGE);
        }
        let (context, context_prompt) = self.desktop_context(is_person);
        let context_json = context.as_ref().map(Value::to_string);
        // Commit the steering reservation before sending the RPC so a failed
        // write or commit never dispatches a steer to the harness.
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
    pub(super) fn drop_queued(&mut self, thread_id: &str, turn: Option<u64>, log: &mut Log) -> Vec<Broadcast> {
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
            // to say it took, and a subagent's waiting request each hold what
            // waits until they end (ADR 0037).
            if self.session_change_pending(&thread_id)
                || self.turns.has_pending_steer(&thread_id)
                || self.turns.has_waiting_subagent_request(&thread_id)
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
}
