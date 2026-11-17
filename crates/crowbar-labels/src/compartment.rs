//! Compartments: the label and mark of one execution context.

use std::fmt;

use crate::label::Label;
use crate::labeled::Labeled;
use crate::mark::{LabelMark, Mark, MarkSource};

/// The label and mark of one compartment: one execution context, from one
/// start (ADR 0016).
///
/// Once a compartment reads a value it is at least at that value's label and
/// carries its mark, for as long as it lives, and so is everything it emits
/// (ADR 0008). The one exception is a value the person moved in by hand: it
/// is released, so the compartment takes its mark and not its label
/// (ADR 0025). Nothing here lowers a label or removes a mark. A compartment
/// ends when it is dropped; a new start is a new compartment.
///
/// It cannot be copied. A copy taken before a read would keep emitting at the
/// lower label, from the same start:
///
/// ```compile_fail
/// use crowbar_labels::Compartment;
/// let before = Compartment::fresh();
/// let copy = before.clone();
/// ```
#[derive(Debug, PartialEq, Eq)]
pub struct Compartment {
    label_mark: LabelMark,
    growth: Growth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Growth {
    /// A task's model context and workspace: the broker and the plan
    /// interpreter see each read, so the compartment rises with each one.
    Grows,
    /// A native sandbox: the gate cannot see a read inside its view, so its
    /// label and mark are fixed at start from the whole view.
    Fixed,
    /// A workbench: a native sandbox that is always marked, because it
    /// reads the open web all the time (ADR 0017).
    Workbench,
}

impl Compartment {
    /// A new start that has read nothing: public, and unmarked.
    pub fn fresh() -> Self {
        Compartment {
            label_mark: LabelMark::public_unmarked(),
            growth: Growth::Grows,
        }
    }

    /// A new start made by `parent`, such as a task the Agent starts. What
    /// starts it is something the parent emits, so it runs at the parent's
    /// label and mark.
    pub fn started_by(parent: &Compartment) -> Self {
        Compartment {
            label_mark: parent.label_mark.clone(),
            growth: Growth::Grows,
        }
    }

    /// A native sandbox, at the join of everything in the view it is given,
    /// whether or not it opens any of it. Its label and mark never change.
    pub fn fixed<'a>(view: impl IntoIterator<Item = &'a LabelMark>) -> Self {
        let label_mark = view
            .into_iter()
            .fold(LabelMark::public_unmarked(), |held, item| held.join(item));
        Compartment {
            label_mark,
            growth: Growth::Fixed,
        }
    }

    /// A workbench: a native sandbox that is always marked, because it reads
    /// the open web all the time (ADR 0017).
    pub fn workbench<'a>(view: impl IntoIterator<Item = &'a LabelMark>) -> Self {
        let mut workbench = Compartment::fixed(view);
        let marked = LabelMark::new(Label::Public, Mark::from_source(MarkSource::Workbench));
        workbench.label_mark = workbench.label_mark.join(&marked);
        workbench.growth = Growth::Workbench;
        workbench
    }

    pub fn label_mark(&self) -> &LabelMark {
        &self.label_mark
    }

    /// Reads a value with this label and mark. A compartment that can grow
    /// rises to the join. A fixed one takes only what is already at or below
    /// its label and mark, and refuses the rest. A value the person moved in
    /// by hand does not come this way: see
    /// [`Compartment::moved_in_by_hand`].
    pub fn read(&mut self, value: &LabelMark) -> Result<(), ReadRefused> {
        match self.growth {
            Growth::Grows => {
                self.label_mark = self.label_mark.join(value);
                Ok(())
            }
            Growth::Fixed | Growth::Workbench if value.is_at_most(&self.label_mark) => Ok(()),
            Growth::Fixed | Growth::Workbench => Err(ReadRefused {
                held: self.label_mark.clone(),
                offered: value.clone(),
            }),
        }
    }

    /// Takes in a value the person moved into this compartment with their
    /// own hand: a paste, a drop, a pick, or a link they clicked (ADR 0025).
    /// This is only for that. The broker calls it when the compositor or
    /// the shell saw the person's act, and never on the word of the
    /// compartment or of a model. Every other value goes through
    /// [`Compartment::read`].
    ///
    /// The move is a release, so the compartment's label stays as it was,
    /// whatever the value's label. The mark is kept: the compartment takes
    /// the value's mark, and everything it emits afterwards carries it.
    /// That holds for a compartment whose label and mark were fixed at
    /// start too. Nothing is refused.
    ///
    /// A workbench already carries the mark, so nothing about it changes
    /// (ADR 0017), and the standing rules written for its mark still cover
    /// its acts.
    pub fn moved_in_by_hand(&mut self, value: &LabelMark) {
        if self.growth == Growth::Workbench {
            return;
        }
        let mark = self.label_mark.mark().join(value.mark());
        self.label_mark = LabelMark::new(self.label_mark.label().clone(), mark);
    }

    /// Something this compartment emits: a file, a message, a log line, or a
    /// model request. It carries the compartment's label and mark.
    pub fn emit<T>(&self, value: T) -> Labeled<T> {
        Labeled::with(value, self.label_mark.clone())
    }
}

/// A fixed compartment was offered a value above the label and mark it started
/// at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRefused {
    held: LabelMark,
    offered: LabelMark,
}

impl ReadRefused {
    /// The compartment's fixed label and mark.
    pub fn held(&self) -> &LabelMark {
        &self.held
    }

    /// The label and mark of the value it was offered.
    pub fn offered(&self) -> &LabelMark {
        &self.offered
    }
}

impl fmt::Display for ReadRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "a native compartment's label and mark are fixed when it starts: it holds {}, {}, and was offered {}, {}",
            self.held.label(),
            self.held.mark(),
            self.offered.label(),
            self.offered.mark()
        )
    }
}

impl std::error::Error for ReadRefused {}
