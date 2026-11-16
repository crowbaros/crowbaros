//! The capability broker: handles, egress, and the Tool contract.
//!
//! The broker creates every handle, and only for an action the gate has allowed.
//! It makes every outbound connection itself, including model calls. A Tool or a
//! task starts with nothing and gets everything through the broker protocol.
//! Adapters, which speak one service and know its operations, are part of the
//! broker.
//!
//! Built so far: **handles** and **effect limits**.
//!
//! - A [`Handle`] is the right of one [`Principal`] to use one [`Resource`],
//!   with its [`Rights`], its [`EffectLimits`], and the label and mark of what
//!   it names. Holding the `Handle` is possession. Its [`HandleId`] is what
//!   the ledger and the Agent see, and it confers nothing.
//! - The [`HandleTable`] is the source of truth. It issues, lets a holder
//!   exercise, states what an id names, revokes, ends a holder's handles, and
//!   passes on narrowed copies. Its rules are written out on the type. It
//!   issues and passes on only with an `Allowed` from the gate
//!   (`crowbar-gate`) that covers the operation, and it tells the gate what it
//!   knows about a handle a caller presents ([`HandleTable::state`]). Each of
//!   its refusals is a reason on the gate's closed list.
//! - [`EffectLimits`] say which objects, how many, for how long, at what cost,
//!   and how often. They narrow and never widen.
//!
//! The shared types, from [`Principal`] to [`EffectLimits`], live in
//! `crowbar-capability` and are re-exported here.
//!
//! Not here yet:
//!
//! - the broker's own loop: asking the gate for each operation and carrying
//!   out what it allows, with an entry for each effect, copy, and
//!   revocation;
//! - handles across a process boundary, and the broker's record of which
//!   compartment each connection belongs to.
//!
//! Decisions: ADR 0004, ADR 0006, ADR 0008, ADR 0011, and ADR 0021 in
//! `docs/adr/`.
//! Design: `docs/architecture/capability-model.md`, "Handles", "Tools", and
//! "Egress and secrets". Scope: `docs/milestones/milestone-2.md`. Glossary:
//! handle, effect limits, principal.

mod handle;

pub use crowbar_capability::{
    Cost, EffectLimits, HandleId, Issue, NotAnId, Principal, Rate, Resource, ResourceKind, Right, Rights, TaskId, Time,
    Widening,
};
pub use handle::{Delegation, Exercised, Handle, HandleTable, Refusal, View};
