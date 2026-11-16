//! Helpers the test files share.

// Each test file is its own crate and uses only some of these.
#![allow(dead_code)]

use std::collections::BTreeSet;

use crowbar_authority::{Authority, Change};
use crowbar_capability::{Cost, EffectLimits, HandleId, Principal, Resource, Right, Rights, TaskId, Time};
use crowbar_gate::{
    Act, Ask, AskWhy, Caller, Decision, Gate, GateRule, InMemory, Limit, Operation, Presented, Refuse, Use, UseCost,
    decide,
};
use crowbar_labels::{Compartment, LabelMark, ObjectId, Origin};

/// The time every test starts at.
pub const NOW: Time = Time::from_secs(10_000);

pub fn at(secs: u64) -> Time {
    Time::from_secs(secs)
}

/// `secs` seconds after [`NOW`].
pub fn later(secs: u64) -> Time {
    Time::from_secs(NOW.as_secs() + secs)
}

pub fn task(name: &str) -> Principal {
    Principal::Task(TaskId::new(name))
}

pub fn tool(name: &str) -> Principal {
    Principal::Tool(name.to_string())
}

pub fn photo(name: &str) -> ObjectId {
    ObjectId::new(&format!("photos/{name}"))
}

/// The label and mark of something the person wrote: personal, unmarked.
pub fn personal() -> LabelMark {
    Origin::Typed.label_mark()
}

/// The photos the handles in these tests name.
pub const ALBUM: [&str; 5] = ["cat.jpg", "dog.jpg", "fox.jpg", "owl.jpg", "yak.jpg"];

/// A live handle, number `n`, held by `holder`: read and write on the
/// [`ALBUM`], personal, with no limits.
pub fn handle(n: u128, holder: Principal) -> Presented {
    Presented {
        id: HandleId::from_bits(n),
        holder,
        resource: Resource::Data(ALBUM.iter().map(|name| photo(name)).collect()),
        rights: Rights::from([Right::Read, Right::Write]),
        limits: EffectLimits::new(),
        label_mark: personal(),
        delegable: false,
        ended: None,
        original: None,
    }
}

/// A use of the adapter operation `operation`, needing `right`, that
/// affects no object, at no cost.
pub fn using(operation: &str, right: Right) -> Use {
    Use {
        operation: operation.to_string(),
        right,
        objects: BTreeSet::new(),
        cost: UseCost::Fixed(Cost::cents(0)),
    }
}

/// Reading the first `objects` photos of the [`ALBUM`]: inside the undo
/// boundary.
pub fn reading(objects: usize) -> Use {
    Use {
        objects: ALBUM[..objects].iter().map(|name| photo(name)).collect(),
        ..using("data.read", Right::Read)
    }
}

/// `principal`, an ad hoc task or a Tool, in `compartment`, asking for `act`
/// with no idempotency key.
pub fn by<'a>(principal: &'a Principal, compartment: &'a Compartment, act: Act<'a>) -> Operation<'a> {
    Operation {
        caller: Caller {
            principal,
            standing_task: None,
            compartment,
        },
        key: None,
        act,
    }
}

pub fn allowed() -> Decision {
    Decision::Allowed { under: Vec::new() }
}

pub fn refused(rule: GateRule, limit: Limit) -> Decision {
    Decision::Refused(Refuse { rule, limit })
}

pub fn asked(why: AskWhy) -> Decision {
    Decision::Asked(Ask { why })
}

/// A gate that records in memory, with the authority it reads.
///
/// [`Bench::decide`] is how every test reaches a decision, and it runs each
/// one as a dry run first: the dry run must give the same decision as the
/// real operation, and change nothing.
pub struct Bench {
    pub authority: Option<Authority>,
    pub gate: Gate<InMemory>,
}

impl Bench {
    /// A gate under authority that grants nothing.
    pub fn new() -> Self {
        Bench::with(Vec::new())
    }

    /// A gate under the authority these changes, made at [`NOW`], add up
    /// to.
    pub fn with(changes: Vec<Change>) -> Self {
        let authority =
            Authority::from_changes(changes.into_iter().map(|change| (NOW, change))).expect("the changes are admitted");
        Bench {
            authority: Some(authority),
            gate: Gate::start(InMemory::default(), &[]),
        }
    }

    /// A gate whose authority record cannot be read.
    pub fn without_authority() -> Self {
        Bench {
            authority: None,
            gate: Gate::start(InMemory::default(), &[]),
        }
    }

    /// Decides `operation` at `now` as the broker would, after checking that
    /// a dry run gives the same decision and changes nothing.
    pub fn decide(&mut self, operation: &Operation<'_>, now: Time) -> Decision {
        let entries = self.gate.ledger().entries().len();
        let usage = self.gate.usage().clone();

        let dry = self.gate.dry_run(self.authority.as_ref(), operation, now);
        assert_eq!(
            self.gate.ledger().entries().len(),
            entries,
            "a dry run adds to the ledger"
        );
        assert_eq!(self.gate.usage(), &usage, "a dry run changes the count");
        assert_eq!(decide(self.authority.as_ref(), &usage, operation, now), dry);

        let real = self.gate.decide(self.authority.as_ref(), operation, now).decision();
        assert_eq!(
            dry, real,
            "a dry run gives a different decision from the real operation"
        );
        real
    }
}

/// A directory of its own for one test, removed when the test ends.
pub struct Scratch(std::path::PathBuf);

impl Scratch {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "crowbar-gate-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        Scratch(dir)
    }

    pub fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
