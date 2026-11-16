//! The append-only file both domains are kept in.
//!
//! One entry per line: the SHA-256 of the rest of the line as 64 hexadecimal
//! digits, a space, and the entry as JSON.
//!
//! ```text
//! <hash of the JSON> {"seq":2,"prev":"<hash of line 1>","time":1000,"entry":{…}}
//! ```
//!
//! `seq` counts from 1, one more on each line. `prev` is the SHA-256 of the
//! whole line before it, and 64 zeros on the first line. So a line that was
//! changed does not match its own hash, and a line that was removed, added,
//! or moved leaves a line that does not follow the one before it.
//!
//! An entry is one write, flushed before the append returns. A crash can
//! leave a part of the last one, and that part is not a line that checks
//! out, so a reader leaves it out and the next writer removes it. A last
//! entry that checks out and lacks only its line break is kept.
//!
//! What this does not catch: the file replaced by an earlier whole copy of
//! itself, or cut back. A file cut at the end of an entry reads as a shorter
//! file, and one cut inside its last entry reads as a crash during that
//! write. Telling either from the truth needs a count kept outside the file,
//! and that comes with signatures and hardware-bound keys.

use std::fs::{DirBuilder, File, OpenOptions, TryLockError};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use crowbar_capability::Time;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Damage, Error};

/// One entry as it was recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded<T> {
    /// The entry's number, from 1: its place among everything recorded.
    pub seq: u64,
    pub time: Time,
    pub entry: T,
}

/// What a file holds.
#[derive(Debug)]
pub(crate) struct Contents<T> {
    /// The entries that are what was written, in order. One that is out of
    /// place is here too, and in `damage`.
    pub entries: Vec<Recorded<T>>,
    /// The lines that do not check out, by number from 1.
    pub damage: Vec<(usize, Damage)>,
    /// The size in bytes of a write a crash cut short, found after the last
    /// line and left out.
    pub unfinished_write: Option<usize>,
}

impl<T> Contents<T> {
    /// The first line that does not check out, as the error that refuses the
    /// whole file.
    pub fn first_damage(&self, path: &Path) -> Result<(), Error> {
        match self.damage.first() {
            Some((line, damage)) => Err(Error::Damaged {
                path: path.to_path_buf(),
                line: *line,
                damage: *damage,
            }),
            None => Ok(()),
        }
    }
}

/// What a writer does with a file that holds a damaged line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OnDamage {
    /// Refuse the file and leave it as it was found.
    Refuse,
    /// Go on appending after it. Reading reports the damaged lines.
    Continue,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope<T> {
    seq: u64,
    prev: String,
    time: Time,
    entry: T,
}

/// The file, open for appending by its one writer.
#[derive(Debug)]
pub(crate) struct Chain {
    file: File,
    path: PathBuf,
    /// The number the next entry takes.
    next: u64,
    /// The hash of the last line, which the next entry names.
    last: String,
    /// Whether a write failed. How much of it reached the disk is not known,
    /// so nothing more is written until the file is opened again, which
    /// finds out.
    write_failed: bool,
}

/// What `prev` names on the first line.
const NO_LINE: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The length of a hash as text, and of the space after it.
const HASH_LEN: usize = 64;

fn hash(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut text = String::with_capacity(HASH_LEN);
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

/// Reads the file at `path`, or says there is none.
pub(crate) fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<Contents<T>>, Error> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(scan(&bytes).contents)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io(path)(error)),
    }
}

/// A file's contents, with what a writer needs to go on from them.
struct Scan<T> {
    contents: Contents<T>,
    /// The number the next entry takes.
    next: u64,
    /// The hash of the last line.
    last: String,
    /// Whether the last line is a whole entry with no line break after it.
    unterminated: bool,
}

