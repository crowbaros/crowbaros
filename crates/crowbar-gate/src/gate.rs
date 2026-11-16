//! The gate as the broker runs it: each decision recorded in the ledger and
//! counted.

use crowbar_authority::{Authority, Decision, Error, Event, Keyed, Ledger, LedgerEntry, Recorded, StandingId};
use crowbar_capability::{Ask, EffectLimits, GateRule, HandleId, Issue, Principal, Refuse, Rights, Time};

use crate::decide::decide;
use crate::operation::{Act, Operation, UseCost};
use crate::usage::Usage;

/// Where the gate records its decisions: the ledger, or a record kept in
/// memory for a rehearsal.
pub trait Record {
    /// Adds one entry and returns its number. When this returns, the entry
    /// is kept.
    fn append(&mut self, entry: &LedgerEntry, now: Time) -> Result<u64, Error>;
}

impl Record for Ledger {
    fn append(&mut self, entry: &LedgerEntry, now: Time) -> Result<u64, Error> {
        Ledger::append(self, entry, now)
    }
}

/// A record kept in memory, for a rehearsal on a simulated clock and for
/// tests. A restart forgets it, and the count with it, so a broker that
/// serves the person records to the [`Ledger`].
#[derive(Debug, Default)]
pub struct InMemory {
    entries: Vec<Recorded<LedgerEntry>>,
}

impl InMemory {
    pub fn entries(&self) -> &[Recorded<LedgerEntry>] {
        &self.entries
    }
}

impl Record for InMemory {
    fn append(&mut self, entry: &LedgerEntry, now: Time) -> Result<u64, Error> {
        let seq = self.entries.len() as u64 + 1;
        self.entries.push(Recorded {
            seq,
            time: now,
            entry: entry.clone(),
        });
        Ok(seq)
    }
}

/// The gate's allowance for one operation: it stands in for the gate's
/// signature until the gate has a process of its own.
///
/// Only the gate makes one, after it has recorded the decision in the
/// ledger, and it covers that one operation. The broker's table issues a
/// handle, or passes one on, only with an allowance that covers it.
///
/// It cannot be made outside this crate:
///
/// ```compile_fail,E0451
/// fn forge() -> crowbar_gate::Allowed {
///     crowbar_gate::Allowed { entry: 1, under: Vec::new(), covers: unreachable!() }
/// }
/// ```
///
/// nor copied, so one allowance is spent once:
///
/// ```compile_fail,E0599
/// fn copy(allowed: &crowbar_gate::Allowed) -> crowbar_gate::Allowed {
///     allowed.clone()
/// }
/// ```
#[derive(Debug)]
pub struct Allowed {
    entry: u64,
    under: Vec<StandingId>,
    covers: Box<Covers>,
}

/// The one operation an allowance covers.
#[derive(Debug)]
enum Covers {
    Issue(Issue),
    Delegate {
        original: HandleId,
        to: Principal,
        rights: Rights,
        limits: EffectLimits,
    },
    Use {
        handle: HandleId,
        operation: String,
    },
}

impl Allowed {
    /// The number of the ledger entry that records the decision.
    pub fn entry(&self) -> u64 {
        self.entry
    }

    /// The standing grant and standing rule a crossing was allowed under.
    pub fn under(&self) -> &[StandingId] {
        &self.under
    }

    /// The handle to issue, if this allows an issue. Otherwise the
    /// allowance comes back unspent.
    pub fn into_issue(self) -> Result<Issue, Allowed> {
        match *self.covers {
            Covers::Issue(issue) => Ok(issue),
            covers => Err(Allowed {
                covers: Box::new(covers),
                ..self
            }),
        }
    }

    /// Whether this allows passing `original` on to `to` with exactly these
    /// rights and limits.
    pub fn covers_delegation(
        &self,
        original: HandleId,
        to: &Principal,
        rights: &Rights,
        limits: &EffectLimits,
    ) -> bool {
        matches!(
            &*self.covers,
            Covers::Delegate { original: allowed, to: allowed_to, rights: allowed_rights, limits: allowed_limits }
                if *allowed == original && allowed_to == to && allowed_rights == rights && allowed_limits == limits
        )
    }

    /// Whether this allows the adapter operation `operation` on `handle`.
    pub fn covers_use(&self, handle: HandleId, operation: &str) -> bool {
        matches!(&*self.covers, Covers::Use { handle: allowed, operation: name } if *allowed == handle && name == operation)
    }
}

/// What the gate ruled on an operation it was asked to carry out.
#[derive(Debug)]
pub enum Ruling {
    /// Allowed: carry it out with this allowance.
    Allowed(Allowed),
    Refused(Refuse),
    /// Put to the person. What the ask shows, and what happens after it, is
    /// not the gate's.
    Asked(Ask),
    /// The idempotency key was decided before, with this decision. Nothing is
    /// carried out again and nothing is counted again, so an allowed repeat
    /// carries no allowance.
    Repeated(Decision),
}

impl Ruling {
    /// The decision, as a dry run would give it.
    pub fn decision(&self) -> Decision {
        match self {
            Ruling::Allowed(allowed) => Decision::Allowed {
                under: allowed.under.clone(),
            },
            Ruling::Refused(refuse) => Decision::Refused(refuse.clone()),
            Ruling::Asked(ask) => Decision::Asked(*ask),
            Ruling::Repeated(decision) => decision.clone(),
        }
    }
}

