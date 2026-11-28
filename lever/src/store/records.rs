//! Record and patch DTOs persisted in Lever's SQLite store.

#[derive(Debug, Clone)]
pub struct ThreadSummary {
    pub archived: bool,
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub started: i64,
    pub updated: i64,
    pub turns: u64,
    pub state: String,
    pub working: bool,
    /// A request of this thread waits for the person.
    pub waiting: bool,
    pub mode: String,
    /// How many subagents this thread has.
    pub subagents: u64,
}

#[derive(Debug, Clone)]
pub struct ThreadMeta {
    pub cwd: String,
    /// The harness's ID for this thread's session. None when the thread has
    /// had no session, and empty when it had one whose ID the harness has
    /// given to another thread since.
    pub harness_session_id: Option<String>,
    /// The Model chosen for this thread, if any.
    pub model: Option<String>,
    /// The effort chosen for this thread, one of its Model's levels, if any.
    pub effort: Option<String>,
    pub mode: String,
    pub latest_turn: Option<u64>,
    pub working_turn: Option<u64>,
    /// True when this thread was discovered from a harness's own store and its
    /// history has not yet been read into Lever's record.
    pub needs_history: bool,
    /// True when this thread moved to a new project folder and its next fresh
    /// harness session must be handed the thread's prior turns.
    pub needs_handoff: bool,
}

#[derive(Debug, Clone)]
pub struct ThreadRecord {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: String,
    pub started: i64,
    pub updated: i64,
    pub working: bool,
    pub turns: Vec<TurnRecord>,
}

#[derive(Debug, Clone)]
pub struct TurnRecord {
    pub turn: u64,
    pub message: String,
    pub context: Option<serde_json::Value>,
    pub answer: String,
    pub state: String,
    pub error: Option<String>,
    /// The mode the turn began in, absent for turns recorded before modes.
    pub mode: Option<String>,
    /// `harness` for a turn the harness began itself, with no message from
    /// the person, as when a subagent's report wakes its thread; absent for
    /// the person's turns.
    pub origin: Option<String>,
    /// When the turn began, and when it ended, if it has.
    pub started: i64,
    pub ended: Option<i64>,
    pub acts: Vec<ActRecord>,
    pub requests: Vec<RequestRecord>,
    pub notes: Vec<NoteRecord>,
    pub subagents: Vec<SubagentRecord>,
    /// Messages the person sent while the turn worked, which the harness
    /// took into it, in the order it took them.
    pub steered: Vec<SteerRecord>,
    /// Messages the person sent while the turn worked that wait behind it,
    /// or that a Stop dropped, in the order sent.
    pub queued: Vec<QueuedRecord>,
}

/// A message that waits behind a turn, to go as its own turn once the turns
/// before it end: one the harness did not take into the turn, or one sent to
/// a harness that cannot take a message into a turn under way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedRecord {
    /// Lever's ID for it, `q-...`.
    pub id: String,
    pub text: String,
    /// The desktop context it carries, when it was sent from the drawer.
    pub context: Option<serde_json::Value>,
    /// `queued`, or `dropped` once a Stop withdrew it.
    pub state: String,
}

/// A message the harness took into a turn under way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerRecord {
    pub text: String,
    /// The desktop context it carried, when it was sent from the drawer.
    pub context: Option<serde_json::Value>,
    /// How much of the turn's answer, in bytes, had come when the harness
    /// took it: where in the answer it was taken.
    pub at: u64,
}

/// One message in a subagent's record: either the `prompt` given to it or
/// its `answer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentMessageRecord {
    pub role: String,
    pub text: String,
}

/// A subagent started by a thread's harness, kept in Lever's record with the
/// thread and identified only by Lever's own ID (`sub-...`).
#[derive(Debug, Clone)]
pub struct SubagentRecord {
    pub id: String,
    pub title: String,
    pub state: String,
    pub error: Option<String>,
    pub started: i64,
    pub ended: Option<i64>,
    pub messages: Vec<SubagentMessageRecord>,
    pub acts: Vec<ActRecord>,
}

/// Fields carried when creating or updating a subagent in Lever's record.
#[derive(Debug, Clone)]
pub struct SubagentPatch<'a> {
    pub thread_id: &'a str,
    pub turn_number: u64,
    pub harness_session_id: &'a str,
    pub title: Option<&'a str>,
    pub state: Option<&'a str>,
    pub error: Option<&'a str>,
}

/// Fields carried when creating or updating an act belonging to a subagent.
#[derive(Debug, Clone)]
pub struct SubagentActPatch<'a> {
    pub subagent_id: &'a str,
    pub thread_id: &'a str,
    pub harness_call_id: &'a str,
    pub kind: Option<&'a str>,
    pub title: Option<&'a str>,
    pub state: Option<&'a str>,
    pub detail: Option<&'a str>,
}

/// A thread's first turn: what Lever keeps of the thread as it begins.
pub struct NewThread<'a> {
    pub title: &'a str,
    pub cwd: &'a str,
    pub harness_name: Option<&'a str>,
    pub mode: &'a str,
}

/// A request that waits for the person: what the harness asks to do, and the
/// answers it offers, as Lever's own (`once`, `always`, `no`, `never`) with
/// the harness's name for each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRecord {
    pub id: String,
    pub act: Option<String>,
    pub subagent: Option<String>,
    pub kind: String,
    pub title: String,
    pub detail: Option<String>,
    pub path: Option<String>,
    pub answers: Vec<(String, String)>,
    /// `waiting`, `answered`, or `withdrawn`.
    pub state: String,
    pub answer: Option<String>,
}

/// A request as it begins, before Lever gives it an ID.
pub struct NewRequest<'a> {
    pub thread_id: &'a str,
    pub turn_number: u64,
    pub act: Option<&'a str>,
    pub subagent: Option<&'a str>,
    pub kind: &'a str,
    pub title: &'a str,
    pub detail: Option<&'a str>,
    pub path: Option<&'a str>,
    pub answers: &'a [(String, String)],
}


pub use crate::harness::STARTING_MODE;

#[derive(Debug, Clone)]
pub struct ActRecord {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub state: String,
    pub detail: Option<String>,
}

/// Fields carried when creating or updating an act in Lever's record.
#[derive(Debug, Clone)]
pub struct ActPatch<'a> {
    pub thread_id: &'a str,
    pub turn_number: u64,
    pub harness_call_id: &'a str,
    pub is_new: bool,
    pub kind: Option<&'a str>,
    pub title: Option<&'a str>,
    pub state: Option<&'a str>,
    pub detail: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct NoteRecord {
    pub id: String,
    pub text: String,
}

