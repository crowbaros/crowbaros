//! Thread CRUD, metadata, discovery/history import, project moves, and lifecycle.

use super::Store;
use super::records::*;
use crate::harness::SessionHistory;
use crate::sys;
use rusqlite::{OptionalExtension, params};
use std::collections::{HashMap, HashSet};

impl Store {
    pub(super) fn create_thread(&self, id: &str, thread: &NewThread<'_>, now: i64) -> Result<(), String> {
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
            bool,
        );
        let row: Option<Row> = self
            .conn
            .query_row(
                "SELECT cwd, harness_session_id, model, mode, effort, needs_history, needs_handoff FROM threads WHERE id = ?1",
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
                    ))
                },
            )
            .optional()
            .map_err(|e| format!("could not query thread: {e}"))?;
        let Some((cwd, harness_session_id, model, mode, effort, needs_history, needs_handoff)) = row else {
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
            needs_handoff,
        }))
    }

    /// Moves a thread to a project folder, clearing its previous harness
    /// session ID (and remembering it as ignored, associated with `thread_id`
    /// so discovery does not re-import it as a second thread and deleting the
    /// thread still asks the harness to delete retired sessions) and marking
    /// whether the next fresh session must be handed the thread's prior turns.
    pub fn move_thread_to_project(
        &self,
        thread_id: &str,
        new_cwd: &str,
        needs_handoff: bool,
        old_session_id: Option<&str>,
        now: i64,
    ) -> Result<(), String> {
        self.all_or_none("move thread to project", || {
            self.conn
                .execute(
                    "UPDATE threads SET cwd = ?1, harness_session_id = NULL, needs_handoff = ?2, updated_at = ?3 \
                     WHERE id = ?4",
                    params![new_cwd, needs_handoff, now, thread_id],
                )
                .map_err(|e| format!("could not update thread project folder: {e}"))?;
            if let Some(sid) = old_session_id.filter(|s| !s.is_empty()) {
                self.conn
                    .execute(
                        "INSERT INTO ignored_sessions (harness_session_id, reason, created_at, thread_id) \
                         VALUES (?1, 'moved', ?2, ?3) \
                         ON CONFLICT(harness_session_id) DO UPDATE SET reason = excluded.reason, thread_id = excluded.thread_id",
                        params![sid, now, thread_id],
                    )
                    .map_err(|e| format!("could not record moved session: {e}"))?;
                self.forget_quiet_session(sid)?;
            }
            Ok(())
        })
    }

    /// Returns harness session IDs previously retired by `thread_id` (e.g.
    /// before moving to a project folder) so deleting the thread also asks the
    /// harness to delete those transcripts. Excludes any session ID currently
    /// owned by a live thread in `threads`.
    pub fn retired_sessions_for_thread(&self, thread_id: &str) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT harness_session_id FROM ignored_sessions \
                 WHERE thread_id = ?1 AND harness_session_id <> '' \
                 AND harness_session_id NOT IN ( \
                     SELECT harness_session_id FROM threads \
                     WHERE harness_session_id IS NOT NULL AND harness_session_id <> '' \
                 ) \
                 ORDER BY created_at ASC",
            )
            .map_err(|e| format!("could not prepare retired sessions query: {e}"))?;
        let rows = stmt
            .query_map(params![thread_id], |r| r.get::<_, String>(0))
            .map_err(|e| format!("could not query retired sessions: {e}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("could not read retired sessions: {e}"))
    }

    /// Persists that `session_id` has been deleted on the harness for
    /// `thread_id` so a partial failure on a later session never retries an
    /// already-deleted session.
    pub fn mark_session_deleted(&self, thread_id: &str, session_id: &str, now: i64) -> Result<(), String> {
        if session_id.is_empty() {
            return Ok(());
        }
        self.all_or_none("mark deleted session", || {
            self.conn
                .execute(
                    "UPDATE threads SET harness_session_id = NULL WHERE id = ?1 AND harness_session_id = ?2",
                    params![thread_id, session_id],
                )
                .map_err(|e| format!("could not clear deleted session id: {e}"))?;
            self.conn
                .execute(
                    "INSERT INTO ignored_sessions (harness_session_id, reason, created_at, thread_id) \
                     VALUES (?1, 'deleted', ?2, NULL) \
                     ON CONFLICT(harness_session_id) DO UPDATE SET reason = 'deleted', thread_id = NULL",
                    params![session_id, now],
                )
                .map_err(|e| format!("could not record deleted session: {e}"))?;
            self.forget_quiet_session(session_id)?;
            Ok(())
        })
    }

    /// Clears the pending history-handoff flag once a fresh session has
    /// received the thread's prior turns.
    pub fn clear_needs_handoff(&self, thread_id: &str) -> Result<(), String> {
        self.conn
            .execute("UPDATE threads SET needs_handoff = 0 WHERE id = ?1", params![thread_id])
            .map_err(|e| format!("could not clear thread handoff flag: {e}"))?;
        Ok(())
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

    /// Sessions a harness's own export found empty or archived, by the
    /// `updated` stamp they had then: not looked at again until it changes.
    pub fn quiet_sessions(&self) -> Result<HashMap<String, String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT harness_session_id, stamp FROM quiet_sessions")
            .map_err(|e| format!("could not read quiet sessions: {e}"))?;
        let rows = stmt
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
            .map_err(|e| format!("could not read quiet sessions: {e}"))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| format!("could not read quiet sessions: {e}"))
    }

    pub fn remember_quiet_session(&self, harness_session_id: &str, stamp: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO quiet_sessions (harness_session_id, stamp) VALUES (?1, ?2)",
                params![harness_session_id, stamp],
            )
            .map_err(|e| format!("could not record a quiet session: {e}"))?;
        Ok(())
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
        self.forget_quiet_session(harness_session_id)
    }

    fn forget_quiet_session(&self, harness_session_id: &str) -> Result<(), String> {
        self.conn
            .execute(
                "DELETE FROM quiet_sessions WHERE harness_session_id = ?1",
                params![harness_session_id],
            )
            .map_err(|e| format!("could not forget a quiet session: {e}"))?;
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
        self.all_or_none("create discovered thread", || {
            self.conn
                .execute(
                    "INSERT INTO threads (id, title, cwd, harness_name, harness_session_id, mode, needs_history, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, ?8)",
                    params![id, title, cwd, harness_name, harness_session_id, mode, created, updated],
                )
                .map_err(|e| format!("could not create discovered thread: {e}"))?;
            self.forget_quiet_session(harness_session_id)
        })
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
    /// The thread keeps a Model or effort the person chose before it was
    /// opened, and otherwise takes the session's own. It keeps its mode: a
    /// harness reports its own default for a loaded session, and the mode is
    /// the person's, chosen for each message.
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
        // A reassigned session ID is now live on `thread_id`, so invalidate
        // any prior retirement/ignored entry for it so deleting an older thread
        // never deletes this thread's live session.
        self.conn
            .execute(
                "DELETE FROM ignored_sessions WHERE harness_session_id = ?1",
                params![session_id],
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
                .execute(
                    "UPDATE ignored_sessions SET reason = 'deleted', thread_id = NULL WHERE thread_id = ?1",
                    params![thread],
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
}
