//! SQLite schema definition, incremental migrations, and startup crash recovery.

use rusqlite::{Connection, params};
use std::path::Path;

pub(super) fn initialize(conn: &Connection, path: &Path, now: i64) -> Result<(), String> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;\n\
             PRAGMA synchronous = NORMAL;\n\
             PRAGMA foreign_keys = ON;\n\
             PRAGMA busy_timeout = 5000;\n\
             CREATE TABLE IF NOT EXISTS meta (\n\
                 key TEXT PRIMARY KEY,\n\
                 value TEXT NOT NULL\n\
             );\n\
             INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', '11');\n\
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
                 needs_handoff INTEGER NOT NULL DEFAULT 0,\n\
                 created_at INTEGER NOT NULL,\n\
                 updated_at INTEGER NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS ignored_sessions (\n\
                 harness_session_id TEXT PRIMARY KEY,\n\
                 reason TEXT NOT NULL,\n\
                 created_at INTEGER NOT NULL,\n\
                 thread_id TEXT\n\
             );\n\
             CREATE TABLE IF NOT EXISTS quiet_sessions (\n\
                 harness_session_id TEXT PRIMARY KEY,\n\
                 stamp TEXT NOT NULL\n\
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
        ensure_column(conn, "threads", "model", "TEXT")?;
        ensure_column(conn, "threads", "archived", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(conn, "turns", "context", "TEXT")?;
        ensure_column(conn, "threads", "mode", "TEXT")?;
        ensure_column(conn, "turns", "mode", "TEXT")?;
        ensure_column(conn, "threads", "effort", "TEXT")?;
        ensure_column(conn, "requests", "subagent_id", "TEXT")?;
        ensure_column(conn, "turns", "origin", "TEXT")?;
        ensure_column(conn, "threads", "needs_history", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(conn, "threads", "needs_handoff", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(conn, "ignored_sessions", "thread_id", "TEXT")?;

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

    Ok(())
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

