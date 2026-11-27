//! Lever's SQLite state store (`lever.sqlite`), kept in the person's home and
//! readable by the person alone.
//!
//! Keeps every thread, its turns, its acts, its requests, and its notes: the
//! updates Lever shows as neither text nor an act (kept as JSON, along with a
//! plain rendering).

use crate::discovery::SessionHistory;
use crate::sys;
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

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

/// The mode a thread starts in when the person has chosen none.
pub const STARTING_MODE: &str = "ask-first";

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

pub struct Store {
    path: PathBuf,
    conn: Connection,
}

impl Store {
    /// Opens or creates the SQLite database at `path` in WAL mode with
    /// person-only (`0600`) permissions on the database and its sidecar files.
    /// Any turn left in `working` state by a prior Lever process is marked
    /// `interrupted`.
    pub fn open(path: &Path, now: i64) -> Result<Self, String> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("could not create database {}: {e}", path.display()))?;
        sys::fchmod(&file, 0o600).map_err(|e| format!("could not set mode on {}: {e}", path.display()))?;
        drop(file);

        let conn = Connection::open(path).map_err(|e| format!("could not open database {}: {e}", path.display()))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;\n\
             PRAGMA synchronous = NORMAL;\n\
             PRAGMA foreign_keys = ON;\n\
             PRAGMA busy_timeout = 5000;\n\
             CREATE TABLE IF NOT EXISTS meta (\n\
                 key TEXT PRIMARY KEY,\n\
                 value TEXT NOT NULL\n\
             );\n\
             INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '10');\n\
             CREATE TABLE IF NOT EXISTS threads (\n\
                 id TEXT PRIMARY KEY,\n\
                 title TEXT NOT NULL,\n\
                 cwd TEXT NOT NULL,\n\
                 harness_name TEXT,\n\
                 harness_session_id TEXT,\n\
                 model TEXT,\n\
                 mode TEXT,\n\
                 effort TEXT,\n\
                 needs_history INTEGER NOT NULL DEFAULT 0,\n\
                 created_at INTEGER NOT NULL,\n\
                 updated_at INTEGER NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS ignored_sessions (\n\
                 harness_session_id TEXT PRIMARY KEY,\n\
                 reason TEXT NOT NULL,\n\
                 created_at INTEGER NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS turns (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,\n\
                 turn_number INTEGER NOT NULL,\n\
                 message TEXT NOT NULL,\n\
                 context TEXT,\n\
                 answer TEXT NOT NULL DEFAULT '',\n\
                 state TEXT NOT NULL,\n\
                 error TEXT,\n\
                 mode TEXT,\n\
                 origin TEXT,\n\
                 created_at INTEGER NOT NULL,\n\
                 ended_at INTEGER,\n\
                 UNIQUE(thread_id, turn_number)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS acts (\n\
                 id TEXT PRIMARY KEY,\n\
                 thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,\n\
                 turn_number INTEGER NOT NULL,\n\
                 harness_call_id TEXT NOT NULL,\n\
                 kind TEXT NOT NULL,\n\
                 title TEXT NOT NULL,\n\
                 state TEXT NOT NULL,\n\
                 detail TEXT,\n\
                 seq INTEGER NOT NULL,\n\
                 UNIQUE(thread_id, turn_number, harness_call_id)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS notes (\n\
                 id TEXT PRIMARY KEY,\n\
                 thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,\n\
                 turn_number INTEGER NOT NULL,\n\
                 raw_json TEXT NOT NULL,\n\
                 plain_text TEXT NOT NULL,\n\
                 after_turn INTEGER NOT NULL DEFAULT 0,\n\
                 seq INTEGER NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS requests (\n\
                 id TEXT PRIMARY KEY,\n\
                 thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,\n\
                 turn_number INTEGER NOT NULL,\n\
                 act_id TEXT,\n\
                 subagent_id TEXT,\n\
                 kind TEXT NOT NULL,\n\
                 title TEXT NOT NULL,\n\
                 detail TEXT,\n\
                 path TEXT,\n\
                 answers TEXT NOT NULL,\n\
                 state TEXT NOT NULL,\n\
                 answer TEXT,\n\
                 seq INTEGER NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS subagents (\n\
                 id TEXT PRIMARY KEY,\n\
                 thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,\n\
                 turn_number INTEGER NOT NULL,\n\
                 harness_session_id TEXT NOT NULL,\n\
                 title TEXT NOT NULL,\n\
                 state TEXT NOT NULL,\n\
                 error TEXT,\n\
                 created_at INTEGER NOT NULL,\n\
                 ended_at INTEGER,\n\
                 seq INTEGER NOT NULL,\n\
                 UNIQUE(thread_id, harness_session_id)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS subagent_messages (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 subagent_id TEXT NOT NULL REFERENCES subagents(id) ON DELETE CASCADE,\n\
                 role TEXT NOT NULL,\n\
                 text TEXT NOT NULL,\n\
                 seq INTEGER NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS steers (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,\n\
                 turn_number INTEGER NOT NULL,\n\
                 text TEXT NOT NULL,\n\
                 context TEXT,\n\
                 at INTEGER NOT NULL,\n\
                 created_at INTEGER NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS queued (\n\
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 id TEXT NOT NULL UNIQUE,\n\
                 thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,\n\
                 turn_number INTEGER NOT NULL,\n\
                 text TEXT NOT NULL,\n\
                 context TEXT,\n\
                 state TEXT NOT NULL,\n\
                 created_at INTEGER NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS subagent_acts (\n\
                 id TEXT PRIMARY KEY,\n\
                 subagent_id TEXT NOT NULL REFERENCES subagents(id) ON DELETE CASCADE,\n\
                 thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,\n\
                 harness_call_id TEXT NOT NULL,\n\
                 kind TEXT NOT NULL,\n\
                 title TEXT NOT NULL,\n\
                 state TEXT NOT NULL,\n\
                 detail TEXT,\n\
                 seq INTEGER NOT NULL,\n\
                 UNIQUE(subagent_id, harness_call_id)\n\
             );",
        )
        .map_err(|e| format!("could not initialize database {}: {e}", path.display()))?;

        // Migrate older databases missing columns added in newer schema versions.
        ensure_column(&conn, "threads", "model", "TEXT")?;
        ensure_column(&conn, "threads", "archived", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "turns", "context", "TEXT")?;
        ensure_column(&conn, "threads", "mode", "TEXT")?;
        ensure_column(&conn, "turns", "mode", "TEXT")?;
        ensure_column(&conn, "threads", "effort", "TEXT")?;
        ensure_column(&conn, "requests", "subagent_id", "TEXT")?;
        ensure_column(&conn, "turns", "origin", "TEXT")?;
        ensure_column(&conn, "threads", "needs_history", "INTEGER NOT NULL DEFAULT 0")?;

        // Any turn or subagent left working by a Lever that ended abruptly is
        // interrupted, and a request that waited went with the harness that asked it.
        conn.execute(
            "UPDATE turns SET state = 'interrupted', ended_at = ?1 WHERE state = 'working'",
            params![now],
        )
        .map_err(|e| format!("could not mark interrupted turns: {e}"))?;
        conn.execute(
            "UPDATE subagents SET state = 'interrupted', ended_at = ?1 WHERE state = 'working'",
            params![now],
        )
        .map_err(|e| format!("could not mark interrupted subagents: {e}"))?;
        conn.execute("UPDATE requests SET state = 'withdrawn' WHERE state = 'waiting'", [])
            .map_err(|e| format!("could not withdraw waiting requests: {e}"))?;
        // A message steered into a turn that a crash interrupted, whose taking
        // the harness never said, waits for the person, who may take it back.
        conn.execute("UPDATE queued SET state = 'queued' WHERE state = 'steering'", [])
            .map_err(|e| format!("could not keep steered messages: {e}"))?;

        tighten_permissions(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            conn,
        })
    }

    pub fn close(self) -> Result<(), String> {
        let path = self.path;
        self.conn
            .close()
            .map_err(|(_, e)| format!("could not close database {}: {e}", path.display()))?;
        tighten_permissions(&path)?;
        Ok(())
    }

    /// Runs `write` in one transaction, which is rolled back when it fails.
    fn all_or_none<T>(&self, what: &str, write: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let all = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("could not {what}: {e}"))?;
        let done = write()?;
        all.commit().map_err(|e| format!("could not {what}: {e}"))?;
        Ok(done)
    }

    fn touch_thread(&self, thread_id: &str, now: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE threads SET updated_at = ?1 WHERE id = ?2",
                params![now, thread_id],
            )
            .map_err(|e| format!("could not touch thread timestamp: {e}"))?;
        Ok(())
    }

    /// Whether the person has enabled OpenCode's free hosted Models.
    /// Defaults to `false` until the person explicitly chooses them.
    pub fn free_hosted_models(&self) -> Result<bool, String> {
        let value: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'free_hosted_models'", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| format!("could not read free_hosted_models setting: {e}"))?;
        Ok(value.as_deref() == Some("true"))
    }

    /// Saves the person's choice for OpenCode's free hosted Models.
    pub fn set_free_hosted_models(&self, enabled: bool) -> Result<(), String> {
        let value = if enabled { "true" } else { "false" };
        self.conn
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('free_hosted_models', ?1)",
                params![value],
            )
            .map_err(|e| format!("could not save free_hosted_models setting: {e}"))?;
        Ok(())
    }

    /// Whether Lever has observed Models offered while free hosted Models were
    /// off (for example, after the person signed in to a provider).
    pub fn provider_models_seen(&self) -> Result<bool, String> {
        let value: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'provider_models_seen'", [], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| format!("could not read provider_models_seen setting: {e}"))?;
        Ok(value.as_deref() == Some("true"))
    }

    /// Saves whether Lever has observed Models offered while free hosted Models
    /// were off.
    pub fn set_provider_models_seen(&self, seen: bool) -> Result<(), String> {
        let value = if seen { "true" } else { "false" };
        self.conn
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('provider_models_seen', ?1)",
                params![value],
            )
            .map_err(|e| format!("could not save provider_models_seen setting: {e}"))?;
        Ok(())
    }

    /// The mode new threads start in: `ask-first` until the person chooses
    /// another.
    pub fn default_mode(&self) -> Result<String, String> {
        let value: Option<String> = self
            .conn
            .query_row("SELECT value FROM meta WHERE key = 'default_mode'", [], |r| r.get(0))
            .optional()
            .map_err(|e| format!("could not read default_mode setting: {e}"))?;
        Ok(value.unwrap_or_else(|| STARTING_MODE.to_string()))
    }

    /// Saves the mode new threads start in.
    pub fn set_default_mode(&self, mode: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('default_mode', ?1)",
                params![mode],
            )
            .map_err(|e| format!("could not save default_mode setting: {e}"))?;
        Ok(())
    }

    /// Begins a turn with the person's message, in a new thread when
    /// `new_thread` is given. The turn, and the thread from it on, is in
    /// `mode` when one is given. All of it is saved or none of it: a thread is
    /// never left with a turn that is working in the record and that Lever
    /// never began.
    pub fn begin_turn(
        &self,
        thread_id: &str,
        new_thread: Option<NewThread<'_>>,
        message: &str,
        context: Option<&str>,
        mode: Option<&str>,
        now: i64,
    ) -> Result<u64, String> {
        let all = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("could not begin a turn: {e}"))?;
        if let Some(thread) = new_thread {
            self.create_thread(thread_id, &thread, now)?;
        }
        if let Some(mode) = mode {
            self.conn
                .execute("UPDATE threads SET mode = ?1 WHERE id = ?2", params![mode, thread_id])
                .map_err(|e| format!("could not set thread mode: {e}"))?;
        }
        let turn = self.create_turn(thread_id, message, context, now)?;
        all.commit().map_err(|e| format!("could not begin a turn: {e}"))?;
        Ok(turn)
    }

    fn create_thread(&self, id: &str, thread: &NewThread<'_>, now: i64) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO threads (id, title, cwd, harness_name, mode, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![id, thread.title, thread.cwd, thread.harness_name, thread.mode, now],
            )
            .map_err(|e| format!("could not create thread: {e}"))?;
        Ok(())
    }

    pub fn thread_meta(&self, thread_id: &str) -> Result<Option<ThreadMeta>, String> {
        type Row = (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            bool,
        );
        let row: Option<Row> = self
            .conn
            .query_row(
                "SELECT cwd, harness_session_id, model, mode, effort, needs_history FROM threads WHERE id = ?1",
                params![thread_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()
            .map_err(|e| format!("could not query thread: {e}"))?;
        let Some((cwd, harness_session_id, model, mode, effort, needs_history)) = row else {
            return Ok(None);
        };
        let latest_turn: Option<u64> = self
            .conn
            .query_row(
                "SELECT MAX(turn_number) FROM turns WHERE thread_id = ?1",
                params![thread_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(|e| format!("could not query latest turn: {e}"))?
            .flatten()
            .map(|n| n as u64);
        let working_turn: Option<u64> = self
            .conn
            .query_row(
                "SELECT turn_number FROM turns WHERE thread_id = ?1 AND state = 'working' \
                 ORDER BY turn_number DESC LIMIT 1",
                params![thread_id],
                |r| r.get::<_, i64>(0),
            )
            .optional()
            .map_err(|e| format!("could not query working turn: {e}"))?
            .map(|n| n as u64);
        Ok(Some(ThreadMeta {
            cwd,
            harness_session_id,
            model,
            effort,
            mode: mode.unwrap_or_else(|| STARTING_MODE.to_string()),
            latest_turn,
            working_turn,
            needs_history,
        }))
    }

    /// Returns `(known_thread_sessions, known_subagents_and_ignored)` from the
    /// database so session discovery skips sessions Lever already owns or has
    /// already classified as subagents/ignored.
    pub fn known_harness_sessions(&self) -> Result<(HashSet<String>, HashSet<String>), String> {
        let mut owned = HashSet::new();
        {
            let mut stmt = self
                .conn
                .prepare("SELECT harness_session_id FROM threads WHERE harness_session_id IS NOT NULL AND harness_session_id <> ''")
                .map_err(|e| format!("could not prepare known thread sessions query: {e}"))?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| format!("could not query known thread sessions: {e}"))?;
            for row in rows {
                owned.insert(row.map_err(|e| format!("could not read known thread session: {e}"))?);
            }
        }
        let mut ignored = HashSet::new();
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT harness_session_id FROM subagents WHERE harness_session_id <> '' \
                     UNION \
                     SELECT harness_session_id FROM ignored_sessions",
                )
                .map_err(|e| format!("could not prepare ignored sessions query: {e}"))?;
            let rows = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .map_err(|e| format!("could not query ignored sessions: {e}"))?;
            for row in rows {
                ignored.insert(row.map_err(|e| format!("could not read ignored session: {e}"))?);
            }
        }
        Ok((owned, ignored))
    }

    /// Remembers a harness session ID that must never be listed as a top-level
    /// thread (a subagent session or a deleted session whose copy may remain).
    pub fn remember_ignored_session(&self, harness_session_id: &str, reason: &str, now: i64) -> Result<(), String> {
        if harness_session_id.is_empty() {
            return Ok(());
        }
        self.conn
            .execute(
                "INSERT OR IGNORE INTO ignored_sessions (harness_session_id, reason, created_at) VALUES (?1, ?2, ?3)",
                params![harness_session_id, reason, now],
            )
            .map_err(|e| format!("could not record ignored session: {e}"))?;
        Ok(())
    }

    /// Looks up a thread by its `harness_session_id`, returning
    /// `(thread_id, title, updated_at, needs_history)` if present.
    pub fn thread_by_harness_session(
        &self,
        harness_session_id: &str,
    ) -> Result<Option<(String, String, i64, bool)>, String> {
        self.conn
            .query_row(
                "SELECT id, title, updated_at, needs_history FROM threads WHERE harness_session_id = ?1 LIMIT 1",
                params![harness_session_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(|e| format!("could not query thread by harness session: {e}"))
    }

    /// Records a newly discovered external session as a thread whose history
    /// will be read from the harness when opened (`needs_history = 1`).
    #[allow(clippy::too_many_arguments)]
    pub fn create_discovered_thread(
        &self,
        id: &str,
        title: &str,
        cwd: &str,
        harness_name: Option<&str>,
        harness_session_id: &str,
        mode: &str,
        created: i64,
        updated: i64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO threads (id, title, cwd, harness_name, harness_session_id, mode, needs_history, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)",
                params![id, title, cwd, harness_name, harness_session_id, mode, created, updated],
            )
            .map_err(|e| format!("could not create discovered thread: {e}"))?;
        Ok(())
    }

    /// Updates the title or timestamp of an unopened discovered thread when
    /// newer metadata arrives from the harness store. Returns `true` if the row
    /// changed.
    pub fn update_discovered_thread(&self, thread_id: &str, title: Option<&str>, updated: i64) -> Result<bool, String> {
        let changed = self
            .conn
            .execute(
                "UPDATE threads \
                 SET title = COALESCE(?1, title), updated_at = MAX(updated_at, ?2) \
                 WHERE id = ?3 AND needs_history = 1 \
                   AND ((?1 IS NOT NULL AND title <> ?1) OR updated_at < ?2)",
                params![title, updated, thread_id],
            )
            .map_err(|e| format!("could not update discovered thread: {e}"))?;
        Ok(changed > 0)
    }

    /// Atomically writes an imported [`SessionHistory`] into a discovered
    /// thread's record and clears `needs_history`. All turns, acts, notes, and
    /// subagents are committed together or rolled back on failure.
    pub fn import_thread_history(
        &self,
        thread_id: &str,
        history: &SessionHistory,
        fallback_title: Option<&str>,
        now: i64,
    ) -> Result<(), String> {
        self.all_or_none("import thread history", || {
            let row: Option<(String, i64, i64, bool)> = self
                .conn
                .query_row(
                    "SELECT title, created_at, updated_at, needs_history FROM threads WHERE id = ?1",
                    params![thread_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .optional()
                .map_err(|e| format!("could not query thread for history import: {e}"))?;
            let Some((cur_title, created_at, updated_at, needs_history)) = row else {
                return Err(format!("there is no thread {thread_id}"));
            };
            if !needs_history {
                return Ok(());
            }

            let turn_time = updated_at.max(created_at);
            let mut act_seq: i64 = 0;
            let mut note_seq: i64 = 0;
            let mut sub_seq: i64 = 0;

            for (idx, turn) in history.turns.iter().enumerate() {
                let turn_number = (idx + 1) as i64;
                self.conn
                    .execute(
                        "INSERT INTO turns (thread_id, turn_number, message, context, answer, state, mode, origin, created_at, ended_at) \
                         VALUES (?1, ?2, ?3, NULL, ?4, 'done', NULL, ?5, ?6, ?7)",
                        params![
                            thread_id,
                            turn_number,
                            turn.message,
                            turn.answer,
                            turn.origin.as_deref(),
                            created_at,
                            turn_time,
                        ],
                    )
                    .map_err(|e| format!("could not insert imported turn: {e}"))?;

                for act in &turn.acts {
                    act_seq += 1;
                    let act_id = format!("act-{}", sys::random_hex(6));
                    self.conn
                        .execute(
                            "INSERT OR IGNORE INTO acts (id, thread_id, turn_number, harness_call_id, kind, title, state, detail, seq) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                            params![
                                act_id,
                                thread_id,
                                turn_number,
                                act.harness_call_id,
                                act.kind,
                                act.title,
                                act.state,
                                act.detail.as_deref(),
                                act_seq,
                            ],
                        )
                        .map_err(|e| format!("could not insert imported act: {e}"))?;
                }

                for (raw_json, plain_text) in &turn.notes {
                    note_seq += 1;
                    let note_id = format!("note-{}", sys::random_hex(6));
                    self.conn
                        .execute(
                            "INSERT INTO notes (id, thread_id, turn_number, raw_json, plain_text, after_turn, seq) \
                             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6)",
                            params![note_id, thread_id, turn_number, raw_json, plain_text, note_seq],
                        )
                        .map_err(|e| format!("could not insert imported note: {e}"))?;
                }

                for sub in &turn.subagents {
                    sub_seq += 1;
                    let sub_id = format!("sub-{}", sys::random_hex(6));
                    let inserted = self
                        .conn
                        .execute(
                            "INSERT OR IGNORE INTO subagents (id, thread_id, turn_number, harness_session_id, title, state, error, created_at, ended_at, seq) \
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                            params![
                                sub_id,
                                thread_id,
                                turn_number,
                                sub.harness_session_id,
                                sub.title,
                                sub.state,
                                sub.error.as_deref(),
                                created_at,
                                turn_time,
                                sub_seq,
                            ],
                        )
                        .map_err(|e| format!("could not insert imported subagent: {e}"))?;
                    if inserted > 0
                        && let Some(prompt) = sub.prompt.as_deref().filter(|p| !p.is_empty())
                    {
                        self.conn
                            .execute(
                                "INSERT INTO subagent_messages (subagent_id, role, text, seq) VALUES (?1, 'prompt', ?2, 1)",
                                params![sub_id, prompt],
                            )
                            .map_err(|e| format!("could not insert imported subagent prompt: {e}"))?;
                    }
                }
            }

            let new_title = history
                .title
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    (cur_title.trim().is_empty() || cur_title == "Thread")
                        .then_some(fallback_title)
                        .flatten()
                });
            self.conn
                .execute(
                    "UPDATE threads \
                     SET needs_history = 0, \
                         title = COALESCE(?1, title), \
                         model = COALESCE(model, ?2), \
                         effort = COALESCE(effort, ?3), \
                         updated_at = MAX(updated_at, ?4) \
                     WHERE id = ?5",
                    params![
                        new_title,
                        history.model.as_deref(),
                        history.effort.as_deref(),
                        now.min(turn_time.max(created_at)),
                        thread_id,
                    ],
                )
                .map_err(|e| format!("could not finalize imported thread: {e}"))?;
            Ok(())
        })
    }

    pub fn set_thread_mode(&self, thread_id: &str, mode: &str, now: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE threads SET mode = ?1, updated_at = ?2 WHERE id = ?3",
                params![mode, now, thread_id],
            )
            .map_err(|e| format!("could not update thread mode: {e}"))?;
        Ok(())
    }

    pub fn set_thread_model(&self, thread_id: &str, model: &str, now: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE threads SET model = ?1, updated_at = ?2 WHERE id = ?3",
                params![model, now, thread_id],
            )
            .map_err(|e| format!("could not update thread model: {e}"))?;
        Ok(())
    }

    /// Saves a thread's effort, or that it has none.
    pub fn set_thread_effort(&self, thread_id: &str, effort: Option<&str>, now: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE threads SET effort = ?1, updated_at = ?2 WHERE id = ?3",
                params![effort, now, thread_id],
            )
            .map_err(|e| format!("could not update thread effort: {e}"))?;
        Ok(())
    }

    /// Records the session the harness gave a thread. A session is one
    /// thread's alone, so any other thread the record has on this ID loses
    /// it: the harness has given the ID out again, and what it now holds
    /// under it is this thread's. That thread's ID is left empty, which says
    /// it had a session and has none to load.
    pub fn set_harness_session_id(&self, thread_id: &str, session_id: &str) -> Result<(), String> {
        let both = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("could not store harness session id: {e}"))?;
        self.conn
            .execute(
                "UPDATE threads SET harness_session_id = '' WHERE harness_session_id = ?1 AND id <> ?2",
                params![session_id, thread_id],
            )
            .map_err(|e| format!("could not store harness session id: {e}"))?;
        // A session this one replaces is Lever's still, and never a thread
        // of its own when a listing finds it.
        self.conn
            .execute(
                "INSERT OR IGNORE INTO ignored_sessions (harness_session_id, reason, created_at)
                 SELECT harness_session_id, 'replaced', strftime('%s', 'now') FROM threads
                 WHERE id = ?1 AND harness_session_id IS NOT NULL AND harness_session_id NOT IN ('', ?2)",
                params![thread_id, session_id],
            )
            .map_err(|e| format!("could not store harness session id: {e}"))?;
        self.conn
            .execute(
                "UPDATE threads SET harness_session_id = ?1 WHERE id = ?2",
                params![session_id, thread_id],
            )
            .map_err(|e| format!("could not store harness session id: {e}"))?;
        both.commit()
            .map_err(|e| format!("could not store harness session id: {e}"))
    }

    pub fn update_thread_title(&self, thread_id: &str, title: &str, now: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE threads SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![title, now, thread_id],
            )
            .map_err(|e| format!("could not update thread title: {e}"))?;
        Ok(())
    }

    /// Begins a turn the harness began itself, with no message from the
    /// person: what it says after a turn of the thread ended is kept apart
    /// from that turn.
    pub fn begin_harness_turn(&self, thread_id: &str, now: i64) -> Result<u64, String> {
        let all = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("could not begin a turn: {e}"))?;
        let turn = self.create_turn(thread_id, "", None, now)?;
        self.conn
            .execute(
                "UPDATE turns SET origin = 'harness' WHERE thread_id = ?1 AND turn_number = ?2",
                params![thread_id, turn as i64],
            )
            .map_err(|e| format!("could not mark the turn the harness began: {e}"))?;
        all.commit().map_err(|e| format!("could not begin a turn: {e}"))?;
        Ok(turn)
    }

    /// Makes a turn the harness began, and Lever ended as done when the
    /// harness went quiet, work again, when it is the thread's newest. Says
    /// whether it did.
    pub fn reopen_harness_turn(&self, thread_id: &str, turn_number: u64, now: i64) -> Result<bool, String> {
        self.all_or_none("reopen the turn", || {
            self.reopen_harness_turn_unguarded(thread_id, turn_number, now)
        })
    }

    fn reopen_harness_turn_unguarded(&self, thread_id: &str, turn_number: u64, now: i64) -> Result<bool, String> {
        let changed = self
            .conn
            .execute(
                "UPDATE turns SET state = 'working', ended_at = NULL \
                 WHERE thread_id = ?1 AND turn_number = ?2 AND origin = 'harness' AND state = 'done' \
                 AND turn_number = (SELECT MAX(turn_number) FROM turns WHERE thread_id = ?1)",
                params![thread_id, turn_number as i64],
            )
            .map_err(|e| format!("could not reopen the turn: {e}"))?;
        if changed > 0 {
            self.touch_thread(thread_id, now)?;
        }
        Ok(changed > 0)
    }

    /// A thread's newest turn: its number, its state, and whether the
    /// harness began it.
    pub fn latest_turn(&self, thread_id: &str) -> Result<Option<(u64, String, bool)>, String> {
        self.conn
            .query_row(
                "SELECT turn_number, state, origin = 'harness' FROM turns WHERE thread_id = ?1 \
                 ORDER BY turn_number DESC LIMIT 1",
                params![thread_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)? as u64,
                        r.get(1)?,
                        r.get::<_, Option<bool>>(2)?.unwrap_or(false),
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("could not query the newest turn: {e}"))
    }

    fn create_turn(&self, thread_id: &str, message: &str, context: Option<&str>, now: i64) -> Result<u64, String> {
        let next: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(turn_number), 0) + 1 FROM turns WHERE thread_id = ?1",
                params![thread_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("could not compute next turn number: {e}"))?;
        self.conn
            .execute(
                "INSERT INTO turns (thread_id, turn_number, message, context, answer, state, mode, created_at) \
                 VALUES (?1, ?2, ?3, ?4, '', 'working', (SELECT mode FROM threads WHERE id = ?1), ?5)",
                params![thread_id, next, message, context, now],
            )
            .map_err(|e| format!("could not insert turn: {e}"))?;
        self.touch_thread(thread_id, now)?;
        Ok(next as u64)
    }

    pub fn append_answer(&self, thread_id: &str, turn_number: u64, chunk: &str, now: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE turns SET answer = answer || ?1 WHERE thread_id = ?2 AND turn_number = ?3",
                params![chunk, thread_id, turn_number as i64],
            )
            .map_err(|e| format!("could not append answer: {e}"))?;
        self.touch_thread(thread_id, now)?;
        Ok(())
    }

    pub fn finish_turn(
        &self,
        thread_id: &str,
        turn_number: u64,
        state: &str,
        error: Option<&str>,
        now: i64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE turns SET state = ?1, error = ?2, ended_at = ?3 \
                 WHERE thread_id = ?4 AND turn_number = ?5",
                params![state, error, now, thread_id, turn_number as i64],
            )
            .map_err(|e| format!("could not finish turn: {e}"))?;
        self.touch_thread(thread_id, now)?;
        Ok(())
    }

    /// The harness's tool call IDs of a thread's recorded acts (including its
    /// subagents' acts).
    pub fn harness_call_ids(&self, thread_id: &str) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DISTINCT harness_call_id FROM acts WHERE thread_id = ?1 \
                 UNION \
                 SELECT DISTINCT harness_call_id FROM subagent_acts WHERE thread_id = ?1",
            )
            .map_err(|e| format!("could not prepare tool call ID query: {e}"))?;
        let rows = stmt
            .query_map(params![thread_id], |r| r.get::<_, String>(0))
            .map_err(|e| format!("could not query tool call IDs: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("could not read tool call ID row: {e}"))
    }

    /// The harness's session IDs of a thread's recorded subagents, used to
    /// redact them after a harness restart and never list them as threads.
    pub fn subagent_harness_session_ids(&self, thread_id: &str) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT harness_session_id FROM subagents WHERE thread_id = ?1")
            .map_err(|e| format!("could not prepare subagent session ID query: {e}"))?;
        let rows = stmt
            .query_map(params![thread_id], |r| r.get::<_, String>(0))
            .map_err(|e| format!("could not query subagent session IDs: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("could not read subagent session ID row: {e}"))
    }

    /// Marks every currently-working turn and subagent as `interrupted` and
    /// returns the affected `(thread_id, turn_number)` pairs.
    pub fn interrupt_all_working(&self, now: i64) -> Result<Vec<(String, u64)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT thread_id, turn_number FROM turns WHERE state = 'working' ORDER BY id ASC")
            .map_err(|e| format!("could not prepare working turns query: {e}"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64)))
            .map_err(|e| format!("could not query working turns: {e}"))?;
        let mut interrupted = Vec::new();
        for row in rows {
            interrupted.push(row.map_err(|e| format!("could not read working turn row: {e}"))?);
        }
        if !interrupted.is_empty() {
            self.conn
                .execute(
                    "UPDATE turns SET state = 'interrupted', ended_at = ?1 WHERE state = 'working'",
                    params![now],
                )
                .map_err(|e| format!("could not mark working turns interrupted: {e}"))?;
        }
        self.conn
            .execute(
                "UPDATE subagents SET state = 'interrupted', ended_at = ?1 WHERE state = 'working'",
                params![now],
            )
            .map_err(|e| format!("could not mark working subagents interrupted: {e}"))?;
        Ok(interrupted)
    }

    /// Inserts or updates an act and returns `(recorded_turn, ActRecord)` with
    /// Lever's own act ID.
    ///
    /// When `patch.is_new` is true (`ToolCall`), lookup is scoped to the
    /// current `(thread_id, turn_number, harness_call_id)` so a call ID reused
    /// on a later turn creates a distinct act. When `patch.is_new` is false
    /// (`ToolCallUpdate`), lookup matches the most recent act with that
    /// `harness_call_id` on the thread so post-turn updates find their act.
    pub fn upsert_act(&self, patch: &ActPatch<'_>, now: i64) -> Result<(u64, ActRecord), String> {
        let existing: Option<(String, u64, String, String, String, Option<String>)> = if patch.is_new {
            self.conn
                .query_row(
                    "SELECT id, turn_number, kind, title, state, detail FROM acts \
                     WHERE thread_id = ?1 AND turn_number = ?2 AND harness_call_id = ?3",
                    params![patch.thread_id, patch.turn_number as i64, patch.harness_call_id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get::<_, i64>(1)? as u64,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| format!("could not query act: {e}"))?
        } else {
            self.conn
                .query_row(
                    "SELECT id, turn_number, kind, title, state, detail FROM acts \
                     WHERE thread_id = ?1 AND harness_call_id = ?2 \
                     ORDER BY turn_number DESC, seq DESC LIMIT 1",
                    params![patch.thread_id, patch.harness_call_id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get::<_, i64>(1)? as u64,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    },
                )
                .optional()
                .map_err(|e| format!("could not query act: {e}"))?
        };

        let (recorded_turn, record) =
            if let Some((id, orig_turn, cur_kind, cur_title, cur_state, cur_detail)) = existing {
                let new_kind = patch.kind.map(str::to_string).unwrap_or(cur_kind);
                let new_title = patch.title.map(str::to_string).unwrap_or(cur_title);
                let new_state = patch.state.map(str::to_string).unwrap_or(cur_state);
                let new_detail = patch.detail.map(str::to_string).or(cur_detail);
                self.conn
                    .execute(
                        "UPDATE acts SET kind = ?1, title = ?2, state = ?3, detail = ?4 WHERE id = ?5",
                        params![new_kind, new_title, new_state, new_detail, id],
                    )
                    .map_err(|e| format!("could not update act: {e}"))?;
                (
                    orig_turn,
                    ActRecord {
                        id,
                        kind: new_kind,
                        title: new_title,
                        state: new_state,
                        detail: new_detail,
                    },
                )
            } else {
                let seq: i64 = self
                    .conn
                    .query_row(
                        "SELECT COALESCE(MAX(seq), 0) + 1 FROM acts WHERE thread_id = ?1",
                        params![patch.thread_id],
                        |r| r.get(0),
                    )
                    .map_err(|e| format!("could not compute act sequence: {e}"))?;
                let id = format!("act-{}", sys::random_hex(6));
                let new_kind = patch.kind.unwrap_or("other").to_string();
                let new_title = patch.title.unwrap_or("Act").to_string();
                let new_state = patch.state.unwrap_or("pending").to_string();
                let new_detail = patch.detail.map(str::to_string);
                self.conn
                .execute(
                    "INSERT INTO acts (id, thread_id, turn_number, harness_call_id, kind, title, state, detail, seq) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        id,
                        patch.thread_id,
                        patch.turn_number as i64,
                        patch.harness_call_id,
                        new_kind,
                        new_title,
                        new_state,
                        new_detail,
                        seq
                    ],
                )
                .map_err(|e| format!("could not insert act: {e}"))?;
                (
                    patch.turn_number,
                    ActRecord {
                        id,
                        kind: new_kind,
                        title: new_title,
                        state: new_state,
                        detail: new_detail,
                    },
                )
            };

        self.touch_thread(patch.thread_id, now)?;
        Ok((recorded_turn, record))
    }

    /// Keeps a message the harness took into a turn under way, at the place
    /// in the turn's answer it had come to.
    pub fn insert_steer(
        &self,
        thread_id: &str,
        turn_number: u64,
        text: &str,
        context: Option<&str>,
        now: i64,
    ) -> Result<(), String> {
        let kept = self
            .conn
            .execute(
                "INSERT INTO steers (thread_id, turn_number, text, context, at, created_at) \
                 SELECT ?1, ?2, ?3, ?4, length(CAST(answer AS BLOB)), ?5 FROM turns \
                 WHERE thread_id = ?1 AND turn_number = ?2",
                params![thread_id, turn_number as i64, text, context, now],
            )
            .map_err(|e| format!("could not insert steered message: {e}"))?;
        if kept == 0 {
            return Err(format!("could not insert steered message: no turn {turn_number}"));
        }
        self.touch_thread(thread_id, now)
    }

    /// Keeps a message that waits behind turn `turn_number` of a thread, and
    /// gives it Lever's ID. One in `state` `steering` is a message steered
    /// into the turn whose place in the queue is kept until the harness says
    /// whether it took it: it is not shown, and does not go.
    pub fn insert_queued(
        &self,
        thread_id: &str,
        turn_number: u64,
        text: &str,
        context: Option<&str>,
        state: &str,
        now: i64,
    ) -> Result<QueuedRecord, String> {
        let id = format!("q-{}", sys::random_hex(6));
        self.conn
            .execute(
                "INSERT INTO queued (id, thread_id, turn_number, text, context, state, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![id, thread_id, turn_number as i64, text, context, state, now],
            )
            .map_err(|e| format!("could not queue the message: {e}"))?;
        self.touch_thread(thread_id, now)?;
        Ok(QueuedRecord {
            id,
            text: text.to_string(),
            context: context.and_then(|c| serde_json::from_str(c).ok()),
            state: state.to_string(),
        })
    }

    /// A thread's queued messages in the order sent, each with the turn it
    /// waits behind: those that wait, and with `dropped` those a Stop dropped.
    pub fn queued_of(&self, thread_id: &str, dropped: bool) -> Result<Vec<(u64, QueuedRecord)>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT turn_number, id, text, context, state FROM queued \
                 WHERE thread_id = ?1 AND (state = 'queued' OR (?2 AND state = 'dropped')) ORDER BY seq ASC",
            )
            .map_err(|e| format!("could not prepare queued query: {e}"))?;
        let rows = stmt
            .query_map(params![thread_id, dropped], |r| {
                let raw_context: Option<String> = r.get(3)?;
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    QueuedRecord {
                        id: r.get(1)?,
                        text: r.get(2)?,
                        context: raw_context.and_then(|s| serde_json::from_str(&s).ok()),
                        state: r.get(4)?,
                    },
                ))
            })
            .map_err(|e| format!("could not query queued messages: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("could not read queued row: {e}"))?);
        }
        Ok(out)
    }

    /// Takes a message that waits out of the record, as when it goes as its
    /// own turn or the person takes it back. Says whether one waited.
    pub fn remove_queued(&self, thread_id: &str, id: &str) -> Result<bool, String> {
        let removed = self
            .conn
            .execute(
                "DELETE FROM queued WHERE thread_id = ?1 AND id = ?2 AND state = 'queued'",
                params![thread_id, id],
            )
            .map_err(|e| format!("could not take the queued message out: {e}"))?;
        Ok(removed > 0)
    }

    /// Moves a queued message from one state to another, as from `queued` to
    /// `going` while it begins its turn, and says whether it was in the first.
    pub fn set_queued_state(&self, thread_id: &str, id: &str, from: &str, to: &str) -> Result<bool, String> {
        let moved = self
            .conn
            .execute(
                "UPDATE queued SET state = ?4 WHERE thread_id = ?1 AND id = ?2 AND state = ?3",
                params![thread_id, id, from, to],
            )
            .map_err(|e| format!("could not change the queued message: {e}"))?;
        Ok(moved > 0)
    }

    /// Takes a message that went as its own turn out of the record.
    pub fn forget_queued(&self, id: &str) -> Result<(), String> {
        self.conn
            .execute("DELETE FROM queued WHERE id = ?1", params![id])
            .map_err(|e| format!("could not forget the queued message: {e}"))?;
        Ok(())
    }

    /// Drops what waits behind a thread's turn (or, with none, all it has
    /// waiting), as a Stop does. They stay in the record as not sent.
    pub fn drop_queued(&self, thread_id: &str, turn_number: Option<u64>) -> Result<Vec<(u64, QueuedRecord)>, String> {
        let waiting: Vec<(u64, QueuedRecord)> = self
            .queued_of(thread_id, false)?
            .into_iter()
            .filter(|(turn, _)| turn_number.is_none_or(|t| *turn == t))
            .collect();
        for (_, q) in &waiting {
            self.conn
                .execute("UPDATE queued SET state = 'dropped' WHERE id = ?1", params![q.id])
                .map_err(|e| format!("could not drop the queued message: {e}"))?;
        }
        Ok(waiting
            .into_iter()
            .map(|(turn, mut q)| {
                q.state = "dropped".to_string();
                (turn, q)
            })
            .collect())
    }

    /// Stores a note: a harness update's JSON in `notes.raw_json` along with
    /// its plain text rendering. Returns Lever's own note record.
    pub fn insert_note(
        &self,
        thread_id: &str,
        turn_number: u64,
        raw_json: &str,
        plain_text: &str,
        after_turn: bool,
        now: i64,
    ) -> Result<NoteRecord, String> {
        let seq: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM notes WHERE thread_id = ?1",
                params![thread_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("could not compute note sequence: {e}"))?;
        let id = format!("note-{}", sys::random_hex(6));
        self.conn
            .execute(
                "INSERT INTO notes (id, thread_id, turn_number, raw_json, plain_text, after_turn, seq) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    thread_id,
                    turn_number as i64,
                    raw_json,
                    plain_text,
                    i64::from(after_turn),
                    seq
                ],
            )
            .map_err(|e| format!("could not insert note: {e}"))?;
        self.touch_thread(thread_id, now)?;
        Ok(NoteRecord {
            id,
            text: plain_text.to_string(),
        })
    }

    pub fn list_threads(&self) -> Result<Vec<ThreadSummary>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.id, t.title, t.cwd, t.model, t.created_at, t.updated_at, \
                        (SELECT COUNT(*) FROM turns WHERE thread_id = t.id), \
                        (SELECT state FROM turns WHERE thread_id = t.id ORDER BY turn_number DESC LIMIT 1), \
                        EXISTS (SELECT 1 FROM requests WHERE thread_id = t.id AND state = 'waiting'), \
                        t.mode, t.archived, t.effort, \
                        EXISTS (SELECT 1 FROM subagents WHERE thread_id = t.id AND state = 'working'), \
                        (SELECT COUNT(*) FROM subagents WHERE thread_id = t.id) \
                 FROM threads t \
                 ORDER BY t.updated_at DESC, t.rowid DESC",
            )
            .map_err(|e| format!("could not prepare thread list query: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                let turn_state: String = r.get::<_, Option<String>>(7)?.unwrap_or_else(|| "idle".to_string());
                let subagent_working: bool = r.get(12)?;
                let working = turn_state == "working" || subagent_working;
                let state = if working { "working".to_string() } else { turn_state };
                Ok(ThreadSummary {
                    archived: r.get(10)?,
                    id: r.get(0)?,
                    title: r.get(1)?,
                    cwd: r.get(2)?,
                    model: r.get(3)?,
                    effort: r.get(11)?,
                    started: r.get(4)?,
                    updated: r.get(5)?,
                    turns: r.get::<_, i64>(6)? as u64,
                    state,
                    working,
                    waiting: r.get(8)?,
                    mode: r
                        .get::<_, Option<String>>(9)?
                        .unwrap_or_else(|| STARTING_MODE.to_string()),
                    subagents: r.get::<_, i64>(13)? as u64,
                })
            })
            .map_err(|e| format!("could not query threads: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("could not read thread row: {e}"))?);
        }
        Ok(out)
    }

    pub fn archive_thread(&self, thread: &str, archived: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE threads SET archived = ?2 WHERE id = ?1",
                params![thread, archived],
            )
            .map_err(|e| format!("could not archive thread: {e}"))?;
        Ok(())
    }

    /// Deletes a thread, and remembers its harness session and its
    /// subagents' sessions as deleted in the same transaction, so a listing
    /// of the harness's sessions taken before the delete never brings them
    /// back as threads.
    pub fn delete_thread(&self, thread: &str, now: i64) -> Result<(), String> {
        self.all_or_none("delete thread", || {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO ignored_sessions (harness_session_id, reason, created_at)
                     SELECT harness_session_id, 'deleted', ?2 FROM threads
                     WHERE id = ?1 AND harness_session_id IS NOT NULL AND harness_session_id <> ''
                     UNION
                     SELECT harness_session_id, 'deleted', ?2 FROM subagents
                     WHERE thread_id = ?1 AND harness_session_id <> ''",
                    params![thread, now],
                )
                .map_err(|e| format!("could not delete thread: {e}"))?;
            self.conn
                .execute("DELETE FROM threads WHERE id = ?1", params![thread])
                .map_err(|e| format!("could not delete thread: {e}"))?;
            Ok(())
        })
    }

    pub fn get_thread(&self, thread_id: &str) -> Result<Option<ThreadRecord>, String> {
        type Row = (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            i64,
            i64,
            Option<String>,
        );
        let row: Option<Row> = self
            .conn
            .query_row(
                "SELECT id, title, cwd, model, mode, created_at, updated_at, effort FROM threads WHERE id = ?1",
                params![thread_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("could not query thread: {e}"))?;
        let Some((id, title, cwd, model, mode, started, updated, effort)) = row else {
            return Ok(None);
        };

        let mut turn_stmt = self
            .conn
            .prepare(
                "SELECT turn_number, message, context, answer, state, error, mode, created_at, ended_at, origin \
                 FROM turns WHERE thread_id = ?1 ORDER BY turn_number ASC",
            )
            .map_err(|e| format!("could not prepare turns query: {e}"))?;
        let turn_rows = turn_stmt
            .query_map(params![thread_id], |r| {
                let raw_context: Option<String> = r.get(2)?;
                let context = raw_context.and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
                Ok(TurnRecord {
                    turn: r.get::<_, i64>(0)? as u64,
                    message: r.get(1)?,
                    context,
                    answer: r.get(3)?,
                    state: r.get(4)?,
                    error: r.get(5)?,
                    mode: r.get(6)?,
                    started: r.get(7)?,
                    ended: r.get(8)?,
                    origin: r.get(9)?,
                    acts: Vec::new(),
                    requests: Vec::new(),
                    notes: Vec::new(),
                    subagents: Vec::new(),
                    steered: Vec::new(),
                    queued: Vec::new(),
                })
            })
            .map_err(|e| format!("could not query turns: {e}"))?;
        let mut turns = Vec::new();
        for row in turn_rows {
            turns.push(row.map_err(|e| format!("could not read turn row: {e}"))?);
        }

        let mut act_stmt = self
            .conn
            .prepare(
                "SELECT turn_number, id, kind, title, state, detail \
                 FROM acts WHERE thread_id = ?1 ORDER BY seq ASC",
            )
            .map_err(|e| format!("could not prepare acts query: {e}"))?;
        let act_rows = act_stmt
            .query_map(params![thread_id], |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    ActRecord {
                        id: r.get(1)?,
                        kind: r.get(2)?,
                        title: r.get(3)?,
                        state: r.get(4)?,
                        detail: r.get(5)?,
                    },
                ))
            })
            .map_err(|e| format!("could not query acts: {e}"))?;
        for row in act_rows {
            let (turn_number, act) = row.map_err(|e| format!("could not read act row: {e}"))?;
            if let Some(turn) = turns.iter_mut().find(|t| t.turn == turn_number) {
                turn.acts.push(act);
            }
        }

        for (turn_number, request) in self.requests_of(thread_id, None)? {
            if let Some(turn) = turns.iter_mut().find(|t| t.turn == turn_number) {
                turn.requests.push(request);
            }
        }

        let mut note_stmt = self
            .conn
            .prepare(
                "SELECT turn_number, id, plain_text \
                 FROM notes WHERE thread_id = ?1 ORDER BY seq ASC",
            )
            .map_err(|e| format!("could not prepare notes query: {e}"))?;
        let note_rows = note_stmt
            .query_map(params![thread_id], |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    NoteRecord {
                        id: r.get(1)?,
                        text: r.get(2)?,
                    },
                ))
            })
            .map_err(|e| format!("could not query notes: {e}"))?;
        for row in note_rows {
            let (turn_number, note) = row.map_err(|e| format!("could not read note row: {e}"))?;
            if let Some(turn) = turns.iter_mut().find(|t| t.turn == turn_number) {
                turn.notes.push(note);
            }
        }

        let mut sub_stmt = self
            .conn
            .prepare(
                "SELECT turn_number, id, title, state, error, created_at, ended_at \
                 FROM subagents WHERE thread_id = ?1 ORDER BY seq ASC",
            )
            .map_err(|e| format!("could not prepare subagents query: {e}"))?;
        let sub_rows = sub_stmt
            .query_map(params![thread_id], |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    SubagentRecord {
                        id: r.get(1)?,
                        title: r.get(2)?,
                        state: r.get(3)?,
                        error: r.get(4)?,
                        started: r.get(5)?,
                        ended: r.get(6)?,
                        messages: Vec::new(),
                        acts: Vec::new(),
                    },
                ))
            })
            .map_err(|e| format!("could not query subagents: {e}"))?;
        for row in sub_rows {
            let (turn_number, mut sub) = row.map_err(|e| format!("could not read subagent row: {e}"))?;
            sub.messages = self.subagent_messages(&sub.id)?;
            sub.acts = self.subagent_acts(&sub.id)?;
            if let Some(turn) = turns.iter_mut().find(|t| t.turn == turn_number) {
                turn.subagents.push(sub);
            }
        }

        let mut steer_stmt = self
            .conn
            .prepare("SELECT turn_number, text, context, at FROM steers WHERE thread_id = ?1 ORDER BY id ASC")
            .map_err(|e| format!("could not prepare steers query: {e}"))?;
        let steer_rows = steer_stmt
            .query_map(params![thread_id], |r| {
                let raw_context: Option<String> = r.get(2)?;
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    SteerRecord {
                        text: r.get(1)?,
                        context: raw_context.and_then(|s| serde_json::from_str(&s).ok()),
                        at: r.get::<_, i64>(3)? as u64,
                    },
                ))
            })
            .map_err(|e| format!("could not query steers: {e}"))?;
        for row in steer_rows {
            let (turn_number, steer) = row.map_err(|e| format!("could not read steer row: {e}"))?;
            if let Some(turn) = turns.iter_mut().find(|t| t.turn == turn_number) {
                turn.steered.push(steer);
            }
        }

        for (turn_number, queued) in self.queued_of(thread_id, true)? {
            if let Some(turn) = turns.iter_mut().find(|t| t.turn == turn_number) {
                turn.queued.push(queued);
            }
        }

        let working = turns.last().is_some_and(|t| t.state == "working")
            || turns.iter().any(|t| t.subagents.iter().any(|s| s.state == "working"));
        Ok(Some(ThreadRecord {
            id,
            title,
            cwd,
            model,
            effort,
            mode: mode.unwrap_or_else(|| STARTING_MODE.to_string()),
            started,
            updated,
            working,
            turns,
        }))
    }

    /// Keeps a request that waits for the person, and gives it Lever's ID.
    pub fn insert_request(&self, request: &NewRequest<'_>, now: i64) -> Result<RequestRecord, String> {
        // All of it, or none: a request the record holds is one Lever holds.
        let all = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("could not keep a request: {e}"))?;
        let seq: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM requests WHERE thread_id = ?1",
                params![request.thread_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("could not compute request sequence: {e}"))?;
        let id = format!("r-{}", sys::random_hex(6));
        let answers = serde_json::to_string(request.answers).map_err(|e| format!("could not encode answers: {e}"))?;
        self.conn
            .execute(
                "INSERT INTO requests (id, thread_id, turn_number, act_id, subagent_id, kind, title, detail, path, answers, state, seq) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'waiting', ?11)",
                params![
                    id,
                    request.thread_id,
                    request.turn_number as i64,
                    request.act,
                    request.subagent,
                    request.kind,
                    request.title,
                    request.detail,
                    request.path,
                    answers,
                    seq
                ],
            )
            .map_err(|e| format!("could not insert request: {e}"))?;
        self.touch_thread(request.thread_id, now)?;
        all.commit().map_err(|e| format!("could not keep a request: {e}"))?;
        Ok(RequestRecord {
            id,
            act: request.act.map(str::to_string),
            subagent: request.subagent.map(str::to_string),
            kind: request.kind.to_string(),
            title: request.title.to_string(),
            detail: request.detail.map(str::to_string),
            path: request.path.map(str::to_string),
            answers: request.answers.to_vec(),
            state: "waiting".to_string(),
            answer: None,
        })
    }

    /// Ends a request that waits, as `answered` with `answer` or as
    /// `withdrawn`. Returns its turn and record, or `None` when no request
    /// with that ID waits.
    pub fn end_request(
        &self,
        request_id: &str,
        state: &str,
        answer: Option<&str>,
        now: i64,
    ) -> Result<Option<(String, u64, RequestRecord)>, String> {
        let changed = self
            .conn
            .execute(
                "UPDATE requests SET state = ?1, answer = ?2 WHERE id = ?3 AND state = 'waiting'",
                params![state, answer, request_id],
            )
            .map_err(|e| format!("could not end request: {e}"))?;
        if changed == 0 {
            return Ok(None);
        }
        let thread_id: String = self
            .conn
            .query_row(
                "SELECT thread_id FROM requests WHERE id = ?1",
                params![request_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("could not query request: {e}"))?;
        self.touch_thread(&thread_id, now)?;
        let found = self
            .requests_of(&thread_id, Some(request_id))?
            .into_iter()
            .next()
            .map(|(turn, record)| (thread_id, turn, record));
        Ok(found)
    }

    /// A thread's requests, oldest first, each with its turn: all of them, or
    /// the one with `request_id`.
    pub fn requests_of(&self, thread_id: &str, request_id: Option<&str>) -> Result<Vec<(u64, RequestRecord)>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT turn_number, id, act_id, subagent_id, kind, title, detail, path, answers, state, answer \
                 FROM requests WHERE thread_id = ?1 AND (?2 IS NULL OR id = ?2) ORDER BY seq ASC",
            )
            .map_err(|e| format!("could not prepare requests query: {e}"))?;
        let rows = stmt
            .query_map(params![thread_id, request_id], |r| {
                let answers: String = r.get(8)?;
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    RequestRecord {
                        id: r.get(1)?,
                        act: r.get(2)?,
                        subagent: r.get(3)?,
                        kind: r.get(4)?,
                        title: r.get(5)?,
                        detail: r.get(6)?,
                        path: r.get(7)?,
                        answers: serde_json::from_str(&answers).unwrap_or_default(),
                        state: r.get(9)?,
                        answer: r.get(10)?,
                    },
                ))
            })
            .map_err(|e| format!("could not query requests: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("could not read request row: {e}"))
    }

    /// Withdraws every request that waits, as when Lever stops.
    pub fn withdraw_all_waiting(&self) -> Result<(), String> {
        self.conn
            .execute("UPDATE requests SET state = 'withdrawn' WHERE state = 'waiting'", [])
            .map_err(|e| format!("could not withdraw waiting requests: {e}"))?;
        Ok(())
    }

    fn subagent_messages(&self, subagent_id: &str) -> Result<Vec<SubagentMessageRecord>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT role, text FROM subagent_messages WHERE subagent_id = ?1 ORDER BY seq ASC")
            .map_err(|e| format!("could not prepare subagent messages query: {e}"))?;
        let rows = stmt
            .query_map(params![subagent_id], |r| {
                Ok(SubagentMessageRecord {
                    role: r.get(0)?,
                    text: r.get(1)?,
                })
            })
            .map_err(|e| format!("could not query subagent messages: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("could not read subagent message row: {e}"))
    }

    fn subagent_acts(&self, subagent_id: &str) -> Result<Vec<ActRecord>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, kind, title, state, detail FROM subagent_acts WHERE subagent_id = ?1 ORDER BY seq ASC")
            .map_err(|e| format!("could not prepare subagent acts query: {e}"))?;
        let rows = stmt
            .query_map(params![subagent_id], |r| {
                Ok(ActRecord {
                    id: r.get(0)?,
                    kind: r.get(1)?,
                    title: r.get(2)?,
                    state: r.get(3)?,
                    detail: r.get(4)?,
                })
            })
            .map_err(|e| format!("could not query subagent acts: {e}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("could not read subagent act row: {e}"))
    }

    /// Inserts or updates a subagent owned by `patch.thread_id` and returns
    /// `(recorded_turn, SubagentRecord)` with Lever's own `sub-...` ID.
    /// A subagent's title, state, and error as the record has them, if it
    /// has the subagent.
    pub fn subagent_summary(
        &self,
        thread_id: &str,
        harness_session_id: &str,
    ) -> Result<Option<(String, String, Option<String>)>, String> {
        self.conn
            .query_row(
                "SELECT title, state, error FROM subagents WHERE thread_id = ?1 AND harness_session_id = ?2",
                params![thread_id, harness_session_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| format!("could not query subagent: {e}"))
    }

    pub fn upsert_subagent(&self, patch: &SubagentPatch<'_>, now: i64) -> Result<(u64, SubagentRecord), String> {
        // All of it, or none: the record never holds half a change.
        self.all_or_none("keep a subagent", || self.upsert_subagent_unguarded(patch, now))
    }

    fn upsert_subagent_unguarded(&self, patch: &SubagentPatch<'_>, now: i64) -> Result<(u64, SubagentRecord), String> {
        type Row = (String, u64, String, String, Option<String>, i64, Option<i64>);
        let existing: Option<Row> = self
            .conn
            .query_row(
                "SELECT id, turn_number, title, state, error, created_at, ended_at \
                 FROM subagents WHERE thread_id = ?1 AND harness_session_id = ?2",
                params![patch.thread_id, patch.harness_session_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get::<_, i64>(1)? as u64,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("could not query subagent: {e}"))?;

        let (turn_number, record) = if let Some((
            id,
            orig_turn,
            cur_title,
            cur_state,
            cur_error,
            created_at,
            cur_ended,
        )) = existing
        {
            let new_title = patch
                .title
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or(cur_title);
            let new_state = patch.state.map(str::to_string).unwrap_or(cur_state);
            let new_error = if new_state == "working" {
                None
            } else {
                patch.error.map(str::to_string).or(cur_error)
            };
            let new_ended = if new_state == "working" {
                None
            } else if patch.state.is_some() {
                Some(cur_ended.unwrap_or(now))
            } else {
                cur_ended
            };
            self.conn
                .execute(
                    "UPDATE subagents SET title = ?1, state = ?2, error = ?3, ended_at = ?4 WHERE id = ?5",
                    params![new_title, new_state, new_error, new_ended, id],
                )
                .map_err(|e| format!("could not update subagent: {e}"))?;
            (
                orig_turn,
                SubagentRecord {
                    id,
                    title: new_title,
                    state: new_state,
                    error: new_error,
                    started: created_at,
                    ended: new_ended,
                    messages: Vec::new(),
                    acts: Vec::new(),
                },
            )
        } else {
            let seq: i64 = self
                .conn
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) + 1 FROM subagents WHERE thread_id = ?1",
                    params![patch.thread_id],
                    |r| r.get(0),
                )
                .map_err(|e| format!("could not compute subagent sequence: {e}"))?;
            let id = format!("sub-{}", sys::random_hex(6));
            let title = patch
                .title
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("Subagent")
                .to_string();
            let state = patch.state.unwrap_or("working").to_string();
            let error = patch.error.map(str::to_string);
            let ended = (state != "working").then_some(now);
            self.conn
                    .execute(
                        "INSERT INTO subagents (id, thread_id, turn_number, harness_session_id, title, state, error, created_at, ended_at, seq) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            id,
                            patch.thread_id,
                            patch.turn_number as i64,
                            patch.harness_session_id,
                            title,
                            state,
                            error,
                            now,
                            ended,
                            seq
                        ],
                    )
                    .map_err(|e| format!("could not insert subagent: {e}"))?;
            (
                patch.turn_number,
                SubagentRecord {
                    id,
                    title,
                    state,
                    error,
                    started: now,
                    ended,
                    messages: Vec::new(),
                    acts: Vec::new(),
                },
            )
        };

        self.touch_thread(patch.thread_id, now)?;
        Ok((turn_number, record))
    }

    /// Marks a subagent as ended with `state` and optional `error`.
    pub fn finish_subagent(
        &self,
        thread_id: &str,
        subagent_id: &str,
        state: &str,
        error: Option<&str>,
        now: i64,
    ) -> Result<(), String> {
        // All of it, or none: the record never holds half a change.
        self.all_or_none("end a subagent", || {
            self.finish_subagent_unguarded(thread_id, subagent_id, state, error, now)
        })
    }

    fn finish_subagent_unguarded(
        &self,
        thread_id: &str,
        subagent_id: &str,
        state: &str,
        error: Option<&str>,
        now: i64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE subagents SET state = ?1, error = COALESCE(?2, error), ended_at = COALESCE(ended_at, ?3) \
                 WHERE id = ?4",
                params![state, error, now, subagent_id],
            )
            .map_err(|e| format!("could not finish subagent: {e}"))?;
        self.touch_thread(thread_id, now)?;
        Ok(())
    }

    /// Appends text to a subagent's messages. Consecutive chunks of the same
    /// role join into one message, except a `prompt` that repeats the newest
    /// prompt verbatim is skipped. Returns `true` if anything was stored.
    pub fn append_subagent_message(
        &self,
        thread_id: &str,
        subagent_id: &str,
        role: &str,
        text: &str,
        now: i64,
    ) -> Result<bool, String> {
        // All of it, or none: the record never holds half a change.
        self.all_or_none("keep a subagent's message", || {
            self.append_subagent_message_unguarded(thread_id, subagent_id, role, text, now)
        })
    }

    fn append_subagent_message_unguarded(
        &self,
        thread_id: &str,
        subagent_id: &str,
        role: &str,
        text: &str,
        now: i64,
    ) -> Result<bool, String> {
        if text.is_empty() {
            return Ok(false);
        }
        let last: Option<(i64, String, String)> = self
            .conn
            .query_row(
                "SELECT id, role, text FROM subagent_messages WHERE subagent_id = ?1 ORDER BY seq DESC LIMIT 1",
                params![subagent_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| format!("could not query last subagent message: {e}"))?;

        if let Some((row_id, last_role, last_text)) = &last
            && last_role == role
        {
            if role == "prompt" && last_text.trim() == text.trim() {
                return Ok(false);
            }
            self.conn
                .execute(
                    "UPDATE subagent_messages SET text = text || ?1 WHERE id = ?2",
                    params![text, row_id],
                )
                .map_err(|e| format!("could not append subagent message: {e}"))?;
            self.touch_thread(thread_id, now)?;
            return Ok(true);
        }
        let seq: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM subagent_messages WHERE subagent_id = ?1",
                params![subagent_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("could not compute subagent message sequence: {e}"))?;
        self.conn
            .execute(
                "INSERT INTO subagent_messages (subagent_id, role, text, seq) VALUES (?1, ?2, ?3, ?4)",
                params![subagent_id, role, text, seq],
            )
            .map_err(|e| format!("could not insert subagent message: {e}"))?;
        self.touch_thread(thread_id, now)?;
        Ok(true)
    }

    /// Inserts or updates an act belonging to a subagent, returning Lever's
    /// own act ID (`act-...`).
    pub fn upsert_subagent_act(&self, patch: &SubagentActPatch<'_>, now: i64) -> Result<ActRecord, String> {
        // All of it, or none: the record never holds half a change.
        self.all_or_none("keep a subagent's act", || {
            self.upsert_subagent_act_unguarded(patch, now)
        })
    }

    fn upsert_subagent_act_unguarded(&self, patch: &SubagentActPatch<'_>, now: i64) -> Result<ActRecord, String> {
        let existing: Option<(String, String, String, String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT id, kind, title, state, detail FROM subagent_acts \
                 WHERE subagent_id = ?1 AND harness_call_id = ?2",
                params![patch.subagent_id, patch.harness_call_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .map_err(|e| format!("could not query subagent act: {e}"))?;

        let record = if let Some((id, cur_kind, cur_title, cur_state, cur_detail)) = existing {
            let new_kind = patch.kind.map(str::to_string).unwrap_or(cur_kind);
            let new_title = patch.title.map(str::to_string).unwrap_or(cur_title);
            let new_state = patch.state.map(str::to_string).unwrap_or(cur_state);
            let new_detail = patch.detail.map(str::to_string).or(cur_detail);
            self.conn
                .execute(
                    "UPDATE subagent_acts SET kind = ?1, title = ?2, state = ?3, detail = ?4 WHERE id = ?5",
                    params![new_kind, new_title, new_state, new_detail, id],
                )
                .map_err(|e| format!("could not update subagent act: {e}"))?;
            ActRecord {
                id,
                kind: new_kind,
                title: new_title,
                state: new_state,
                detail: new_detail,
            }
        } else {
            let seq: i64 = self
                .conn
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) + 1 FROM subagent_acts WHERE subagent_id = ?1",
                    params![patch.subagent_id],
                    |r| r.get(0),
                )
                .map_err(|e| format!("could not compute subagent act sequence: {e}"))?;
            let id = format!("act-{}", sys::random_hex(6));
            let new_kind = patch.kind.unwrap_or("other").to_string();
            let new_title = patch.title.unwrap_or("Act").to_string();
            let new_state = patch.state.unwrap_or("pending").to_string();
            let new_detail = patch.detail.map(str::to_string);
            self.conn
                .execute(
                    "INSERT INTO subagent_acts (id, subagent_id, thread_id, harness_call_id, kind, title, state, detail, seq) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        id,
                        patch.subagent_id,
                        patch.thread_id,
                        patch.harness_call_id,
                        new_kind,
                        new_title,
                        new_state,
                        new_detail,
                        seq
                    ],
                )
                .map_err(|e| format!("could not insert subagent act: {e}"))?;
            ActRecord {
                id,
                kind: new_kind,
                title: new_title,
                state: new_state,
                detail: new_detail,
            }
        };

        self.touch_thread(patch.thread_id, now)?;
        Ok(record)
    }
}

