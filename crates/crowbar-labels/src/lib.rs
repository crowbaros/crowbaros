//! Labels, marks, and the origin table.
//!
//! **Skeleton: no code yet.** This is the first deliverable of Milestone 2.
//!
//! - A **label** says how private a piece of data is: public, personal, or
//!   private with a category. It comes from where the data came from.
//! - A **mark** records that a value was produced by a context that had read
//!   open untrusted input. Nothing a model does removes it.
//! - The **origin table** is the set of rules that gives every object its label
//!   and mark from its origin. It is deterministic, and no model takes part.
//!
//! Decisions: ADR 0003 and ADR 0008 in `docs/adr/`.
//! Design: `docs/architecture/capability-model.md`, "Data, labels, and
//! compartments". Scope: `docs/milestones/milestone-2.md`.
