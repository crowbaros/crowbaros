//! What the person does by hand is the answer (ADR 0025). A value they move
//! into a compartment is released: the compartment's label stays as it was,
//! the compartment takes the value's mark, and nothing is refused. The first
//! four tests are the four cases of ADR 0025's table.

use std::collections::BTreeSet;

use crowbar_labels::{Compartment, Fetch, Label, MarkSource, Origin};

mod common;
use common::{me, news, sources};

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
