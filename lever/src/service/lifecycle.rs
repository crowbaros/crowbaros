//! Thread archive and delete lifecycle handlers.

use super::Service;
use super::turns::{DeleteTarget, PendingDelete};
use super::wire::{Broadcast, Effects, PeerReply, RequestOutcome, error_outcome, store_failed, unix_now};
use crate::log::{Event, Log};
use serde_json::{Value, json};
use std::collections::VecDeque;

impl Service {
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
}