fn ensure_column(conn: &Connection, table: &str, column: &str, sql_type: &str) -> Result<(), String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| format!("could not inspect {table} table: {e}"))?;
    let cols = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| format!("could not read {table} schema: {e}"))?;
    for col in cols {
        if col.map_err(|e| format!("could not read column name: {e}"))? == column {
            return Ok(());
        }
    }
    conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {sql_type}"), [])
        .map_err(|e| format!("could not migrate {table}.{column} column: {e}"))?;
    Ok(())
}

fn tighten_permissions(db_path: &Path) -> Result<(), String> {
    let mut paths = vec![db_path.to_path_buf()];
    for suffix in ["-wal", "-shm"] {
        let mut s = db_path.as_os_str().to_os_string();
        s.push(suffix);
        paths.push(PathBuf::from(s));
    }
    for p in paths {
        if p.exists() {
            sys::chmod(&p, 0o600).map_err(|e| format!("could not set mode on {}: {e}", p.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_and_migrates_v2_database_without_threads_model_column() {
        let dir = std::env::temp_dir().join(format!(
            "crowbar-lever-store-migrate-{}-{}",
            std::process::id(),
            crate::sys::random_hex(4)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("lever.db");

        // Create a schema-v2 database (without `threads.model`).
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta (key, value) VALUES ('schema_version', '2');
                 CREATE TABLE threads (
                     id TEXT PRIMARY KEY,
                     title TEXT NOT NULL,
                     cwd TEXT NOT NULL,
                     harness_name TEXT,
                     harness_session_id TEXT,
                     created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 INSERT INTO threads (id, title, cwd, harness_name, harness_session_id, created_at, updated_at)
                 VALUES ('t-old', 'Old thread', '/tmp', 'OpenCode', 'sess-1', 100, 100);",
            )
            .unwrap();
        }

        let store = Store::open(&db, 200).unwrap();
        let meta = store.thread_meta("t-old").unwrap().expect("migrated thread");
        assert_eq!(meta.model, None);
        store.set_thread_model("t-old", "stub/stub-1", 201).unwrap();
        let updated = store.thread_meta("t-old").unwrap().expect("updated thread");
        assert_eq!(updated.model.as_deref(), Some("stub/stub-1"));
        assert!(!store.free_hosted_models().unwrap());
        assert!(!store.provider_models_seen().unwrap());
        store.set_provider_models_seen(true).unwrap();
        assert!(store.provider_models_seen().unwrap());
        store.set_provider_models_seen(false).unwrap();
        assert!(!store.provider_models_seen().unwrap());
        store.close().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opens_and_migrates_v3_database_without_turns_context_column() {
        let dir = std::env::temp_dir().join(format!(
            "crowbar-lever-store-migrate-v3-{}-{}",
            std::process::id(),
            crate::sys::random_hex(4)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("lever.db");

        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta (key, value) VALUES ('schema_version', '3');
                 CREATE TABLE threads (
                     id TEXT PRIMARY KEY,
                     title TEXT NOT NULL,
                     cwd TEXT NOT NULL,
                     harness_name TEXT,
                     harness_session_id TEXT,
                     model TEXT,
                     created_at INTEGER NOT NULL,
                     updated_at INTEGER NOT NULL
                 );
                 CREATE TABLE turns (
                     id INTEGER PRIMARY KEY AUTOINCREMENT,
                     thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                     turn_number INTEGER NOT NULL,
                     message TEXT NOT NULL,
                     answer TEXT NOT NULL DEFAULT '',
                     state TEXT NOT NULL,
                     error TEXT,
                     created_at INTEGER NOT NULL,
                     ended_at INTEGER,
                     UNIQUE(thread_id, turn_number)
                 );
                 INSERT INTO threads (id, title, cwd, harness_name, harness_session_id, model, created_at, updated_at)
                 VALUES ('t-v3', 'Old v3 thread', '/tmp', 'OpenCode', 'sess-1', 'stub/stub-1', 100, 100);
                 INSERT INTO turns (thread_id, turn_number, message, answer, state, created_at)
                 VALUES ('t-v3', 1, 'earlier', 'answer', 'done', 100);",
            )
            .unwrap();
        }

        let store = Store::open(&db, 200).unwrap();
        let rec = store.get_thread("t-v3").unwrap().expect("migrated v3 thread");
        assert_eq!(rec.turns.len(), 1);
        assert_eq!(rec.turns[0].context, None);

        let ctx = r#"{"workspace":1,"active":null,"locked":false,"windows":[],"displays":[]}"#;
        let turn2 = store.begin_turn("t-v3", None, "second", Some(ctx), None, 201).unwrap();
        assert_eq!(turn2, 2);
        let rec = store.get_thread("t-v3").unwrap().expect("updated v3 thread");
        assert_eq!(
            rec.turns[1].context.as_ref().and_then(|v| v.get("workspace")),
            Some(&serde_json::json!(1))
        );
        store.close().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opens_a_v4_database_with_no_modes_or_requests_and_keeps_both_after() {
        let dir = std::env::temp_dir().join(format!(
            "crowbar-lever-store-migrate-v4-{}-{}",
            std::process::id(),
            crate::sys::random_hex(4)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("lever.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta (key, value) VALUES ('schema_version', '4');
                 CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT NOT NULL, cwd TEXT NOT NULL,
                     harness_name TEXT, harness_session_id TEXT, model TEXT,
                     created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
                 CREATE TABLE turns (id INTEGER PRIMARY KEY AUTOINCREMENT,
                     thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                     turn_number INTEGER NOT NULL, message TEXT NOT NULL, context TEXT,
                     answer TEXT NOT NULL DEFAULT '', state TEXT NOT NULL, error TEXT,
                     created_at INTEGER NOT NULL, ended_at INTEGER, UNIQUE(thread_id, turn_number));
                 INSERT INTO threads (id, title, cwd, created_at, updated_at) VALUES ('t-v4', 'Old', '/tmp', 1, 1);
                 INSERT INTO turns (thread_id, turn_number, message, state, created_at) VALUES ('t-v4', 1, 'hi', 'done', 1);",
            )
            .unwrap();
        }
        let store = Store::open(&db, 2).unwrap();
        let rec = store.get_thread("t-v4").unwrap().unwrap();
        assert_eq!(rec.mode, STARTING_MODE);
        assert_eq!(rec.turns[0].mode, None);
        assert!(rec.turns[0].requests.is_empty());
        assert_eq!(store.default_mode().unwrap(), STARTING_MODE);

        store.set_thread_mode("t-v4", "plan", 3).unwrap();
        let turn = store.begin_turn("t-v4", None, "again", None, None, 3).unwrap();
        let answers = vec![("once".to_string(), "Allow once".to_string())];
        let request = store
            .insert_request(
                &NewRequest {
                    thread_id: "t-v4",
                    turn_number: turn,
                    act: None,
                    subagent: None,
                    kind: "command",
                    title: "ls",
                    detail: Some("ls"),
                    path: None,
                    answers: &answers,
                },
                4,
            )
            .unwrap();
        assert!(store.list_threads().unwrap()[0].waiting);
        let (thread, turn_number, ended) = store
            .end_request(&request.id, "answered", Some("once"), 5)
            .unwrap()
            .unwrap();
        assert_eq!(
            (thread.as_str(), turn_number, ended.answer.as_deref()),
            ("t-v4", 2, Some("once"))
        );
        assert_eq!(
            store.end_request(&request.id, "withdrawn", None, 6).unwrap(),
            None,
            "it ends once"
        );
        let rec = store.get_thread("t-v4").unwrap().unwrap();
        assert_eq!(rec.turns[1].mode.as_deref(), Some("plan"));
        assert_eq!(rec.turns[1].requests[0].state, "answered");

        // A request the database takes only part of is not kept at all.
        store
            .conn
            .execute_batch("CREATE TRIGGER refused BEFORE UPDATE ON threads BEGIN SELECT RAISE(ABORT, 'refused'); END")
            .unwrap();
        let partial = NewRequest {
            thread_id: "t-v4",
            turn_number: turn,
            act: None,
            subagent: None,
            kind: "command",
            title: "rm",
            detail: Some("rm"),
            path: None,
            answers: &answers,
        };
        assert!(store.insert_request(&partial, 7).is_err());
        store.conn.execute_batch("DROP TRIGGER refused").unwrap();
        let rec = store.get_thread("t-v4").unwrap().unwrap();
        assert_eq!(
            rec.turns[1].requests.len(),
            1,
            "no request is left that nobody can answer"
        );
        store.close().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[test]
    fn a_v6_database_takes_subagents_and_harness_turns_and_keeps_no_half_subagent() {
        let dir = std::env::temp_dir().join(format!(
            "crowbar-lever-store-v6-{}-{}",
            std::process::id(),
            crate::sys::random_hex(4)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("lever.db");

        // The schema a Lever before subagents left: no subagent tables, no
        // `requests.subagent_id`, no `turns.origin`.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta (key, value) VALUES ('schema_version', '6');
                 CREATE TABLE threads (id TEXT PRIMARY KEY, title TEXT NOT NULL, cwd TEXT NOT NULL,
                     harness_name TEXT, harness_session_id TEXT, model TEXT, mode TEXT, effort TEXT,
                     archived INTEGER NOT NULL DEFAULT 0,
                     created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
                 CREATE TABLE turns (id INTEGER PRIMARY KEY AUTOINCREMENT,
                     thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                     turn_number INTEGER NOT NULL, message TEXT NOT NULL, context TEXT,
                     answer TEXT NOT NULL DEFAULT '', state TEXT NOT NULL, error TEXT, mode TEXT,
                     created_at INTEGER NOT NULL, ended_at INTEGER, UNIQUE(thread_id, turn_number));
                 CREATE TABLE requests (id TEXT PRIMARY KEY,
                     thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
                     turn_number INTEGER NOT NULL, act_id TEXT, kind TEXT NOT NULL, title TEXT NOT NULL,
                     detail TEXT, path TEXT, answers TEXT NOT NULL, state TEXT NOT NULL, answer TEXT,
                     seq INTEGER NOT NULL);
                 INSERT INTO threads (id, title, cwd, mode, created_at, updated_at)
                 VALUES ('t-v6', 'Old thread', '/tmp', 'ask-first', 100, 100);
                 INSERT INTO turns (thread_id, turn_number, message, state, created_at, ended_at)
                 VALUES ('t-v6', 1, 'hello', 'done', 100, 101);
                 INSERT INTO requests (id, thread_id, turn_number, kind, title, answers, state, seq)
                 VALUES ('r-old', 't-v6', 1, 'command', 'ls', '[]', 'waiting', 1);",
            )
            .unwrap();
        }

        let store = Store::open(&db, 200).unwrap();
        let rec = store.get_thread("t-v6").unwrap().unwrap();
        assert_eq!(rec.turns[0].origin, None, "a turn from before is the person's");
        assert_eq!(rec.turns[0].requests[0].state, "withdrawn");
        assert_eq!(rec.turns[0].requests[0].subagent, None);

        let patch = SubagentPatch {
            thread_id: "t-v6",
            turn_number: 1,
            harness_session_id: "ses-child",
            title: Some("Look around"),
            state: Some("working"),
            error: None,
        };
        let (turn, sub) = store.upsert_subagent(&patch, 102).unwrap();
        assert_eq!((turn, sub.state.as_str()), (1, "working"));

        // A turn the harness began is its own, and only the newest is reopened.
        let woke = store.begin_harness_turn("t-v6", 103).unwrap();
        assert_eq!(woke, 2);
        assert_eq!(
            store.latest_turn("t-v6").unwrap(),
            Some((2, "working".to_string(), true))
        );
        store.finish_turn("t-v6", 2, "done", None, 104).unwrap();
        assert!(store.reopen_harness_turn("t-v6", 2, 105).unwrap());
        assert_eq!(
            store.latest_turn("t-v6").unwrap(),
            Some((2, "working".to_string(), true))
        );
        store.finish_turn("t-v6", 2, "done", None, 106).unwrap();
        store.begin_turn("t-v6", None, "next", None, None, 107).unwrap();
        assert!(
            !store.reopen_harness_turn("t-v6", 2, 108).unwrap(),
            "a turn with a newer one after it stays ended"
        );
        assert!(
            !store.reopen_harness_turn("t-v6", 1, 108).unwrap(),
            "the person's turn is not reopened"
        );
        let rec = store.get_thread("t-v6").unwrap().unwrap();
        assert_eq!(rec.turns[1].origin.as_deref(), Some("harness"));
        assert_eq!(rec.turns[1].message, "");

        // A subagent the database takes only part of is not kept at all,
        // nor is half a reopened turn.
        store
            .conn
            .execute_batch("CREATE TRIGGER refused BEFORE UPDATE ON threads BEGIN SELECT RAISE(ABORT, 'refused'); END")
            .unwrap();
        let woke_again = store.begin_harness_turn("t-v6", 110);
        assert!(
            woke_again.is_err(),
            "the thread is not touched, so the turn is not begun"
        );
        store.conn.execute_batch("DROP TRIGGER refused").unwrap();
        let woke_again = store.begin_harness_turn("t-v6", 111).unwrap();
        store.finish_turn("t-v6", woke_again, "done", None, 112).unwrap();
        store
            .conn
            .execute_batch("CREATE TRIGGER refused BEFORE UPDATE ON threads BEGIN SELECT RAISE(ABORT, 'refused'); END")
            .unwrap();
        assert!(store.reopen_harness_turn("t-v6", woke_again, 113).is_err());
        assert_eq!(
            store.latest_turn("t-v6").unwrap(),
            Some((woke_again, "done".to_string(), true)),
            "a reopen the database takes only part of leaves the turn ended"
        );
        let partial = SubagentPatch {
            harness_session_id: "ses-half",
            ..patch
        };
        assert!(store.upsert_subagent(&partial, 109).is_err());
        store.conn.execute_batch("DROP TRIGGER refused").unwrap();
        let rec = store.get_thread("t-v6").unwrap().unwrap();
        let subagents: usize = rec.turns.iter().map(|t| t.subagents.len()).sum();
        assert_eq!(subagents, 1, "no subagent is left working that Lever does not hold");
        store.close().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovered_threads_import_history_atomically_and_track_ignored_sessions() {
        use crate::discovery::{ImportedAct, ImportedSubagent, ImportedTurn};

        let dir = std::env::temp_dir().join(format!(
            "crowbar-lever-store-discovered-{}-{}",
            std::process::id(),
            crate::sys::random_hex(4)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("lever.db");

        let store = Store::open(&db, 100).unwrap();
        store
            .create_discovered_thread(
                "t-ext1",
                "Thread",
                "/tmp/work",
                Some("OpenCode"),
                "ses-ext-1",
                STARTING_MODE,
                90,
                95,
            )
            .unwrap();
        store.remember_ignored_session("ses-sub-1", "subagent", 96).unwrap();

        let (owned, ignored) = store.known_harness_sessions().unwrap();
        assert!(owned.contains("ses-ext-1"));
        assert!(ignored.contains("ses-sub-1"));

        let meta = store.thread_meta("t-ext1").unwrap().unwrap();
        assert!(meta.needs_history);
        assert_eq!(meta.harness_session_id.as_deref(), Some("ses-ext-1"));

        assert!(
            store
                .update_discovered_thread("t-ext1", Some("Terminal chat"), 98)
                .unwrap()
        );
        assert!(
            !store
                .update_discovered_thread("t-ext1", Some("Terminal chat"), 97)
                .unwrap()
        );

        let history = SessionHistory {
            title: None,
            turns: vec![ImportedTurn {
                message: "List the files".to_string(),
                answer: "Found Cargo.toml".to_string(),
                acts: vec![ImportedAct {
                    harness_call_id: "call-1".to_string(),
                    kind: "command".to_string(),
                    title: "ls".to_string(),
                    state: "done".to_string(),
                    detail: Some("Cargo.toml".to_string()),
                }],
                notes: vec![],
                subagents: vec![ImportedSubagent {
                    harness_session_id: "ses-sub-2".to_string(),
                    title: "Helper".to_string(),
                    state: "done".to_string(),
                    error: None,
                    prompt: Some("Inspect".to_string()),
                }],
                origin: None,
            }],
            model: Some("stub/stub-1".to_string()),
            effort: Some("high".to_string()),
            mode: None,
        };
        store.import_thread_history("t-ext1", &history, None, 105).unwrap();

        let meta = store.thread_meta("t-ext1").unwrap().unwrap();
        assert!(!meta.needs_history);
        assert_eq!(meta.model.as_deref(), Some("stub/stub-1"));
        assert_eq!(meta.effort.as_deref(), Some("high"));
        // A Model the person chose before opening it outlives the import.
        store
            .create_discovered_thread(
                "t-ext2",
                "Thread",
                "/tmp",
                Some("OpenCode"),
                "ses-ext-2",
                "ask-first",
                90,
                96,
            )
            .unwrap();
        store.set_thread_model("t-ext2", "stub/chosen", 100).unwrap();
        store.import_thread_history("t-ext2", &history, None, 105).unwrap();
        let meta = store.thread_meta("t-ext2").unwrap().unwrap();
        assert_eq!(meta.model.as_deref(), Some("stub/chosen"));
        assert_eq!(meta.effort.as_deref(), Some("high"));

        let rec = store.get_thread("t-ext1").unwrap().unwrap();
        assert_eq!(rec.title, "Terminal chat");
        assert_eq!(rec.turns.len(), 1);
        assert_eq!(rec.turns[0].message, "List the files");
        assert_eq!(rec.turns[0].answer, "Found Cargo.toml");
        assert_eq!(rec.turns[0].acts.len(), 1);
        assert_eq!(rec.turns[0].acts[0].title, "ls");
        assert_eq!(rec.turns[0].subagents.len(), 1);
        assert_eq!(rec.turns[0].subagents[0].title, "Helper");

        let (_, ignored_after) = store.known_harness_sessions().unwrap();
        assert!(ignored_after.contains("ses-sub-2"));

        // A session a new one replaces is never taken for one started by hand.
        store.set_harness_session_id("t-ext1", "ses-ext-1b").unwrap();
        let (owned, ignored) = store.known_harness_sessions().unwrap();
        assert!(owned.contains("ses-ext-1b"));
        assert!(ignored.contains("ses-ext-1"));

        // Deleting remembers the thread's session and its subagents', whether
        // or not the harness deleted its own copy.
        store
            .conn
            .execute(
                "DELETE FROM ignored_sessions WHERE harness_session_id = 'ses-sub-2'",
                [],
            )
            .unwrap();
        store.delete_thread("t-ext1", 110).unwrap();
        let (owned, ignored) = store.known_harness_sessions().unwrap();
        assert!(!owned.contains("ses-ext-1b"));
        assert!(ignored.contains("ses-ext-1b"));
        assert!(ignored.contains("ses-sub-2"));
        assert!(store.thread_meta("t-ext1").unwrap().is_none());

        store.close().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
