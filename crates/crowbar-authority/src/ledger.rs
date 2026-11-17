//! The ledger: the history of what the gate decided and what was done.

use std::path::Path;

use crowbar_capability::{Ask, Cost, HandleId, IdempotencyKey, Principal, Refuse, TaskId, Time};
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
/// and a refusal names its rule and the limit that applied.
///
/// The gate rebuilds its count of uses from these entries when it starts,
/// so an entry for a use names the handle, its original, how many objects
/// it affected, and its cost. A field that does not apply is left out when
/// the entry is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerEntry {
    /// Who asked, as the broker knows it from the connection.
    pub principal: Principal,
    /// The operation, as the broker names it.
    pub operation: String,
    pub event: Event,
    /// The handle the operation used, or passed on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<HandleId>,
    /// For a copy, the handle it was passed on from. Its uses count
    /// against that original too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<HandleId>,
    /// The task the operation served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<TaskId>,
    /// How many objects the operation affected. One that affects no object
    /// counts as one operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub objects: Option<u64>,
    /// What the operation cost, where that was a fixed amount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
    /// The idempotency key the task attached, with the digest of the
    /// operation it was attached to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Keyed>,
}

impl LedgerEntry {
    /// An entry that names only who asked, the operation, and what happened.
    pub fn new(principal: Principal, operation: &str, event: Event) -> Self {
        LedgerEntry {
            principal,
            operation: operation.to_string(),
            event,
            handle: None,
            original: None,
            task: None,
            objects: None,
            cost: None,
            key: None,
        }
    }
}

/// An idempotency key as the ledger records it: the key, and the SHA-256 of
/// the operation it was first attached to, as 64 hexadecimal digits. The
/// same key on a different operation has a different digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Keyed {
    pub key: IdempotencyKey,
    pub digest: String,
}

/// What a ledger entry records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    /// The gate's decision on the operation.
    Decided(Decision),
    /// What carrying out an allowed operation did.
    Effect(Outcome),
    /// One of the principal's waiting asks closed: answered, declined,
    /// withdrawn, or ended with its task. It does not say which. The gate
    /// keeps a count, and a principal can have held asks waiting too.
    AskClosed,
}

/// What the gate decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Allowed. An operation that crossed the undo boundary names the
    /// standing grant and the standing rule it crossed under.
    Allowed { under: Vec<StandingId> },
    /// Put to the person as an ask.
    Asked(Ask),
    /// Refused: the rule, and the limit that applied.
    Refused(Refuse),
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
