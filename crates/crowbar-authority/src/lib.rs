//! The authority domain and the ledger: what the person granted, and what
//! was decided and done, each kept in a directory of its own.
//!
//! Neither is part of the Crowbar State or of a generation (ADR 0012). A
//! generation is a build of the State, so nothing here is copied when one is
//! made, and nothing here goes back when one is rolled back: undoing software
//! never brings back a revoked grant. A State that is shared carries none of
//! it.
//!
//! - **Authority** is standing grants, standing rules, clearances, and the
//!   ceiling on tasks that do not cross the undo boundary. The
//!   [`AuthorityStore`] records each change the person makes, and
//!   [`Authority`] is what the changes add up to. The gate is given an
//!   `Authority`, which reads and has nothing that writes.
//! - A standing grant or rule is held by a [`Holder`]: a Tool, a standing
//!   task, or a workbench. The Agent is not one, so it cannot be recorded as
//!   holding either (ADR 0011).
//! - **Revoking** adds a change and takes nothing away. A revoked grant or
//!   rule stays in the record as revoked, and no change brings it back:
//!   granting the same thing again is a new grant with a new [`StandingId`].
//! - A [`Party`] with no recorded clearance has the default for its kind. A
//!   clearance the person lowered is a record, so it stays lowered.
//! - The [`Ledger`] is the history of gate decisions and of effects. It is
//!   only ever added to. An entry for a decision names enough for the gate
//!   to rebuild its count of uses from it: the handle, its original, the
//!   task, the objects, the cost, and the idempotency key.
//! - [`Authority::from_changes`] makes authority in memory, for a rehearsal
//!   on a simulated clock and for tests. It is admitted as a record is.
//!
//! Both are kept the same way, in a file that is only appended to. Each
//! entry carries a hash of itself and of the entry before it, and is on disk
//! before the call that made it returns.
//!
//! - **A crash** loses at most the write it interrupted. What it left is
//!   reported, and removed by the next writer.
//! - **A missing authority record is refused**, not read as an empty one. A
//!   new machine makes its record once, with [`AuthorityStore::create`].
//! - **A damaged authority record is refused whole**, and the error names the
//!   line: a changed byte, a removed or reordered change, or a change the
//!   ones before it do not admit. Reading around it could leave out a
//!   revocation or read a limit as a wider one, so nothing is read, and with
//!   no authority to allow under, every crossing asks.
//! - **A damaged ledger line is reported** by number, and the rest of the
//!   history is still read and still added to.
//!
//! Not here yet:
//!
//! - who may write: the person's signature over each change to authority
//!   (ADR 0014), and file ownership that keeps every task out;
//! - the gate's signature on each ledger entry, and a count kept outside the
//!   files, without which a file cut back, or replaced by an earlier whole
//!   copy of itself, is not caught;
//! - a grant's binding to a Tool's needs and a clean build, budgets, and
//!   Allowances;
//! - a machine's clearance, the lead machine, and leases (Milestone 6);
//! - the vault, which is a domain of its own (Milestone 5);
//! - the apply engine's history of applies and rollbacks, which stays in its
//!   Milestone 1 file.
//!
//! Decisions: ADR 0003, ADR 0011, ADR 0012, and ADR 0021 in `docs/adr/`. Design:
//! `docs/architecture/gate-monitor-and-autonomy.md`, "Standing rules and
//! standing grants" and "The ledger". Scope: `docs/milestones/milestone-2.md`.
//! Glossary: authority, standing grant, standing rule, clearance, ledger.

mod authority;
mod chain;
mod error;
mod ledger;

pub use authority::{Authority, AuthorityStore, Change, Grant, Holder, Party, Rule, StandingId, Status};
pub use chain::Recorded;
pub use error::{Damage, Error};
pub use ledger::{Decision, Event, Keyed, Ledger, LedgerEntry, LedgerHistory, Outcome};
