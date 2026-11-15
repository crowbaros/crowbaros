//! Labels, marks, and the origin table: the first deliverable of Milestone 2.
//!
//! - A **label** says how private a piece of data is: public, personal, or
//!   private with one category or more. It comes from where the data came from.
//! - A **mark** records that a value was produced by a context that had read
//!   open untrusted input, and from which sources. Nothing a model does
//!   removes it.
//! - The **origin table** gives every object its label and mark from its
//!   origin. It is deterministic, and no model takes part. The table is
//!   written out in the docs of [`Origin`], and a test keeps it the same as the
//!   code.
//!
//! Labels and marks only ever rise. They spread through:
//!
//! - **values**: [`Labeled`] keeps a value's label and mark through every
//!   result computed from it, and joins them when results combine;
//! - **compartments**: a [`Compartment`] is at the join of everything it has
//!   read, for as long as it lives, and so is everything it emits. A native
//!   sandbox is fixed at the join of its view when it starts, and a workbench
//!   is always marked;
//! - **storage**: a [`LabelStore`] write joins with what is stored, and the
//!   records keep every label and mark when they are saved and loaded.
//!
//! Nothing here lowers a label or removes a mark. Declassifying, which only
//! the person can do, and clearances, which the gate checks, are not in this
//! crate.
//!
//! Decisions: ADR 0003, ADR 0008, ADR 0011, ADR 0016, ADR 0017, and ADR 0019
//! in `docs/adr/`. Design: `docs/architecture/capability-model.md`, "Data,
//! labels, and compartments". Scope: `docs/milestones/milestone-2.md`.

mod compartment;
mod label;
mod labeled;
mod mark;
mod origin;
mod store;

pub use compartment::{Compartment, ReadRefused};
pub use label::{Categories, Category, Label, Site};
pub use labeled::Labeled;
pub use mark::{LabelMark, MailAccount, Mark, MarkSource};
pub use origin::{AdmittedSet, Fetch, FileOrigin, Origin};
pub use store::{AlreadyRecorded, LabelStore, ObjectId};
