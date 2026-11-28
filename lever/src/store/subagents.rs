//! Subagent records, messages, and acts.

use super::records::*;
use super::Store;
use crate::sys;
use rusqlite::{OptionalExtension, params};

impl Store {
    pub(super) fn subagent_messages(&self, subagent_id: &str) -> Result<Vec<SubagentMessageRecord>, String> {
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


    pub(super) fn subagent_acts(&self, subagent_id: &str) -> Result<Vec<ActRecord>, String> {
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

    /// Returns the turn number a recorded subagent belongs to, if known.
    pub fn subagent_turn(&self, thread_id: &str, harness_session_id: &str) -> Result<Option<u64>, String> {
        self.conn
            .query_row(
                "SELECT turn_number FROM subagents WHERE thread_id = ?1 AND harness_session_id = ?2",
                params![thread_id, harness_session_id],
                |r| Ok(r.get::<_, i64>(0)? as u64),
            )
            .optional()
            .map_err(|e| format!("could not query subagent turn: {e}"))
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

