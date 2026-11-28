//! Wire-protocol types, JSON serializers, and shared message constants for
//! Lever's Unix socket API.

use crate::harness::{self, EffortOption, HarnessCommand, ModelCatalog};
use crate::log::{Event, Log};
use crate::store::{
    ActRecord, NoteRecord, QueuedRecord, RequestRecord, SteerRecord, SubagentRecord, ThreadRecord, ThreadSummary,
    TurnRecord,
};
use serde_json::{Value, json};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Metadata subscription for the drawer's thread list.
pub const THREAD_LIST_SUBSCRIPTION: &str = "@thread-list";

/// The key peers that sent `watch` are subscribed under. No thread has it.
pub(super) const ACTIVITY: &str = "*activity";
/// The modes a new thread can start in.
pub(super) const STARTING_MODES: [&str; 2] = ["ask-first", "full-access"];

/// Why a subagent the harness took up again after it restarted is shown as
/// failed, though it may go on.
pub(super) const UNFOLLOWED_MESSAGE: &str = "The Agent took up this subagent again after its harness restarted. The harness \
     does not pass on what it does or asks now, so it cannot be followed here.";
/// Why a message whose prompt was held ended unsent.
pub(super) const NOT_SENT_MESSAGE: &str =
    "Not sent: the Agent went on by itself before your message went out. Send it again.";
/// What a turn failed with when the harness finished it and Lever's database
/// did not take all of it.
pub(super) const UNSAVED_MESSAGE: &str = "Lever could not save all of this turn.";
/// What a turn failed with when the database did not take the session the
/// harness gave its thread.
pub(super) const UNSAVED_SESSION_MESSAGE: &str = "Lever could not save this thread's session.";
/// What Lever says when free hosted Models are off and no provider is signed in.
pub(super) const NO_MODEL_MESSAGE: &str =
    "No Model is available. Run `crowbar-agent free-models on` or `crowbar-agent login`.";
/// The note a thread gets when its harness session could not be picked up
/// again, and what Lever keeps as that note's source.
pub(super) const NEW_SESSION_NOTE: &str = "The harness could not pick this thread's session up again. It began a new one, which has none of the turns before.";
pub(super) const NEW_SESSION_SOURCE: &str = r#"{"lever":"new-session"}"#;
/// Why a message sent while a turn works is refused when it would change what
/// the turn runs with.
pub(super) const STEER_CHANGES_MESSAGE: &str = "A message sent while the thread works cannot change its mode, Model, or \
     effort. Send it without them, or when the turn ends.";

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
    pub(super) fn extend(&mut self, other: Effects) {
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

/// Says in the log that the database did not take a write. The database's
/// error names what it could not do, and nothing a thread said.
pub(super) fn store_failed(log: &mut Log, err: &str) {
    let _ = log.write(Event::Error, &format!("storage: {err}"));
}

pub(super) fn error_json(code: &str, message: &str) -> Value {
    json!({
        "reply": "error",
        "code": code,
        "message": message,
    })
}

pub(super) fn error_outcome(code: &str, message: &str) -> RequestOutcome {
    RequestOutcome {
        reply: Some(error_json(code, message)),
        subscribe: None,
        broadcasts: Vec::new(),
    }
}

fn models_payload_json(kind_key: &str, catalog: &ModelCatalog, free_hosted_models: bool, steering: bool) -> Value {
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
            .map(|m| json!({ "id": m, "name": harness::mode_name(m).unwrap_or(m) }))
            .collect::<Vec<_>>(),
        "steering": steering,
    });
    obj[kind_key] = json!("models");
    obj
}

pub(super) fn models_reply_json(catalog: &ModelCatalog, free_hosted_models: bool, steering: bool) -> Value {
    models_payload_json("reply", catalog, free_hosted_models, steering)
}

pub(super) fn models_event_json(catalog: &ModelCatalog, free_hosted_models: bool, steering: bool) -> Value {
    models_payload_json("event", catalog, free_hosted_models, steering)
}

pub(super) fn efforts_reply_json(model: &str, levels: &[EffortOption]) -> Value {
    json!({
        "reply": "efforts",
        "model": model,
        "efforts": levels
            .iter()
            .map(|level| json!({ "id": level.id, "name": level.name }))
            .collect::<Vec<_>>(),
    })
}

pub(super) fn commands_json(commands: &[HarnessCommand]) -> Vec<Value> {
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

pub(super) fn thread_summary_json(t: &ThreadSummary) -> Value {
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

pub(super) fn thread_record_json(r: &ThreadRecord) -> Value {
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
    if !t.steered.is_empty() {
        obj["steered"] = json!(t.steered.iter().map(steer_record_json).collect::<Vec<_>>());
    }
    if !t.queued.is_empty() {
        obj["queued"] = json!(t.queued.iter().map(queued_record_json).collect::<Vec<_>>());
    }
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

fn queued_record_json(q: &QueuedRecord) -> Value {
    let mut obj = json!({ "queued": q.id, "text": q.text, "state": q.state });
    if let Some(context) = &q.context {
        obj["context"] = context.clone();
    }
    obj
}

/// A `queued` event: a message that waits behind a turn, and each change to
/// it.
pub(super) fn queued_event_json(thread_id: &str, turn: u64, q: &QueuedRecord) -> Value {
    let mut obj = queued_record_json(q);
    obj["event"] = json!("queued");
    obj["thread"] = json!(thread_id);
    obj["turn"] = json!(turn);
    obj
}

fn steer_record_json(s: &SteerRecord) -> Value {
    let mut obj = json!({ "text": s.text, "at": s.at });
    if let Some(context) = &s.context {
        obj["context"] = context.clone();
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

pub(super) fn subagent_event_json(thread_id: &str, turn: u64, s: &SubagentRecord) -> Value {
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

pub(super) fn subagent_act_event_json(thread_id: &str, turn: u64, subagent_id: &str, a: &ActRecord) -> Value {
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

pub(super) fn act_event_json(thread_id: &str, turn: u64, a: &ActRecord) -> Value {
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

pub(super) fn request_event_json(thread_id: &str, turn: u64, r: &RequestRecord) -> Value {
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

pub(super) fn initial_title(text: &str) -> String {
    let first_line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Thread");
    let mut title = String::new();
    for ch in first_line.chars().take(80) {
        title.push(ch);
    }
    title
}

pub(super) fn default_cwd() -> String {
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
