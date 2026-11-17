//! What the person does by hand is the answer (ADR 0025). A value they move
//! into a compartment is released: the compartment's label stays as it was,
//! the compartment takes the value's mark, and nothing is refused. The first
//! four tests are the four cases of ADR 0025's table.

use std::collections::BTreeSet;

use crowbar_labels::{Compartment, Fetch, Label, MarkSource, Origin};

mod common;
use common::{news, sources};

#[test]
fn text_pasted_from_a_web_page_into_notes_lands_and_what_the_notes_app_saves_carries_the_mark() {
    // The notes app is installed software. Its view holds the person's
    // notes, so it started personal and unmarked, and that is fixed.
    let own_notes = Origin::Typed.label_mark();
    let mut notes = Compartment::fixed([&own_notes]);
    let page = Origin::WebPage(Fetch::WithoutLogin(news())).label_mark();

    // Offered any other way, the page's text is refused.
    assert!(notes.read(&page).is_err());

    notes.moved_in_by_hand(&page);

    assert_eq!(notes.label_mark().label(), &Label::Personal);
    assert_eq!(sources(notes.label_mark()), BTreeSet::from([MarkSource::Site(news())]));
    // What the notes app saves afterwards carries the mark, so a task that
    // later reads the note is marked.
    let saved = notes.emit("notes from the article");
    assert_eq!(sources(saved.label_mark()), BTreeSet::from([MarkSource::Site(news())]));
    let mut task = Compartment::fresh();
    task.read(saved.label_mark()).expect("a task's compartment grows");
    assert!(task.label_mark().mark().is_marked());
}
