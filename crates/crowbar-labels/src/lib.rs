//! Labels, marks, and the origin table.
//!
//! - A **label** says how private a piece of data is: public, personal, or
//!   private with a category. It comes from where the data came from.
//! - A **mark** records that a value was produced by a context that had read
//!   open untrusted input. Nothing a model does removes it.
//! - The **origin table** is the set of rules that gives every object its label
//!   and mark from its origin. It is deterministic, and no model takes part.
//!
//! Decisions: ADR 0003, ADR 0008, and ADR 0011 in `docs/adr/`.
//! Design: `docs/architecture/capability-model.md`, "Data, labels, and
//! compartments". Scope: `docs/milestones/milestone-2.md`.

mod label;
mod labeled;
mod mark;
mod origin;

pub use label::{Categories, Category, Label, Site};
pub use labeled::Labeled;
pub use mark::{LabelMark, MailAccount, Mark, MarkSource};
pub use origin::{AdmittedSet, FileOrigin, Origin};
