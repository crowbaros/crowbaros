//! The append-only file both domains are kept in.
//!
//! One entry per line: the SHA-256 of the rest of the line as 64 hexadecimal
//! digits, a space, and the entry as JSON.
//!
//! ```text
//! <hash of the JSON> {"seq":2,"prev":"<hash of line 1>","time":1000,"entry":{…}}
//! ```
//!
//! `seq` counts from 1 and is the line's number. `prev` is the SHA-256 of the
//! whole line before it, and 64 zeros on the first line.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crowbar_broker::Time;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::Error;

/// One entry as it was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded<T> {
    /// The entry's place in the file, from 1.
    pub seq: u64,
    pub time: Time,
    pub entry: T,
}

/// What a file holds.
#[derive(Debug)]
pub(crate) struct Contents<T> {
    pub entries: Vec<Recorded<T>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    seq: u64,
    prev: String,
    time: Time,
    entry: T,
}

/// The file, open for appending.
#[derive(Debug)]
pub(crate) struct Log {
    file: File,
    path: PathBuf,
    /// How many entries the file holds.
    seq: u64,
    /// The hash of the last line, which the next entry names.
    last: String,
}

const NO_LINE: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn hash(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(text, "{byte:02x}").expect("writing to a string");
    }
    text
}

fn io(path: &Path) -> impl Fn(std::io::Error) -> Error + '_ {
    move |error| Error::Io {
        path: path.to_path_buf(),
        error,
    }
}

/// Reads the file at `path`. A file that is not there holds nothing.
pub(crate) fn read<T: DeserializeOwned>(path: &Path) -> Result<Contents<T>, Error> {
    Ok(scan(path)?.0)
}

/// The file's contents, with the hash of its last line.
fn scan<T: DeserializeOwned>(path: &Path) -> Result<(Contents<T>, String), Error> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(io(path)(error)),
    };
    let mut entries = Vec::new();
    let mut last = NO_LINE.to_string();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\n").unwrap_or(line);
        let envelope: Envelope<T> = serde_json::from_slice(&line[65..]).expect("an entry");
        entries.push(Recorded {
            seq: envelope.seq,
            time: envelope.time,
            entry: envelope.entry,
        });
        last = hash(line);
    }
    Ok((Contents { entries }, last))
}

impl Log {
    /// Opens the file at `path` for appending, and makes it and its directory
    /// if they are not there.
    pub fn open<T: DeserializeOwned>(path: &Path) -> Result<(Log, Contents<T>), Error> {
        let dir = path.parent().expect("a log has a directory");
        std::fs::create_dir_all(dir).map_err(io(dir))?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(io(path))?;
        // A new file is only durable once its directory entry is on disk.
        File::open(dir).and_then(|dir| dir.sync_all()).map_err(io(dir))?;
        let (contents, last) = scan(path)?;
        let log = Log {
            file,
            path: path.to_path_buf(),
            seq: contents.entries.len() as u64,
            last,
        };
        Ok((log, contents))
    }

    /// Appends one entry and returns its place in the file. The entry is on
    /// disk when this returns.
    pub fn append<T: Serialize>(&mut self, time: Time, entry: &T) -> Result<u64, Error> {
        let seq = self.seq + 1;
        let json = serde_json::to_string(&Envelope {
            seq,
            prev: self.last.clone(),
            time,
            entry,
        })
        .map_err(|error| Error::NotStorable(error.to_string()))?;
        let line = format!("{} {json}", hash(json.as_bytes()));
        // One write, so a crash tears at most this entry.
        self.file
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|()| self.file.sync_all())
            .map_err(io(&self.path))?;
        self.seq = seq;
        self.last = hash(line.as_bytes());
        Ok(seq)
    }
}
