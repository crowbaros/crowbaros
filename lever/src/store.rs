//! Lever's SQLite state store (`lever.sqlite`), kept in the person's home and
//! readable by the person alone.
//!
//! Keeps every thread, its turns, its acts, and any updates Lever did not
//! recognize (kept verbatim as they arrived, along with a plain rendering).

use crate::sys;
use rusqlite::{Connection, OptionalExtension, params};
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ThreadSummary {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub started: i64,
    pub updated: i64,
    pub turns: u64,
    pub state: String,
    pub working: bool,
}

#[derive(Debug, Clone)]
pub struct ThreadMeta {
    pub cwd: String,
    pub harness_session_id: Option<String>,
    pub latest_turn: Option<u64>,
    pub working_turn: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct ThreadRecord {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub started: i64,
    pub updated: i64,
    pub working: bool,
    pub turns: Vec<TurnRecord>,
}

#[derive(Debug, Clone)]
pub struct TurnRecord {
    pub turn: u64,
    pub message: String,
    pub answer: String,
    pub state: String,
    pub error: Option<String>,
    pub acts: Vec<ActRecord>,
    pub notes: Vec<NoteRecord>,
}

#[derive(Debug, Clone)]
pub struct ActRecord {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub state: String,
    pub detail: Option<String>,
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
             INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '2');\n\
             CREATE TABLE IF NOT EXISTS threads (\n\
                 id TEXT PRIMARY KEY,\n\
                 title TEXT NOT NULL,\n\
                 cwd TEXT NOT NULL,\n\
                 harness_name TEXT,\n\
                 harness_session_id TEXT,\n\
                 created_at INTEGER NOT NULL,\n\
                 updated_at INTEGER NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS turns (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,\n\
                 turn_number INTEGER NOT NULL,\n\
                 message TEXT NOT NULL,\n\
                 answer TEXT NOT NULL DEFAULT '',\n\
                 state TEXT NOT NULL,\n\
                 error TEXT,\n\
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
                 UNIQUE(thread_id, harness_call_id)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS notes (\n\
                 id TEXT PRIMARY KEY,\n\
                 thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,\n\
                 turn_number INTEGER NOT NULL,\n\
                 raw_json TEXT NOT NULL,\n\
                 plain_text TEXT NOT NULL,\n\
                 after_turn INTEGER NOT NULL DEFAULT 0,\n\
                 seq INTEGER NOT NULL\n\
             );",
        )
        .map_err(|e| format!("could not initialize database {}: {e}", path.display()))?;

        // Any turn left working by a Lever that ended abruptly is interrupted.
        conn.execute(
            "UPDATE turns SET state = 'interrupted', ended_at = ?1 WHERE state = 'working'",
            params![now],
        )
        .map_err(|e| format!("could not mark interrupted turns: {e}"))?;

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

    pub fn create_thread(
        &self,
        id: &str,
        title: &str,
        cwd: &str,
        harness_name: Option<&str>,
        now: i64,
    ) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO threads (id, title, cwd, harness_name, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, title, cwd, harness_name, now],
            )
            .map_err(|e| format!("could not create thread: {e}"))?;
        Ok(())
    }

    pub fn thread_meta(&self, thread_id: &str) -> Result<Option<ThreadMeta>, String> {
        let row: Option<(String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT cwd, harness_session_id FROM threads WHERE id = ?1",
                params![thread_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| format!("could not query thread: {e}"))?;
        let Some((cwd, harness_session_id)) = row else {
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
            latest_turn,
            working_turn,
        }))
    }

    pub fn set_harness_session_id(&self, thread_id: &str, session_id: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE threads SET harness_session_id = ?1 WHERE id = ?2",
                params![session_id, thread_id],
            )
            .map_err(|e| format!("could not store harness session id: {e}"))?;
        Ok(())
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

    pub fn create_turn(&self, thread_id: &str, message: &str, now: i64) -> Result<u64, String> {
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
                "INSERT INTO turns (thread_id, turn_number, message, answer, state, created_at) \
                 VALUES (?1, ?2, ?3, '', 'working', ?4)",
                params![thread_id, next, message, now],
            )
            .map_err(|e| format!("could not insert turn: {e}"))?;
        self.conn
            .execute(
                "UPDATE threads SET updated_at = ?1 WHERE id = ?2",
                params![now, thread_id],
            )
            .map_err(|e| format!("could not touch thread timestamp: {e}"))?;
        Ok(next as u64)
    }

    pub fn append_answer(&self, thread_id: &str, turn_number: u64, chunk: &str, now: i64) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE turns SET answer = answer || ?1 WHERE thread_id = ?2 AND turn_number = ?3",
                params![chunk, thread_id, turn_number as i64],
            )
            .map_err(|e| format!("could not append answer: {e}"))?;
        self.conn
            .execute(
                "UPDATE threads SET updated_at = ?1 WHERE id = ?2",
                params![now, thread_id],
            )
            .map_err(|e| format!("could not touch thread timestamp: {e}"))?;
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
        self.conn
            .execute(
                "UPDATE threads SET updated_at = ?1 WHERE id = ?2",
                params![now, thread_id],
            )
            .map_err(|e| format!("could not touch thread timestamp: {e}"))?;
        Ok(())
    }

    /// Marks every currently-working turn as `interrupted` and returns the
    /// affected `(thread_id, turn_number)` pairs.
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
        Ok(interrupted)
    }

    /// Inserts or updates an act identified internally by `(thread_id, harness_call_id)`
    /// and returns the stored `ActRecord` with Lever's own act ID.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_act(
        &self,
        thread_id: &str,
        turn_number: u64,
        harness_call_id: &str,
        kind: Option<&str>,
        title: Option<&str>,
        state: Option<&str>,
        detail: Option<&str>,
        now: i64,
    ) -> Result<(u64, ActRecord), String> {
        let existing: Option<(String, u64, String, String, String, Option<String>)> = self
            .conn
            .query_row(
                "SELECT id, turn_number, kind, title, state, detail FROM acts \
                 WHERE thread_id = ?1 AND harness_call_id = ?2",
                params![thread_id, harness_call_id],
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
            .map_err(|e| format!("could not query act: {e}"))?;

        let (recorded_turn, record) =
            if let Some((id, orig_turn, cur_kind, cur_title, cur_state, cur_detail)) = existing {
                let new_kind = kind.map(str::to_string).unwrap_or(cur_kind);
                let new_title = title.map(str::to_string).unwrap_or(cur_title);
                let new_state = state.map(str::to_string).unwrap_or(cur_state);
                let new_detail = detail.map(str::to_string).or(cur_detail);
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
                        params![thread_id],
                        |r| r.get(0),
                    )
                    .map_err(|e| format!("could not compute act sequence: {e}"))?;
                let id = format!("act-{}", sys::random_hex(6));
                let new_kind = kind.unwrap_or("other").to_string();
                let new_title = title.unwrap_or("Act").to_string();
                let new_state = state.unwrap_or("pending").to_string();
                let new_detail = detail.map(str::to_string);
                self.conn
                .execute(
                    "INSERT INTO acts (id, thread_id, turn_number, harness_call_id, kind, title, state, detail, seq) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        id,
                        thread_id,
                        turn_number as i64,
                        harness_call_id,
                        new_kind,
                        new_title,
                        new_state,
                        new_detail,
                        seq
                    ],
                )
                .map_err(|e| format!("could not insert act: {e}"))?;
                (
                    turn_number,
                    ActRecord {
                        id,
                        kind: new_kind,
                        title: new_title,
                        state: new_state,
                        detail: new_detail,
                    },
                )
            };

        self.conn
            .execute(
                "UPDATE threads SET updated_at = ?1 WHERE id = ?2",
                params![now, thread_id],
            )
            .map_err(|e| format!("could not touch thread timestamp: {e}"))?;
        Ok((recorded_turn, record))
    }

    /// Stores an unrecognized harness update verbatim in `notes.raw_json` along
    /// with its plain text rendering, returning Lever's own note record.
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
        self.conn
            .execute(
                "UPDATE threads SET updated_at = ?1 WHERE id = ?2",
                params![now, thread_id],
            )
            .map_err(|e| format!("could not touch thread timestamp: {e}"))?;
        Ok(NoteRecord {
            id,
            text: plain_text.to_string(),
        })
    }

    pub fn list_threads(&self) -> Result<Vec<ThreadSummary>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT t.id, t.title, t.cwd, t.created_at, t.updated_at, \
                        (SELECT COUNT(*) FROM turns WHERE thread_id = t.id), \
                        (SELECT state FROM turns WHERE thread_id = t.id ORDER BY turn_number DESC LIMIT 1) \
                 FROM threads t \
                 ORDER BY t.updated_at DESC, t.rowid DESC",
            )
            .map_err(|e| format!("could not prepare thread list query: {e}"))?;
        let rows = stmt
            .query_map([], |r| {
                let state: String = r.get::<_, Option<String>>(6)?.unwrap_or_else(|| "idle".to_string());
                let working = state == "working";
                Ok(ThreadSummary {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    cwd: r.get(2)?,
                    started: r.get(3)?,
                    updated: r.get(4)?,
                    turns: r.get::<_, i64>(5)? as u64,
                    state,
                    working,
                })
            })
            .map_err(|e| format!("could not query threads: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("could not read thread row: {e}"))?);
        }
        Ok(out)
    }

    pub fn get_thread(&self, thread_id: &str) -> Result<Option<ThreadRecord>, String> {
        let row: Option<(String, String, String, i64, i64)> = self
            .conn
            .query_row(
                "SELECT id, title, cwd, created_at, updated_at FROM threads WHERE id = ?1",
                params![thread_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()
            .map_err(|e| format!("could not query thread: {e}"))?;
        let Some((id, title, cwd, started, updated)) = row else {
            return Ok(None);
        };

        let mut turn_stmt = self
            .conn
            .prepare(
                "SELECT turn_number, message, answer, state, error \
                 FROM turns WHERE thread_id = ?1 ORDER BY turn_number ASC",
            )
            .map_err(|e| format!("could not prepare turns query: {e}"))?;
        let turn_rows = turn_stmt
            .query_map(params![thread_id], |r| {
                Ok(TurnRecord {
                    turn: r.get::<_, i64>(0)? as u64,
                    message: r.get(1)?,
                    answer: r.get(2)?,
                    state: r.get(3)?,
                    error: r.get(4)?,
                    acts: Vec::new(),
                    notes: Vec::new(),
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

        let working = turns.last().is_some_and(|t| t.state == "working");
        Ok(Some(ThreadRecord {
            id,
            title,
            cwd,
            started,
            updated,
            working,
            turns,
        }))
    }
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
