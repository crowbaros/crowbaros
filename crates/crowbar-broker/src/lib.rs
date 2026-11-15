//! The capability broker: handles, egress, and the Tool contract.
//!
//! **Skeleton: no code yet.** Milestone 2, part one.
//!
//! The broker creates every handle, and only for an action the gate has signed.
//! It makes every outbound connection itself, including model calls. A Tool or a
//! task starts with nothing and gets everything through the broker protocol.
//! Adapters, which speak one service and know its operations, are part of the
//! broker.
//!
//! Decisions: ADR 0004 and ADR 0006 in `docs/adr/`.
//! Design: `docs/architecture/capability-model.md`, "Handles", "Tools", and
//! "Egress and secrets". Scope: `docs/milestones/milestone-2.md`.
