//! Lever's SQLite state store (`lever.sqlite`), kept in the person's home and
//! readable by the person alone.
//!
//! Keeps every thread, its turns, its acts, its requests, its subagents, its
//! steers/queued messages, and its notes in one harness-agnostic form.
//! Organized internally by aggregate while keeping [`Store`] concrete against
//! real SQLite.

mod records;
mod schema;
mod subagents;
mod threads;
mod turns;

#[cfg(test)]
mod tests;

pub use records::*;

use crate::sys;
use rusqlite::{Connection, OptionalExtension, params};
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub struct Store {
    pub(super) path: PathBuf,
    pub(super) conn: Connection,
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
        schema::initialize(&conn, path, now)?;
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
