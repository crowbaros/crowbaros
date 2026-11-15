//! Helpers the test files share.

// Each test file is its own crate and uses only some of these.
#![allow(dead_code)]

use std::collections::BTreeSet;

use crowbar_labels::{Categories, Category, Label, LabelMark, MailAccount, MarkSource, Site};

/// A site the person is not logged in to.
pub fn news() -> Site {
    Site::new("news.example")
}

/// A site the person is logged in to.
pub fn bank() -> Site {
    Site::new("bank.example")
}

/// One of the person's mail accounts.
pub fn me() -> MailAccount {
    MailAccount::new("me@mail.example")
}

/// A private label with exactly these categories. It is built from the set
/// itself, not with `Label::join`, so tests of the join do not check the join
/// against itself.
pub fn private(categories: &[Category]) -> Label {
    let set: BTreeSet<Category> = categories.iter().cloned().collect();
    Label::Private(Categories::try_from(set).expect("a private label needs a category"))
}

/// The sources of a mark, as a set.
pub fn sources(label_mark: &LabelMark) -> BTreeSet<MarkSource> {
    label_mark.mark().sources().cloned().collect()
}
