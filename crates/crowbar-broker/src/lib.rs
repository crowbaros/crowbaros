//! The capability broker: handles, egress, and the Tool contract.
//!
//! The broker creates every handle, and only for an action the gate has signed.
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
//!   passes on narrowed copies. Its rules are written out on the type.
//! - [`EffectLimits`] say which objects, how many, for how long, at what cost,
//!   and how often. They narrow and never widen.
//!
//! Not here yet:
//!
//! - the gate: checking its signature at issuance, and checking each
//!   operation against the limits, including a copy's uses against its
//!   original's;
//! - a ledger entry for each issue, copy, and revocation;
//! - handles across a process boundary, and the broker's record of which
//!   compartment each connection belongs to.
//!
//! Decisions: ADR 0004, ADR 0006, ADR 0008, and ADR 0011 in `docs/adr/`.
//! Design: `docs/architecture/capability-model.md`, "Handles", "Tools", and
//! "Egress and secrets". Scope: `docs/milestones/milestone-2.md`. Glossary:
//! handle, effect limits, principal.

mod handle;

pub use crowbar_capability::{
    Cost, EffectLimits, HandleId, Issue, NotAnId, Principal, Rate, Resource, ResourceKind, Right, Rights, TaskId, Time,
    Widening,
};
pub use handle::{Delegation, Exercised, Handle, HandleTable, Refusal, View};
