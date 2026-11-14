//! Append-only ledger of every apply, rollback, and failure.

use crate::Result;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub time: String,
    pub actor: String,
    pub action: String,
    pub generation: Option<u32>,
    pub from: Option<u32>,
    pub outcome: String,
    pub detail: String,
}

pub fn append(path: &Path, entry: &Entry) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{}", serde_json::to_string(entry)?)?;
    file.sync_all()?;
    Ok(())
}

pub fn read(path: &Path) -> Result<Vec<Entry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    std::fs::read_to_string(path)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
