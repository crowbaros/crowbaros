//! Lever's core service controller: owns the SQLite thread store, the on-demand
//! ACP harness lifecycle (including the idle timer), active turns across
//! concurrent threads, and Lever's wire messages.

use crate::acp::{AcpEvent, AcpHarness, HarnessConfig, TurnOutcome};
use crate::channel::LEVER_VERSION;
use crate::log::{Event, Log};
use crate::store::{ActPatch, ActRecord, NoteRecord, Store, ThreadRecord, ThreadSummary, TurnRecord};
use crate::sys;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::os::fd::RawFd;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Default time an idle harness stays running after the last working turn ends.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// One broadcast event targeted at peers subscribed to `thread_id`.
pub struct Broadcast {
    pub thread_id: String,
    pub message: Value,
}

/// Result of handling one request from a peer.
pub struct RequestOutcome {
    pub reply: Value,
    /// Thread ID to subscribe the requesting peer to, if any.
    pub subscribe: Option<String>,
    /// Broadcasts to emit to subscribed peers after the reply is queued.
    pub broadcasts: Vec<Broadcast>,
}

enum TurnDispatch {
    WaitingForInit,
    WaitingForSession,
    PromptInFlight { session_id: String },
}

struct WorkingTurn {
    turn: u64,
    message: String,
    cwd: String,
    existing_session_id: Option<String>,
    dispatch: TurnDispatch,
    stopping: bool,
}

pub struct Service {
    store: Store,
    harness_config: Option<HarnessConfig>,
    harness: Option<AcpHarness>,
    idle_timeout: Duration,
    idle_since: Option<Instant>,
    working_turns: HashMap<String, WorkingTurn>,
}

impl Service {
    pub fn new(store: Store, harness_config: Option<HarnessConfig>, idle_timeout: Duration) -> Self {
        Self {
            store,
            harness_config,
            harness: None,
            idle_timeout,
            idle_since: None,
            working_turns: HashMap::new(),
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

    /// Timeout in milliseconds for `poll(2)`, or `-1` when no idle timer is active.
    pub fn poll_timeout_ms(&self) -> libc::c_int {
        let Some(since) = self.idle_since else {
            return -1;
        };
        if self.harness.is_none() || !self.working_turns.is_empty() {
            return -1;
        }
        let elapsed = since.elapsed();
        if elapsed >= self.idle_timeout {
            0
        } else {
            let remaining = (self.idle_timeout - elapsed).as_millis();
            remaining.min(i32::MAX as u128) as libc::c_int
        }
    }

    /// Stops the harness if no thread has worked for `idle_timeout`.
    pub fn check_idle_timeout(&mut self, log: &mut Log) {
        if self.harness.is_none() || !self.working_turns.is_empty() {
            return;
        }
        let Some(since) = self.idle_since else {
            return;
        };
        if since.elapsed() >= self.idle_timeout {
            self.stop_harness("after idle timeout", log);
        }
    }

    pub fn handle_request(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(op) = value.as_object().and_then(|o| o.get("op")).and_then(Value::as_str) else {
            return error_outcome("invalid-request", "Expected a JSON object with a string op field.");
        };

        match op {
            "status" => self.handle_status(),
            "list" => self.handle_list(),
            "send" => self.handle_send(value, log),
            "resume" => self.handle_resume(value),
            "stop" => self.handle_stop(value),
            _ => error_outcome("unsupported", "This request is not supported."),
        }
    }

    pub fn on_harness_writable(&mut self, log: &mut Log) -> Vec<Broadcast> {
        let ok = self.harness.as_mut().is_none_or(AcpHarness::on_writable);
        if !ok {
            return self.on_harness_exited(log);
        }
        Vec::new()
    }

    pub fn on_harness_readable(&mut self, log: &mut Log) -> Vec<Broadcast> {
        let events = match self.harness.as_mut() {
            Some(h) => h.on_readable(),
            None => return Vec::new(),
        };
        let mut broadcasts = Vec::new();
        for ev in events {
            match ev {
                AcpEvent::Exited => {
                    broadcasts.extend(self.on_harness_exited(log));
                    break;
                }
                other => broadcasts.extend(self.apply_acp_event(other, log)),
            }
        }
        self.refresh_idle_timer();
        broadcasts
    }

    pub fn shutdown(mut self, log: &mut Log) -> Result<(), String> {
        if self.harness.is_some() {
            self.stop_harness("on Lever stop", log);
        }
        self.store.close()
    }

    fn handle_status(&self) -> RequestOutcome {
        let harnesses: Vec<Value> = self
            .harness_config
            .iter()
            .map(|cfg| json!({ "id": cfg.id, "name": cfg.name }))
            .collect();
        RequestOutcome {
            reply: json!({
                "reply": "status",
                "version": LEVER_VERSION,
                "harness_started": self.harness.is_some(),
                "harnesses": harnesses,
            }),
            subscribe: None,
            broadcasts: Vec::new(),
        }
    }

    fn handle_list(&self) -> RequestOutcome {
        match self.store.list_threads() {
            Ok(threads) => RequestOutcome {
                reply: json!({
                    "reply": "list",
                    "threads": threads.iter().map(thread_summary_json).collect::<Vec<_>>(),
                }),
                subscribe: None,
                broadcasts: Vec::new(),
            },
            Err(err) => error_outcome("storage-error", &err),
        }
    }

    fn handle_resume(&self, value: &Value) -> RequestOutcome {
        let Some(thread_id) = value.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "Expected a non-empty string thread field.");
        };
        match self.store.get_thread(thread_id) {
            Ok(Some(record)) => RequestOutcome {
                reply: thread_record_json(&record),
                subscribe: Some(thread_id.to_string()),
                broadcasts: Vec::new(),
            },
            Ok(None) => error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
            Err(err) => error_outcome("storage-error", &err),
        }
    }

