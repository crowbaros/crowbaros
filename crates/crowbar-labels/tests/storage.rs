//! Marks persist in storage (ADR 0008): a stored object keeps the label and
//! mark of whatever wrote it, a later write can only add to them, and they
//! survive being saved and loaded.

use std::collections::BTreeSet;

use crowbar_labels::{Category, Compartment, Fetch, Label, LabelStore, MarkSource, ObjectId, Origin};

mod common;
use common::{bank, me, news, sources};

#[test]
fn a_write_joins_with_what_is_already_stored() {
    let typed = Origin::Typed.label_mark();
    let page = Origin::WebPage(Fetch::WithoutLogin(news())).label_mark();
    let mail = Origin::Mail { account: me() }.label_mark();
    let template = Origin::Template.label_mark();
    let finance = Compartment::fixed([&typed])
        .emit(())
        .raise(&Label::private(Category::Finance));
    let finance = finance.label_mark();

    // (name, writers in order, stored label, stored mark sources)
    let cases = [
        ("one unmarked write", vec![&typed], Label::Personal, BTreeSet::new()),
        (
            "a marked write",
            vec![&page],
            Label::Public,
            BTreeSet::from([MarkSource::Site(news())]),
        ),
        (
            "an unmarked writer overwrites a marked object",
            vec![&page, &typed],
            Label::Personal,
            BTreeSet::from([MarkSource::Site(news())]),
        ),
        (
            "a public writer overwrites a private object",
            vec![&mail, &template],
            Label::private(Category::Messages),
            BTreeSet::from([MarkSource::Mail(me())]),
        ),
        (
            "two private categories",
            vec![&mail, finance],
            Label::private(Category::Messages).join(&Label::private(Category::Finance)),
            BTreeSet::from([MarkSource::Mail(me())]),
        ),
    ];
    for (name, writers, label, marks) in cases {
        let mut store = LabelStore::default();
        for writer in writers {
            store.write(&ObjectId::new("notes/today.md"), writer);
        }
        let stored = store.origin_of(&ObjectId::new("notes/today.md")).label_mark();
        assert_eq!(stored.label(), &label, "label after {name}");
        assert_eq!(sources(&stored), marks, "mark after {name}");
    }
}

#[test]
fn an_object_with_no_record_falls_to_the_unknown_row() {
    let store = LabelStore::default();
    assert_eq!(store.origin_of(&ObjectId::new("never/written")), Origin::Unknown);
}

#[test]
fn labels_and_marks_survive_saving_and_loading() {
    let mut store = LabelStore::default();
    let records = [
        ("typed", Origin::Typed.label_mark()),
        ("page", Origin::WebPage(Fetch::WithoutLogin(news())).label_mark()),
        ("bank", Origin::WebPage(Fetch::UnderLogin(bank())).label_mark()),
        ("mail", Origin::Mail { account: me() }.label_mark()),
        ("paste", Origin::Pasted { copied_from: None }.label_mark()),
        ("unknown", Origin::Unknown.label_mark()),
        ("workbench", Compartment::workbench([]).label_mark().clone()),
        (
            "mixed",
            Origin::Mail { account: me() }
                .label_mark()
                .join(&Origin::Unknown.label_mark()),
        ),
    ];
    for (object, label_mark) in &records {
        store.write(&ObjectId::new(object), label_mark);
    }

    let saved = serde_json::to_string(&store).unwrap();
    let loaded: LabelStore = serde_json::from_str(&saved).unwrap();
    assert_eq!(loaded, store);
    for (object, label_mark) in &records {
        assert_eq!(
            &loaded.origin_of(&ObjectId::new(object)).label_mark(),
            label_mark,
            "{object} after loading"
        );
    }
}

#[test]
fn a_record_that_would_load_lower_than_it_was_written_is_refused() {
    // A record must say both its label and its mark. A missing field is never
    // read as public or as unmarked.
    let cases = [
        ("no mark", r#"{"a": {"label": "public"}}"#),
        ("no label", r#"{"a": {"mark": []}}"#),
        (
            "private with no category",
            r#"{"a": {"label": {"private": []}, "mark": []}}"#,
        ),
        (
            "a category it does not know",
            r#"{"a": {"label": {"private": ["secret"]}, "mark": []}}"#,
        ),
        (
            "a mark source it does not know",
            r#"{"a": {"label": "public", "mark": ["trusted"]}}"#,
        ),
        (
            "a field it does not know",
            r#"{"a": {"label": "public", "mark": [], "cleaned": true}}"#,
        ),
        ("a label it does not know", r#"{"a": {"label": "cleared", "mark": []}}"#),
        (
            "the same object twice, the second unmarked",
            r#"{"a": {"label": "public", "mark": ["unknown"]}, "a": {"label": "public", "mark": []}}"#,
        ),
    ];
    for (name, json) in cases {
        assert!(serde_json::from_str::<LabelStore>(json).is_err(), "{name} was accepted");
    }
    // The written form, for reference: it loads.
    let ok = r#"{"a": {
        "label": {"private": ["messages", {"site": "bank.example"}]},
        "mark": [{"mail": "me@mail.example"}, "paste"]
    }}"#;
    let loaded: LabelStore = serde_json::from_str(ok).unwrap();
    assert!(loaded.origin_of(&ObjectId::new("a")).label_mark().mark().is_marked());
}

#[test]
fn findings_a_marked_task_stored_mark_a_later_task_that_reads_them() {
    // Attack A11: a research task's findings are stored; a later task reads them.
    let mut research = Compartment::fresh();
    research.read(&Origin::Typed.label_mark()).unwrap();
    research
        .read(&Origin::WebPage(Fetch::WithoutLogin(news())).label_mark())
        .unwrap();
    let findings = research.emit("findings");

    let mut store = LabelStore::default();
    store.write(&ObjectId::new("memory/findings"), findings.label_mark());
    let saved = serde_json::to_string(&store).unwrap();
    drop(research);

    let store: LabelStore = serde_json::from_str(&saved).unwrap();
    let mut later = Compartment::fresh();
    later.read(&Origin::Typed.label_mark()).unwrap();
    later
        .read(&store.origin_of(&ObjectId::new("memory/findings")).label_mark())
        .unwrap();
    assert_eq!(sources(later.label_mark()), BTreeSet::from([MarkSource::Site(news())]));
}