/// The gate as the broker runs it: [`decide`], with each decision recorded
/// and counted.
///
/// It starts from the ledger's history and counts on from there. Every
/// decision is recorded before it is returned: allowed, refused, or asked.
/// A ledger that cannot be added to refuses every operation under
/// [`GateRule::Ledger`], because an allowance that is not recorded is not
/// counted. Dry runs still answer.
#[derive(Debug)]
pub struct Gate<L: Record> {
    ledger: L,
    usage: Usage,
}

/// The ledger's name for closing an ask.
const ASK_CLOSE: &str = "ask.close";

impl<L: Record> Gate<L> {
    /// A gate that records to `ledger`, counting on from `history`: what
    /// [`Ledger::read`] gives for the same directory, read after the ledger
    /// is opened, so no other writer adds to it in between.
    pub fn start(ledger: L, history: &[Recorded<LedgerEntry>]) -> Self {
        Gate {
            ledger,
            usage: Usage::rebuild(history),
        }
    }

    /// Decides `operation` at `now`, records the decision, and counts it.
    /// `authority` is `None` when the authority record cannot be read.
    pub fn decide(&mut self, authority: Option<&Authority>, operation: &Operation<'_>, now: Time) -> Ruling {
        let decision = decide(authority, &self.usage, operation, now);
        let principal = operation.caller.principal;
        let repeated = operation
            .key
            .and_then(|key| self.usage.first(principal, key))
            .is_some_and(|first| first.digest == operation.digest());
        if repeated {
            return Ruling::Repeated(decision);
        }
        let covers = covers(operation);
        let entry = entry_for(operation, &decision);
        let Ok(seq) = self.ledger.append(&entry, now) else {
            return Ruling::Refused(Refuse::by(GateRule::Ledger));
        };
        self.usage.record(&entry, now);
        match (decision, covers) {
            (Decision::Allowed { under }, Some(covers)) => Ruling::Allowed(Allowed {
                entry: seq,
                under,
                covers: Box::new(covers),
            }),
            (Decision::Allowed { .. }, None) => Ruling::Refused(Refuse::by(GateRule::Unknown)),
            (Decision::Asked(ask), _) => Ruling::Asked(ask),
            (Decision::Refused(refuse), _) => Ruling::Refused(refuse),
        }
    }

    /// What [`Gate::decide`] would decide for `operation` at `now`, without
    /// carrying it out. It counts no use, opens no ask, issues nothing, and
    /// adds nothing to the ledger. It reveals nothing a real operation would
    /// not: the answer is the same decision.
    pub fn dry_run(&self, authority: Option<&Authority>, operation: &Operation<'_>, now: Time) -> Decision {
        decide(authority, &self.usage, operation, now)
    }

    /// Records that `principal`'s open ask closed: answered, withdrawn, or
    /// ended with its task.
    pub fn ask_closed(&mut self, principal: &Principal, now: Time) -> Result<u64, Refuse> {
        let entry = LedgerEntry::new(principal.clone(), ASK_CLOSE, Event::AskClosed);
        let seq = self
            .ledger
            .append(&entry, now)
            .map_err(|_| Refuse::by(GateRule::Ledger))?;
        self.usage.record(&entry, now);
        Ok(seq)
    }

    /// What the gate has counted.
    pub fn usage(&self) -> &Usage {
        &self.usage
    }

    /// Where the gate records.
    pub fn ledger(&self) -> &L {
        &self.ledger
    }
}

/// What an allowance for `operation` covers, if it names a handle where it
/// must.
fn covers(operation: &Operation<'_>) -> Option<Covers> {
    Some(match &operation.act {
        Act::Issue { issue, .. } => Covers::Issue((*issue).clone()),
        Act::Delegate {
            handle,
            to,
            rights,
            limits,
        } => Covers::Delegate {
            original: handle.as_ref()?.id,
            to: (*to).clone(),
            rights: (*rights).clone(),
            limits: (*limits).clone(),
        },
        Act::Use { handle, using } | Act::Egress { handle, using, .. } => Covers::Use {
            handle: handle.as_ref()?.id,
            operation: using.operation.clone(),
        },
    })
}

/// The ledger entry that records `decision` on `operation`.
fn entry_for(operation: &Operation<'_>, decision: &Decision) -> LedgerEntry {
    let principal = operation.caller.principal;
    let mut entry = LedgerEntry::new(principal.clone(), operation.name(), Event::Decided(decision.clone()));
    if let Principal::Task(task) = principal {
        entry.task = Some(task.clone());
    }
    match &operation.act {
        Act::Issue { .. } => {}
        // A delegation names the handle passed on as the copy's original.
        // The copy has no id until the table issues it.
        Act::Delegate { handle, .. } => entry.original = handle.map(|handle| handle.id),
        Act::Use { handle, using } | Act::Egress { handle, using, .. } => {
            entry.handle = handle.map(|handle| handle.id);
            entry.original = handle
                .and_then(|handle| handle.original.as_ref())
                .map(|original| original.id);
            entry.objects = Some(using.units());
            if let UseCost::Fixed(cost) = using.cost {
                entry.cost = Some(cost);
            }
        }
    }
    entry.key = operation.key.map(|key| Keyed {
        key: key.clone(),
        digest: operation.digest(),
    });
    entry
}