    fn handle_send(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
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

        let Some(harness_name) = self.harness_config.as_ref().map(|c| c.name.clone()) else {
            return error_outcome("no-harness", "No harness is available.");
        };

        let now = unix_now();
        let (thread_id, cwd, existing_session_id) = if let Some(tid) = requested_thread {
            match self.store.thread_meta(tid) {
                Ok(Some(meta)) => {
                    if meta.working_turn.is_some() || self.working_turns.contains_key(tid) {
                        return error_outcome("working", "This thread is already working on a turn.");
                    }
                    (tid.to_string(), meta.cwd, meta.harness_session_id)
                }
                Ok(None) => return error_outcome("no-thread", &format!("There is no thread {tid}.")),
                Err(err) => return error_outcome("storage-error", &err),
            }
        } else {
            let tid = format!("t-{}", sys::random_hex(6));
            let cwd = requested_cwd.map(str::to_string).unwrap_or_else(default_cwd);
            let title = initial_title(text);
            if let Err(err) = self.store.create_thread(&tid, &title, &cwd, Some(&harness_name), now) {
                return error_outcome("storage-error", &err);
            }
            (tid, cwd, None)
        };

        let turn = match self.store.create_turn(&thread_id, text, now) {
            Ok(turn) => turn,
            Err(err) => return error_outcome("storage-error", &err),
        };

        let mut broadcasts = vec![Broadcast {
            thread_id: thread_id.clone(),
            message: json!({
                "event": "message",
                "thread": thread_id,
                "turn": turn,
                "text": text,
            }),
        }];

        self.idle_since = None;
        self.working_turns.insert(
            thread_id.clone(),
            WorkingTurn {
                turn,
                message: text.to_string(),
                cwd: cwd.clone(),
                existing_session_id: existing_session_id.clone(),
                dispatch: TurnDispatch::WaitingForInit,
                stopping: false,
            },
        );

        // Ensure the harness child is running, then dispatch session/prompt.
        if self.harness.is_none()
            && let Err(err) = self.start_harness(log)
        {
            broadcasts.extend(self.fail_turn(&thread_id, turn, &err));
            return RequestOutcome {
                reply: json!({ "reply": "sent", "thread": thread_id, "turn": turn }),
                subscribe: Some(thread_id),
                broadcasts,
            };
        }

        if self.harness.as_ref().is_some_and(AcpHarness::is_initialized) {
            self.dispatch_turn(&thread_id);
        }

        RequestOutcome {
            reply: json!({
                "reply": "sent",
                "thread": thread_id,
                "turn": turn,
            }),
            subscribe: Some(thread_id),
            broadcasts,
        }
    }

