//! The ledger: the history of what the gate decided and what was done.

use std::path::Path;

use crowbar_broker::{Principal, Time};
use serde::{Deserialize, Serialize};

use crate::authority::StandingId;
use crate::chain::{self, Chain, Contents, OnDamage, Recorded};
use crate::error::{Damage, Error};

/// The file the ledger is kept in, inside its directory.
const FILE: &str = "entries";

/// One thing that happened: which principal asked for which operation, and
/// what was decided or done.
///
/// An entry names things and never holds one. No field takes a value, a
/// secret, or content: the operation is the name the broker knows it by,
/// and a refusal names its rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEntry {
    /// Who asked, as the broker knows it from the connection.
    pub principal: Principal,
    /// The operation, as the broker names it.
    pub operation: String,
    pub event: Event,
}

/// What a ledger entry records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// The gate's decision on the operation.
    Decided(Decision),
    /// What carrying out an allowed operation did.
    Effect(Outcome),
}

/// What the gate decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Allowed. An operation that crossed the undo boundary names the
    /// standing grant and the standing rule it crossed under.
    Allowed { under: Vec<StandingId> },
    /// Put to the person as an ask.
    Asked,
    /// Refused, under the rule with this name.
    Refused { rule: String },
}

/// What carrying out an operation did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The effect happened.
    Done,
    /// It failed. Whether any effect happened is for the adapter to say.
    Failed,
}

/// What the ledger holds, as far as it can be read.
#[derive(Debug)]
pub struct LedgerHistory {
    /// The entries that are what was written, in order, each with its
    /// number and its time.
    pub entries: Vec<Recorded<LedgerEntry>>,
    /// The lines that do not check out, by number from 1. An unreadable
    /// line is left out of `entries`. An entry that is out of place is kept
    /// there: it is what was written, and something before it is missing or
    /// was moved.
    pub damaged: Vec<(usize, Damage)>,
    /// The size in bytes of a write a crash cut short, found after the last
    /// line and left out.
    pub unfinished_write: Option<usize>,
}

/// The ledger, open for the broker and the gate to add to.
///
/// Entries are added and never changed. A line that goes bad is reported
/// when the ledger is read, and it stops nothing: the entries around it are
/// still read, and the history goes on after it. That is the opposite of the
/// authority domain, which is refused whole, because a history that leaves
/// out a line is still a history and authority that leaves out a revocation
/// is not.
#[derive(Debug)]
pub struct Ledger {
    chain: Chain,
    unfinished_write: Option<usize>,
}

impl Ledger {
    /// Opens the ledger kept in `dir`, and makes it if it is not there. One
    /// writer has it open at a time.
    pub fn open(dir: &Path) -> Result<Ledger, Error> {
        let (chain, contents) = Chain::open::<LedgerEntry>(&dir.join(FILE), OnDamage::Continue)?;
        Ok(Ledger {
            chain,
            unfinished_write: contents.unfinished_write,
        })
    }

    /// The size in bytes of a write a crash cut short, if opening the ledger
    /// found one after the last entry and removed it. Nothing on disk says so
    /// afterwards, so the caller that wants it kept records it.
    pub fn unfinished_write(&self) -> Option<usize> {
        self.unfinished_write
    }

    /// Adds one entry and returns its number. It is on disk when this
    /// returns.
    pub fn append(&mut self, entry: &LedgerEntry, now: Time) -> Result<u64, Error> {
        self.chain.append(now, entry)
    }

    /// Reads the ledger kept in `dir`. A directory that holds none yet has
    /// an empty history.
    pub fn read(dir: &Path) -> Result<LedgerHistory, Error> {
        let contents = chain::read(&dir.join(FILE))?.unwrap_or(Contents {
            entries: Vec::new(),
            damage: Vec::new(),
            unfinished_write: None,
        });
        Ok(LedgerHistory {
            entries: contents.entries,
            damaged: contents.damage,
            unfinished_write: contents.unfinished_write,
        })
    }
}
