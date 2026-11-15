//! Append-only ledger of every apply, rollback, and failure.
//!
//! One JSON object per line. A crash can leave a partial last line; reading
//! skips a damaged line and reports its number, and the next append starts on
//! a fresh line, so one torn write never hides the rest of the history.

use crate::Result;
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    pub entries: Vec<Entry>,
    /// 1-based numbers of the lines that are not a ledger entry.
    pub damaged_lines: Vec<usize>,
}

pub fn append(path: &Path, entry: &Entry) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    let mut line = String::new();
    if file.seek(SeekFrom::End(0))? > 0 {
        let mut last = [0u8; 1];
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last[0] != b'\n' {
            line.push('\n');
        }
    }
    line.push_str(&serde_json::to_string(entry)?);
    line.push('\n');
    // One write, so a crash tears at most this entry.
    file.write_all(line.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

pub fn read(path: &Path) -> Result<Ledger> {
    let mut ledger = Ledger::default();
    if !path.exists() {
        return Ok(ledger);
    }
    let bytes = std::fs::read(path)?;
    // Lines are split as bytes and each must be valid UTF-8 on its own. A lossy
    // conversion would turn a corrupt byte into a replacement character and
    // hand back an altered entry as if it were what was recorded.
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match std::str::from_utf8(line)
            .ok()
            .and_then(|line| serde_json::from_str(line).ok())
        {
            Some(entry) => ledger.entries.push(entry),
            None => ledger.damaged_lines.push(index + 1),
        }
    }
    Ok(ledger)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    fn entry(detail: &str) -> Entry {
        Entry {
            time: "2026-11-14T20:00:00Z".into(),
            actor: "test".into(),
            action: "apply".into(),
            generation: Some(2),
            from: Some(1),
            outcome: "ok".into(),
            detail: detail.into(),
        }
    }

    #[test]
    fn a_torn_line_does_not_hide_the_rest() {
        let dir = scratch();
        let path = dir.0.join("ledger.jsonl");
        append(&path, &entry("first")).unwrap();
        // A crash in the middle of the second append.
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"time\":\"2026-11-14T20:0").unwrap();
        drop(file);

        let torn = read(&path).unwrap();
        assert_eq!(torn.entries, vec![entry("first")]);
        assert_eq!(torn.damaged_lines, vec![2]);

        append(&path, &entry("third")).unwrap();
        let after = read(&path).unwrap();
        assert_eq!(after.entries, vec![entry("first"), entry("third")]);
        assert_eq!(after.damaged_lines, vec![2]);
    }

    #[test]
    fn a_corrupt_byte_makes_the_line_damaged_not_repaired() {
        let dir = scratch();
        let path = dir.0.join("ledger.jsonl");
        append(&path, &entry("first")).unwrap();
        append(&path, &entry("second")).unwrap();
        append(&path, &entry("third")).unwrap();
        // Storage corruption: one byte of the second entry's detail goes bad.
        let mut bytes = std::fs::read(&path).unwrap();
        let at = bytes.windows(6).position(|w| w == b"second").unwrap();
        bytes[at] = 0xff;
        std::fs::write(&path, bytes).unwrap();

        let ledger = read(&path).unwrap();
        assert_eq!(ledger.entries, vec![entry("first"), entry("third")]);
        assert_eq!(ledger.damaged_lines, vec![2]);
    }
}