    fn handle_stop(&mut self, value: &Value) -> RequestOutcome {
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

        let Some(active) = self.working_turns.get_mut(thread_id) else {
            return RequestOutcome {
                reply: json!({ "reply": "ok" }),
                subscribe: None,
                broadcasts: Vec::new(),
            };
        };
        if let Some(t) = requested_turn
            && active.turn != t
        {
            return RequestOutcome {
                reply: json!({ "reply": "ok" }),
                subscribe: None,
                broadcasts: Vec::new(),
            };
        }

        let broadcasts = match &active.dispatch {
            TurnDispatch::PromptInFlight { session_id } => {
                active.stopping = true;
                let sid = session_id.clone();
                if let Some(harness) = self.harness.as_mut() {
                    harness.cancel_session(&sid);
                }
                Vec::new()
            }
            TurnDispatch::WaitingForInit | TurnDispatch::WaitingForSession => {
                let turn = active.turn;
                self.working_turns.remove(thread_id);
                let _ = self.store.finish_turn(thread_id, turn, "stopped", None, unix_now());
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

        RequestOutcome {
            reply: json!({ "reply": "ok" }),
            subscribe: None,
            broadcasts,
        }
    }

    fn start_harness(&mut self, log: &mut Log) -> Result<(), String> {
        let cfg = self
            .harness_config
            .as_ref()
            .ok_or_else(|| "No harness is available.".to_string())?
            .clone();
        let harness = AcpHarness::spawn(&cfg)?;
        let _ = log.write(
            Event::Harness,
            &format!("started {} (process {})", cfg.name, harness.pid()),
        );
        self.harness = Some(harness);
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
            harness.send_prompt(thread_id, active.turn, &session_id, &active.message);
            active.dispatch = TurnDispatch::PromptInFlight { session_id };
        } else {
            active.dispatch = TurnDispatch::WaitingForSession;
            harness.ensure_session(
                thread_id,
                active.turn,
                &active.cwd,
                active.existing_session_id.as_deref(),
            );
        }
    }

    fn apply_acp_event(&mut self, event: AcpEvent, log: &mut Log) -> Vec<Broadcast> {
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
                Vec::new()
            }
            AcpEvent::InitializeFailed { message } => {
                let _ = log.write(Event::Error, &format!("harness initialize failed: {message}"));
                let tids: Vec<(String, u64)> = self
                    .working_turns
                    .iter()
                    .map(|(tid, t)| (tid.clone(), t.turn))
                    .collect();
                let mut broadcasts = Vec::new();
                for (tid, turn) in tids {
                    broadcasts.extend(self.fail_turn(&tid, turn, &message));
                }
                self.stop_harness("after initialize failure", log);
                broadcasts
            }
            AcpEvent::SessionReady {
                thread_id,
                turn,
                session_id,
            } => {
                let _ = self.store.set_harness_session_id(&thread_id, &session_id);
                let Some(active) = self.working_turns.get_mut(&thread_id) else {
                    // Turn was stopped before session setup finished.
                    return Vec::new();
                };
                if active.turn != turn {
                    return Vec::new();
                }
                if let Some(harness) = self.harness.as_mut() {
                    harness.send_prompt(&thread_id, turn, &session_id, &active.message);
                    active.dispatch = TurnDispatch::PromptInFlight { session_id };
                }
                Vec::new()
            }
            AcpEvent::SessionFailed {
                thread_id,
                turn,
                message,
            } => self.fail_turn(&thread_id, turn, &message),
            AcpEvent::TextChunk { thread_id, text } => {
                let Some(turn) = self.turn_for_update(&thread_id) else {
                    return Vec::new();
                };
                let _ = self.store.append_answer(&thread_id, turn, &text, unix_now());
                vec![Broadcast {
                    thread_id: thread_id.clone(),
                    message: json!({
                        "event": "text",
                        "thread": thread_id,
                        "turn": turn,
                        "text": text,
                    }),
                }]
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
                    return Vec::new();
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
                let Ok((recorded_turn, act)) = self.store.upsert_act(&patch, unix_now()) else {
                    return Vec::new();
                };
                vec![Broadcast {
                    thread_id: thread_id.clone(),
                    message: act_event_json(&thread_id, recorded_turn, &act),
                }]
            }
            AcpEvent::TitleUpdated { thread_id, title } => {
                let _ = self.store.update_thread_title(&thread_id, &title, unix_now());
                vec![Broadcast {
                    thread_id: thread_id.clone(),
                    message: json!({
                        "event": "thread-updated",
                        "thread": thread_id,
                        "title": title,
                    }),
                }]
            }
            AcpEvent::UnknownUpdate {
                thread_id,
                raw_json,
                plain_text,
            } => {
                let after_turn = !self.working_turns.contains_key(&thread_id);
                let Some(turn) = self.turn_for_update(&thread_id) else {
                    return Vec::new();
                };
                let Ok(note) = self
                    .store
                    .insert_note(&thread_id, turn, &raw_json, &plain_text, after_turn, unix_now())
                else {
                    return Vec::new();
                };
                vec![Broadcast {
                    thread_id: thread_id.clone(),
                    message: json!({
                        "event": "note",
                        "thread": thread_id,
                        "turn": turn,
                        "note": note.id,
                        "text": note.text,
                    }),
                }]
            }
            AcpEvent::PromptFinished {
                thread_id,
                turn,
                outcome,
            } => {
                let Some(active) = self.working_turns.remove(&thread_id) else {
                    return Vec::new();
                };
                if active.turn != turn {
                    return Vec::new();
                }
                // A harness that answers a `session/cancel` with an error
                // instead of `StopReason::Cancelled` still stopped the turn.
                let outcome = match outcome {
                    TurnOutcome::Failed(_) if active.stopping => TurnOutcome::Stopped,
                    other => other,
                };
                let now = unix_now();
                match outcome {
                    TurnOutcome::Done => {
                        let _ = self.store.finish_turn(&thread_id, turn, "done", None, now);
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
                        let _ = self.store.finish_turn(&thread_id, turn, "stopped", None, now);
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
                        let _ = self.store.finish_turn(&thread_id, turn, "failed", Some(&msg), now);
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
                }
            }
            AcpEvent::Exited => Vec::new(),
        }
    }

