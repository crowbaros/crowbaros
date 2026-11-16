//! Why a store could not be read or written.

use std::fmt;
use std::path::PathBuf;

use crate::authority::StandingId;

/// Why the authority domain or the ledger could not be read or written.
#[derive(Debug)]
pub enum Error {
    /// The file or its directory could not be read or written.
    Io { path: PathBuf, error: std::io::Error },
    /// Another writer has the file open. Each domain has one writer.
    Locked { path: PathBuf },
    /// The file holds a line that cannot be trusted, so none of it is read.
    /// `line` counts from 1.
    Damaged { path: PathBuf, line: usize, damage: Damage },
    /// An earlier write through this writer failed, and how much of it
    /// reached the disk is not known. Opening the file again finds out.
    Broken { path: PathBuf },
    /// The entry cannot be written down, such as a path that is not text.
    NotStorable(String),
    /// No standing grant or rule has this id.
    NotStanding(StandingId),
    /// A standing rule always has a rate limit, and this one has none.
    RuleWithoutRate,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { path, error } => write!(f, "{}: {error}", path.display()),
            Error::Locked { path } => write!(f, "{}: another writer has it open", path.display()),
            Error::Damaged { path, line, damage } => write!(f, "{}: line {line} {damage}", path.display()),
            Error::Broken { path } => write!(f, "{}: an earlier write failed; open it again", path.display()),
            Error::NotStorable(why) => write!(f, "the entry cannot be stored: {why}"),
            Error::NotStanding(id) => write!(f, "no standing grant or rule has the id {id}"),
            Error::RuleWithoutRate => f.write_str("a standing rule needs a rate limit"),
        }
    }
}

impl std::error::Error for Error {}

/// What is wrong with one line of a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Damage {
    /// The line is not an entry, or is not the entry that was written: its
    /// hash does not match what it holds.
    Unreadable,
    /// The line is a whole entry, and it does not follow the line before it:
    /// an entry was removed, added, or moved.
    OutOfPlace,
    /// The line is a whole entry in its place, and it records a change the
    /// entries before it do not admit, such as revoking what was never
    /// granted.
    NotAdmitted,
}

impl fmt::Display for Damage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Damage::Unreadable => "is not the entry that was written",
            Damage::OutOfPlace => "does not follow the line before it",
            Damage::NotAdmitted => "records a change the entries before it do not admit",
        })
    }
}