fn scan<T: DeserializeOwned>(bytes: &[u8]) -> Scan<T> {
    let mut scan = Scan {
        contents: Contents {
            entries: Vec::new(),
            damage: Vec::new(),
            unfinished_write: None,
        },
        next: 1,
        last: NO_LINE.to_string(),
        unterminated: false,
    };
    // An unreadable line has already been reported, so the line after it is
    // not reported again for not naming it.
    let mut follows_unreadable = false;
    for (index, chunk) in bytes.split_inclusive(|byte| *byte == b'\n').enumerate() {
        let (line, ended) = match chunk.strip_suffix(b"\n") {
            Some(line) => (line, true),
            None => (chunk, false),
        };
        let envelope = read_line::<T>(line);
        if !ended && envelope.is_none() {
            // Only the last chunk has no line break. It is not an entry, so
            // it is what a crash left of a write.
            scan.contents.unfinished_write = Some(line.len());
            break;
        }
        match envelope {
            Some(envelope) => {
                let follows = follows_unreadable || envelope.prev == scan.last;
                if envelope.seq != scan.next || !follows {
                    scan.contents.damage.push((index + 1, Damage::OutOfPlace));
                }
                // The count goes on from this entry, so a gap is reported
                // once, where it is.
                scan.next = envelope.seq.saturating_add(1);
                scan.contents.entries.push(Recorded {
                    seq: envelope.seq,
                    time: envelope.time,
                    entry: envelope.entry,
                });
                follows_unreadable = false;
            }
            None => {
                scan.contents.damage.push((index + 1, Damage::Unreadable));
                scan.next = scan.next.saturating_add(1);
                follows_unreadable = true;
            }
        }
        scan.last = hash(line);
        scan.unterminated = !ended;
    }
    scan
}

/// Reads one line, without its line break, if it is the entry that was
/// written: its hash matches what it holds.
fn read_line<T: DeserializeOwned>(line: &[u8]) -> Option<Envelope<T>> {
    let (stated, json) = line.split_at_checked(HASH_LEN)?;
    let json = json.strip_prefix(b" ")?;
    if stated != hash(json).as_bytes() {
        return None;
    }
    serde_json::from_slice(json).ok()
}

impl Chain {
    /// Opens the file at `path` for appending, and makes it and its directory
    /// if they are not there. One writer has it open at a time.
    ///
    /// What a crash left of a write is removed, and a last entry with no
    /// line break gets one, so the next entry starts on a line of its own.
    pub fn open<T: DeserializeOwned>(path: &Path, on_damage: OnDamage) -> Result<(Chain, Contents<T>), Error> {
        let dir = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        // The directory and the file are made for their owner alone.
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(io(dir))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(io(path))?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(Error::Locked {
                    path: path.to_path_buf(),
                });
            }
            Err(TryLockError::Error(error)) => return Err(io(path)(error)),
        }
        // A new file is only durable once its directory entry is on disk.
        File::open(dir).and_then(|dir| dir.sync_all()).map_err(io(dir))?;

        let bytes = std::fs::read(path).map_err(io(path))?;
        let scan = scan::<T>(&bytes);
        if on_damage == OnDamage::Refuse {
            scan.contents.first_damage(path)?;
        }
        if let Some(unfinished) = scan.contents.unfinished_write {
            file.set_len((bytes.len() - unfinished) as u64).map_err(io(path))?;
            file.sync_all().map_err(io(path))?;
        } else if scan.unterminated {
            file.write_all(b"\n").map_err(io(path))?;
            file.sync_all().map_err(io(path))?;
        }
        let chain = Chain {
            file,
            path: path.to_path_buf(),
            next: scan.next,
            last: scan.last,
            write_failed: false,
        };
        Ok((chain, scan.contents))
    }

    /// Appends one entry and returns its place in the file. The entry is on
    /// disk when this returns.
    pub fn append<T: Serialize>(&mut self, time: Time, entry: &T) -> Result<u64, Error> {
        self.usable()?;
        let seq = self.next;
        let json = serde_json::to_string(&Envelope {
            seq,
            prev: self.last.clone(),
            time,
            entry,
        })
        .map_err(|error| Error::NotStorable(error.to_string()))?;
        let line = format!("{} {json}", hash(json.as_bytes()));
        // One write, so a crash tears at most this entry.
        let written = self
            .file
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|()| self.file.sync_all());
        if let Err(error) = written {
            self.write_failed = true;
            return Err(io(&self.path)(error));
        }
        self.next = seq + 1;
        self.last = hash(line.as_bytes());
        Ok(seq)
    }

    /// Whether every write through this writer reached the disk. After one
    /// that failed, what the writer knows may not be what the file holds.
    pub fn usable(&self) -> Result<(), Error> {
        match self.write_failed {
            true => Err(Error::AfterFailedWrite {
                path: self.path.clone(),
            }),
            false => Ok(()),
        }
    }
}
