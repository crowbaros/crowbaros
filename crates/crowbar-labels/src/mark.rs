//! Marks, and the label and mark a value carries together.

use std::collections::BTreeSet;
use std::fmt;

use crate::label::{Label, Site, write_list};

/// One of the person's mail accounts, as the broker's mail adapter names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MailAccount(String);

impl MailAccount {
    pub fn new(account: &str) -> Self {
        MailAccount(account.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Where open untrusted input that marked a value came from.
///
/// ADR 0011 lets a marked task act under a standing rule the person wrote for
/// that source, so a mark keeps its sources and the gate can check them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MarkSource {
    /// A web site the broker fetched from.
    Site(Site),
    /// Mail that arrived in one of the person's accounts.
    Mail(MailAccount),
    /// Something the person pasted or dropped.
    Paste,
    /// A file that came from outside the fleet some other way, such as
    /// removable media.
    Outside,
    /// An origin the table does not know.
    Unknown,
    /// A workbench, which reads the open web all the time (ADR 0017).
    Workbench,
}

/// The record that a value was produced by a context that had read open
/// untrusted input, with the sources of that input.
///
/// A value is marked when it has at least one source. Nothing here removes a
/// source: a join is the union, and no method takes one away.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Mark(BTreeSet<MarkSource>);

impl Mark {
    pub(crate) fn none() -> Self {
        Mark(BTreeSet::new())
    }

    pub(crate) fn from_source(source: MarkSource) -> Self {
        Mark(BTreeSet::from([source]))
    }

    pub fn is_marked(&self) -> bool {
        !self.0.is_empty()
    }

    pub fn sources(&self) -> impl Iterator<Item = &MarkSource> {
        self.0.iter()
    }

    pub fn join(&self, other: &Mark) -> Mark {
        Mark(self.0.union(&other.0).cloned().collect())
    }

    /// Whether every source of this mark is a source of `other`.
    pub fn is_at_most(&self, other: &Mark) -> bool {
        self.0.is_subset(&other.0)
    }
}

/// The label and mark a value carries.
///
/// The only ways to get one are the origin table ([`crate::Origin::label_mark`])
/// and joins of what it gave. Its fields are private, so a value's label and
/// mark can be raised and never lowered.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LabelMark {
    label: Label,
    mark: Mark,
}

impl LabelMark {
    pub(crate) fn new(label: Label, mark: Mark) -> Self {
        LabelMark { label, mark }
    }

    /// What a compartment holds before it has read anything: public, and
    /// unmarked.
    pub(crate) fn clean() -> Self {
        LabelMark::new(Label::Public, Mark::none())
    }

    pub fn label(&self) -> &Label {
        &self.label
    }

    pub fn mark(&self) -> &Mark {
        &self.mark
    }

    /// The label and mark of something that carries both: the join of the
    /// labels and the union of the marks.
    pub fn join(&self, other: &LabelMark) -> LabelMark {
        LabelMark::new(self.label.join(&other.label), self.mark.join(&other.mark))
    }

    /// Whether `other` is at least as strict in label and carries every
    /// source of this mark, so that joining this into it changes nothing.
    pub fn is_at_most(&self, other: &LabelMark) -> bool {
        self.label.is_at_most(&other.label) && self.mark.is_at_most(&other.mark)
    }
}

impl fmt::Display for MarkSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MarkSource::Site(site) => write!(f, "site {}", site.host()),
            MarkSource::Mail(account) => write!(f, "mail {}", account.as_str()),
            MarkSource::Paste => f.write_str("paste"),
            MarkSource::Outside => f.write_str("outside"),
            MarkSource::Unknown => f.write_str("unknown"),
            MarkSource::Workbench => f.write_str("workbench"),
        }
    }
}

/// The wording of the origin table: `none`, or `marked:` followed by the
/// sources.
impl fmt::Display for Mark {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_marked() {
            return f.write_str("none");
        }
        f.write_str("marked: ")?;
        write_list(f, self.sources())
    }
}
