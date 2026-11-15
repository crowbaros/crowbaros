//! Labels: how private a piece of data is.

use std::collections::BTreeSet;

/// A web site, named by its host. Host names compare without regard to ASCII
/// case, as DNS does.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Site(String);

impl Site {
    pub fn new(host: &str) -> Self {
        Site(host.to_ascii_lowercase())
    }

    pub fn host(&self) -> &str {
        &self.0
    }
}

/// The category that private data carries.
///
/// Messages, health, and finance are the categories the vision names. A page
/// fetched under a login is private to that site (ADR 0008), so a site is a
/// category too. `Unclassified` is what the origin table gives data whose
/// origin it does not know; no party is cleared for it unless the person names
/// it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Messages,
    Health,
    Finance,
    Site(Site),
    Unclassified,
}

/// How private a piece of data is: public, personal, or private, in that
/// order. Private data carries one category or more.
///
/// Labels form a lattice. [`Label::join`] is the least label at or above both,
/// and [`Label::is_at_most`] is its order. Two private labels with different
/// categories are not ordered: neither is at most the other.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Label {
    Public,
    Personal,
    Private(Categories),
}

/// The categories of a private label. Never empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Categories(BTreeSet<Category>);

impl Categories {
    pub fn iter(&self) -> impl Iterator<Item = &Category> {
        self.0.iter()
    }

    pub fn contains(&self, category: &Category) -> bool {
        self.0.contains(category)
    }
}

impl Label {
    /// A private label with one category.
    pub fn private(category: Category) -> Self {
        Label::Private(Categories(BTreeSet::from([category])))
    }

    /// The least label at or above both: the stricter level, and the union of
    /// the categories.
    pub fn join(&self, other: &Label) -> Label {
        match (self, other) {
            (Label::Private(a), Label::Private(b)) => Label::Private(Categories(a.0.union(&b.0).cloned().collect())),
            (Label::Private(_), _) => self.clone(),
            (_, Label::Private(_)) => other.clone(),
            (Label::Personal, _) | (_, Label::Personal) => Label::Personal,
            (Label::Public, Label::Public) => Label::Public,
        }
    }

    /// Whether `other` is at least as strict as this label in every way, so
    /// that joining this label into it changes nothing.
    pub fn is_at_most(&self, other: &Label) -> bool {
        match (self, other) {
            (Label::Private(a), Label::Private(b)) => a.0.is_subset(&b.0),
            (Label::Private(_), _) => false,
            (Label::Personal, Label::Public) => false,
            _ => true,
        }
    }
}
