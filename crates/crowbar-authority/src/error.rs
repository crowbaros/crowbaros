//! Why a store could not be read or written.

use std::fmt;
use std::path::PathBuf;

use crate::authority::StandingId;

/// Why the authority domain or the ledger could not be read or written.
#[derive(Debug)]
pub enum Error {
    /// The file or its directory could not be read or written.
    Io { path: PathBuf, error: std::io::Error },
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
            Error::NotStorable(why) => write!(f, "the entry cannot be stored: {why}"),
            Error::NotStanding(id) => write!(f, "no standing grant or rule has the id {id}"),
            Error::RuleWithoutRate => f.write_str("a standing rule needs a rate limit"),
        }
    }
}

impl std::error::Error for Error {}
