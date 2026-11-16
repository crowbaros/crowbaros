//! What the broker asks to issue.

use crowbar_labels::LabelMark;
use serde::Serialize;

use crate::limits::EffectLimits;
use crate::principal::Principal;
use crate::resource::{Resource, Rights};

/// What the broker asks to issue: the gate decides it, and the handle table
/// issues only what the gate allowed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Issue {
    /// The one principal that may use the handle.
    pub holder: Principal,
    pub resource: Resource,
    pub rights: Rights,
    pub limits: EffectLimits,
    /// The label and mark of what the handle names. Reading through the
    /// handle gives the reader both.
    pub label_mark: LabelMark,
    /// Whether the holder may pass a narrowed copy on.
    pub delegable: bool,
    /// Plain-language text from the manifest, for the person.
    pub reason: String,
}
