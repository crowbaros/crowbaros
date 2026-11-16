//! The terms the broker, the gate, and the ledger share.
//!
//! - [`Principal`]: anything that can hold authority.
//! - [`Resource`], [`Right`], and [`Rights`]: what a handle names, and what it
//!   allows on it.
//! - [`EffectLimits`]: which objects, how many, for how long, at what cost,
//!   and how often. They narrow and never widen.
//! - [`HandleId`]: the id of a handle as the ledger and the Agent see it. It
//!   confers nothing.
//! - [`Issue`]: what the broker asks to issue.
//! - The gate's reasons: a [`Refuse`] names a [`GateRule`] and the [`Limit`]
//!   that applied, and an [`Ask`] says why it asks. The list is closed.
//! - [`IdempotencyKey`]: what a task attaches so an operation is decided and
//!   carried out once.
//!
//! These live in a crate of their own so that the broker can depend on the
//! gate, the gate on authority, and authority on none of them.
//! `crowbar-broker` re-exports the ones it used to define.
//!
//! Decisions: ADR 0004, ADR 0011, and ADR 0021 in `docs/adr/`. Glossary:
//! principal, handle, effect limits, idempotency key.

mod handle_id;
mod issue;
mod key;
mod limits;
mod principal;
mod reason;
mod resource;

pub use handle_id::{HandleId, NotAnId};
pub use issue::Issue;
pub use key::IdempotencyKey;
pub use limits::{Cost, EffectLimits, Rate, Time, Widening};
pub use principal::{Principal, TaskId};
pub use reason::{Ask, AskWhy, GateRule, Limit, Refuse};
pub use resource::{Resource, ResourceKind, Right, Rights};
