//! Turn records, streaming answers, acts, steers, queued messages, notes, and requests.

use super::Store;
use super::records::*;
use crate::sys;
use rusqlite::{OptionalExtension, params};

impl Store {
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

    /// Reserves a `steering` row inside a SQLite transaction, invokes `send`
    /// only after the insert succeeds, and either commits the `steering` row
    /// (`send() == true`) or transitions it to `queued` before committing
    /// (`send() == false`). If any write or commit fails, the transaction
    /// rolls back so an error reply never leaves an orphaned `steering` row.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_steering_or_queue(
        &self,
        thread_id: &str,
        turn_number: u64,
        text: &str,
        context: Option<&str>,
        now: i64,
        send: impl FnOnce() -> bool,
    ) -> Result<(bool, QueuedRecord), String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("could not begin steering transaction: {e}"))?;
        let id = format!("q-{}", sys::random_hex(6));
        tx.execute(
            "INSERT INTO queued (id, thread_id, turn_number, text, context, state, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'steering', ?6)",
            params![id, thread_id, turn_number as i64, text, context, now],
        )
        .map_err(|e| format!("could not queue the message: {e}"))?;
        tx.execute(
            "UPDATE threads SET updated_at = ?1 WHERE id = ?2",
            params![now, thread_id],
        )
        .map_err(|e| format!("could not touch thread timestamp: {e}"))?;

        let sent = send();
        let state = if sent {
            "steering"
        } else {
            tx.execute(
                "UPDATE queued SET state = 'queued' WHERE thread_id = ?1 AND id = ?2 AND state = 'steering'",
                params![thread_id, id],
            )
            .map_err(|e| format!("could not change the queued message: {e}"))?;
            "queued"
        };
        tx.commit()
            .map_err(|e| format!("could not commit queued message: {e}"))?;
        Ok((
            sent,
            QueuedRecord {
                id,
                text: text.to_string(),
                context: context.and_then(|c| serde_json::from_str(c).ok()),
                state: state.to_string(),
            },
        ))
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
}
