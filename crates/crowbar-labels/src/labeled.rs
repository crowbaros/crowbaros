//! A value together with its label and mark.

use crate::label::Label;
use crate::mark::{LabelMark, Mark};
use crate::origin::Origin;

/// A value and the label and mark it carries.
///
/// A value enters with the label and mark its origin gives. Whatever is
/// computed from it keeps them: [`Labeled::map`] for a result of one value,
/// such as a summary, a size, or a bounded result, and [`Labeled::combine`] for
/// a result of two, which carries the join of both. No method lowers either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Labeled<T> {
    value: T,
    label_mark: LabelMark,
}

impl<T> Labeled<T> {
    /// A value that came from `origin`, with the label and mark the origin
    /// table gives.
    pub fn from_origin(value: T, origin: &Origin) -> Self {
        Labeled { value, label_mark: origin.label_mark() }
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn label_mark(&self) -> &LabelMark {
        &self.label_mark
    }

    /// A result computed from this value. It carries this value's label and
    /// mark, whatever `f` returns.
    pub fn map<U>(&self, f: impl FnOnce(&T) -> U) -> Labeled<U> {
        Labeled { value: f(&self.value), label_mark: self.label_mark.clone() }
    }

    /// A result computed from this value and `other`. It carries the join of
    /// both labels and marks.
    pub fn combine<U, V>(&self, other: &Labeled<U>, f: impl FnOnce(&T, &U) -> V) -> Labeled<V> {
        Labeled { value: f(&self.value, &other.value), label_mark: self.label_mark.join(&other.label_mark) }
    }

    /// The same value at a label at least as strict as `label`. The Agent may
    /// make a label stricter without asking (ADR 0008); lowering one is a
    /// declassify, which only the person can do, and is not here.
    pub fn raise(self, label: &Label) -> Self {
        let raised = LabelMark::new(label.clone(), Mark::none());
        Labeled { value: self.value, label_mark: self.label_mark.join(&raised) }
    }
}
