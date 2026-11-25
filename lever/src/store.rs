//! Lever's SQLite state store (`lever.sqlite`), kept in the person's home and
//! readable by the person alone.

use crate::sys;
use rusqlite::Connection;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub struct Store {
    path: PathBuf,
    conn: Connection,
}

impl Store {
    /// Opens or creates the SQLite database at `path` in WAL mode with
    /// person-only (`0600`) permissions on the database and its sidecar files.
    pub fn open(path: &Path) -> Result<Self, String> {
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
             INSERT OR IGNORE INTO meta (key, value) VALUES ('schema_version', '1');",
        )
        .map_err(|e| format!("could not initialize database {}: {e}", path.display()))?;

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
