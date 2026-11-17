//! What the person does by hand is the answer (ADR 0025). A value they move
//! into a compartment is released: the compartment's label stays as it was,
//! the compartment takes the value's mark, and nothing is refused. The first
//! four tests are the four cases of ADR 0025's table.

use std::collections::BTreeSet;

use crowbar_labels::{Category, Compartment, Fetch, FileOrigin, Label, LabelMark, MarkSource, Origin};

mod common;
use common::{bank, me, news, private, sources};

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

#[test]
fn a_one_time_code_pasted_from_mail_into_the_browser_does_not_raise_the_browsers_label() {
    // The browser's view holds pages from the open web.
    let page = Origin::WebPage(Fetch::WithoutLogin(news())).label_mark();
    let mut browser = Compartment::fixed([&page]);
    let code = Origin::Mail { account: me() }.label_mark();
    assert_ne!(code.label(), &Label::Public);

    browser.moved_in_by_hand(&code);

    // The browser is still public, so its next request to any other site is
    // not refused for what it holds.
    assert_eq!(browser.label_mark().label(), &Label::Public);
    // A stranger wrote the mail, and moving it does not make it the
    // person's, so the mark is kept.
    assert_eq!(
        sources(browser.label_mark()),
        BTreeSet::from([MarkSource::Site(news()), MarkSource::Mail(me())])
    );
}

#[test]
fn a_stack_trace_pasted_from_a_web_page_into_a_workbench_changes_nothing() {
    // ADR 0017: a workbench reads the open web all the time, so it already
    // carries the mark. Its standing rules were written for that mark, and
    // they still cover its acts after the paste.
    let source = Origin::Typed.label_mark();
    let mut workbench = Compartment::workbench([&source]);
    let before = workbench.label_mark().clone();
    let trace = Origin::WebPage(Fetch::WithoutLogin(news())).label_mark();

    workbench.moved_in_by_hand(&trace);

    assert_eq!(workbench.label_mark(), &before);
    assert_eq!(workbench.emit("a commit").label_mark(), &before);
}

#[test]
fn photos_imported_from_a_camera_card_are_personal_and_marked_as_from_outside() {
    let photos = Origin::File(FileOrigin::BroughtInByHand).label_mark();

    // The person chose them, so the label is theirs to decide.
    assert_eq!(photos.label(), &Label::Personal);
    // A stranger may have written them, so the mark is kept.
    assert_eq!(sources(&photos), BTreeSet::from([MarkSource::Outside]));

    // A file from outside with no act of the person's behind it still fails
    // closed.
    let dropped_off = Origin::File(FileOrigin::FromOutside).label_mark();
    assert_eq!(dropped_off.label(), &private(&[Category::Unknown]));
    assert_eq!(sources(&dropped_off), BTreeSet::from([MarkSource::Outside]));
}

#[test]
fn a_conversation_the_person_pastes_into_stays_at_its_label_and_takes_the_values_mark() {
    // ADR 0008: a value with no mark, such as the person's own note, leaves
    // the conversation clean.
    let mut conversation = Compartment::fresh();
    conversation.moved_in_by_hand(&Origin::Typed.label_mark());
    assert_eq!(conversation.label_mark().label(), &Label::Public);
    assert!(!conversation.label_mark().mark().is_marked());

    // A page of the bank's, under a login, is private to that site. Pasted
    // in, it marks the conversation and does not raise it.
    conversation.moved_in_by_hand(&Origin::WebPage(Fetch::UnderLogin(bank())).label_mark());
    assert_eq!(conversation.label_mark().label(), &Label::Public);
    assert_eq!(
        sources(conversation.label_mark()),
        BTreeSet::from([MarkSource::Site(bank())])
    );

    // What it reads any other way still raises it.
    conversation
        .read(&Origin::Mail { account: me() }.label_mark())
        .expect("a conversation's compartment grows");
    assert_eq!(conversation.label_mark().label(), &private(&[Category::Messages]));
}

/// One of each kind of compartment, each holding something already.
fn compartments() -> Vec<(&'static str, Compartment)> {
    let note = Origin::Typed.label_mark();
    let download = Origin::File(FileOrigin::Downloaded(Fetch::WithoutLogin(news()))).label_mark();
    let mut task = Compartment::fresh();
    task.read(&note).expect("a task's compartment grows");
    task.read(&download).expect("a task's compartment grows");
    vec![
        ("a new start", Compartment::fresh()),
        ("a task that has read a note and a download", task),
        ("installed software", Compartment::fixed([&note, &download])),
        ("a workbench", Compartment::workbench([&note])),
    ]
}

/// A value from every row of the origin table that gives a label and mark
/// of its own.
fn values() -> Vec<LabelMark> {
    [
        Origin::Typed,
        Origin::Template,
        Origin::Mail { account: me() },
        Origin::WebPage(Fetch::WithoutLogin(news())),
        Origin::WebPage(Fetch::UnderLogin(bank())),
        Origin::File(FileOrigin::FromOutside),
        Origin::File(FileOrigin::BroughtInByHand),
        Origin::Pasted { copied_from: None },
        Origin::Unknown,
    ]
    .iter()
    .map(Origin::label_mark)
    .collect()
}

#[test]
fn a_move_by_hand_is_never_refused_never_changes_a_label_and_never_removes_a_mark() {
    for value in values() {
        for (name, mut compartment) in compartments() {
            let before = compartment.label_mark().clone();

            compartment.moved_in_by_hand(&value);

            let after = compartment.label_mark();
            assert_eq!(after.label(), before.label(), "label of {name} after {value:?}");
            assert!(
                before.mark().is_at_most(after.mark()),
                "{name} lost a mark after {value:?}"
            );
            assert_eq!(
                compartment.emit("a file").label_mark(),
                after,
                "what {name} emits after {value:?}"
            );
        }
    }
}

#[test]
fn every_compartment_but_a_workbench_carries_the_mark_of_what_was_moved_into_it() {
    for value in values() {
        for (name, mut compartment) in compartments() {
            let is_workbench = sources(compartment.label_mark()).contains(&MarkSource::Workbench);
            compartment.moved_in_by_hand(&value);
            if !is_workbench {
                assert!(
                    value.mark().is_at_most(compartment.label_mark().mark()),
                    "{name} does not carry the mark of {value:?}"
                );
            }
        }
    }
}
