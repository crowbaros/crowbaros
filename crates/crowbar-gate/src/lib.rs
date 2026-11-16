//! The gate: the deterministic rules engine that is the only thing that
//! allows an action.
//!
//! For each operation the broker states, the gate allows, refuses, or asks.
//! The operations are **issue**, **delegate**, **use** (an adapter operation
//! on a handle), and **egress**. It checks the handle, the right, the effect
//! limits, the label and mark at issuance, the clearance on egress, and the
//! undo boundary, with the standing grant and standing rule a crossing needs.
//! The order is fixed and written out on [`decide`].
//!
//! - [`decide`] is the decision, and it is pure: it reads no file, no clock,
//!   and no network. It decides only from metadata, and a check that would
//!   depend on a value the task cannot read asks, so the person sees the
//!   value.
//! - A **dry run** ([`Gate::dry_run`]) is the same decision, with nothing
//!   counted, no ask opened, nothing issued, and nothing added to the ledger.
//! - A **refusal** names a rule and the limit that applied, from one closed
//!   list ([`GateRule`], [`Limit`]) that the ledger and the harness share. An
//!   **ask** says why it asks ([`AskWhy`]).
//! - The [`Gate`] records each decision in the ledger and counts uses from
//!   it. It rebuilds the count from the ledger's history when it starts, so a
//!   restart resets no limit, and it keeps the first decision for each
//!   **idempotency key**.
//! - [`Allowed`] stands in for the gate's signature until the gate has a
//!   process of its own. Only the gate makes one, it cannot be copied, and
//!   the broker's table issues and passes on handles only with one.
//!
//! It fails closed: an unknown handle, principal, or operation is refused;
//! with no authority record to read, every crossing asks and no party is
//! cleared above public; and a ledger that cannot be added to refuses
//! everything.
//!
//! It reads authority and never writes it: it is given a
//! `crowbar_authority::Authority`, which has nothing that writes. It works
//! with no model at all, and it is in the trusted base, so it must stay small
//! enough to test completely.
//!
//! Not here yet:
//!
//! - what happens after an ask, authorization at a task's start, releases,
//!   the ceiling, and budgets across tasks (issue 103);
//! - the check on a commit of staged writes (issue 90);
//! - the label check on a handle that is not a Data handle (issue 91);
//! - a signature, and a process of its own;
//! - machine clearances (Milestone 6).
//!
//! Decisions: ADR 0003, ADR 0011, and ADR 0021 in `docs/adr/`.
//! Design: `docs/architecture/gate-monitor-and-autonomy.md`, "The gate".
//! Scope: `docs/milestones/milestone-2.md`. Glossary: gate, dry run,
//! idempotency key.

mod boundary;
mod decide;
mod gate;
mod operation;
mod usage;

pub use boundary::{INSIDE, crosses};
pub use crowbar_authority::Decision;
pub use crowbar_capability::{Ask, AskWhy, GateRule, Limit, Refuse};
pub use decide::{AGENT_ASKS_AN_HOUR, ASKS_AN_HOUR, OPEN_ASKS, decide};
pub use gate::{Allowed, Gate, InMemory, Record, Ruling};
pub use operation::{Act, Caller, Ending, Operation, Original, Presented, Use, UseCost};
pub use usage::Usage;