    fn on_harness_exited(&mut self, log: &mut Log) -> Vec<Broadcast> {
        if let Some(harness) = self.harness.take() {
            let pid = harness.pid();
            harness.stop();
            let name = self
                .harness_config
                .as_ref()
                .map(|c| c.name.as_str())
                .unwrap_or("harness");
            let _ = log.write(Event::Harness, &format!("{name} (process {pid}) exited unexpectedly"));
        }
        self.idle_since = None;
        self.working_turns.clear();

        let interrupted = self.store.interrupt_all_working(unix_now()).unwrap_or_default();
        interrupted
            .into_iter()
            .map(|(thread_id, turn)| Broadcast {
                thread_id: thread_id.clone(),
                message: json!({
                    "event": "interrupted",
                    "thread": thread_id,
                    "turn": turn,
                }),
            })
            .collect()
    }

    fn fail_turn(&mut self, thread_id: &str, turn: u64, message: &str) -> Vec<Broadcast> {
        self.working_turns.remove(thread_id);
        let _ = self
            .store
            .finish_turn(thread_id, turn, "failed", Some(message), unix_now());
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
        if self.harness.is_some() && self.working_turns.is_empty() {
            if self.idle_since.is_none() {
                self.idle_since = Some(Instant::now());
            }
        } else {
            self.idle_since = None;
        }
    }
}

fn error_outcome(code: &str, message: &str) -> RequestOutcome {
    RequestOutcome {
        reply: json!({
            "reply": "error",
            "code": code,
            "message": message,
        }),
        subscribe: None,
        broadcasts: Vec::new(),
    }
}

fn thread_summary_json(t: &ThreadSummary) -> Value {
    json!({
        "thread": t.id,
        "title": t.title,
        "cwd": t.cwd,
        "started": t.started,
        "updated": t.updated,
        "turns": t.turns,
        "state": t.state,
        "working": t.working,
    })
}

fn thread_record_json(r: &ThreadRecord) -> Value {
    json!({
        "reply": "resumed",
        "thread": r.id,
        "title": r.title,
        "cwd": r.cwd,
        "started": r.started,
        "updated": r.updated,
        "working": r.working,
        "turns": r.turns.iter().map(turn_record_json).collect::<Vec<_>>(),
    })
}

fn turn_record_json(t: &TurnRecord) -> Value {
    let mut obj = json!({
        "turn": t.turn,
        "message": t.message,
        "answer": t.answer,
        "state": t.state,
        "acts": t.acts.iter().map(act_record_json).collect::<Vec<_>>(),
        "notes": t.notes.iter().map(note_record_json).collect::<Vec<_>>(),
    });
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
