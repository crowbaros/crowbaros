//! Marks persist in storage (ADR 0008): a stored object keeps the label and
//! mark of whatever wrote it, a later write can only add to them, and they
//! survive being saved and loaded.

use std::collections::BTreeSet;

use crowbar_labels::{Category, Compartment, Fetch, Label, LabelStore, MarkSource, ObjectId, Origin};

mod common;
use common::{bank, me, news, private, sources};

#[test]
fn a_write_joins_with_what_is_already_stored() {
    // The first writer creates the object; every later one writes to it.
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
        let object = ObjectId::new("notes/today.md");
        let (first, later) = writers.split_first().unwrap();
        store.create(&object, first).unwrap();
        for writer in later {
            store.write(&object, writer);
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
fn a_write_to_an_object_with_no_record_joins_with_the_unknown_row() {
    // An object with no record may be a file that was on disk before any
    // record was kept. A write to it cannot lower it below the unknown row.
    let unknown = Origin::Unknown.label_mark();
    let typed = Origin::Typed.label_mark();
    let page = Origin::WebPage(Fetch::WithoutLogin(news())).label_mark();

    // (writer, stored label, stored mark sources)
    let cases = [
        (&typed, private(&[Category::Unknown]), BTreeSet::from([MarkSource::Unknown])),
        (
            &page,
            private(&[Category::Unknown]),
            BTreeSet::from([MarkSource::Unknown, MarkSource::Site(news())]),
        ),
    ];
    for (writer, label, marks) in cases {
        let mut store = LabelStore::default();
        let object = ObjectId::new("old/notes.txt");
        store.write(&object, writer);
        let stored = store.origin_of(&object).label_mark();
        assert_eq!(stored.label(), &label, "label after a write by {writer:?}");
        assert_eq!(sources(&stored), marks, "mark after a write by {writer:?}");
        assert!(unknown.is_at_most(&stored), "a write lowered an object with no record");
    }
}

#[test]
fn a_new_object_starts_at_its_writers_label_and_mark() {
    let mut store = LabelStore::default();
    let object = ObjectId::new("notes/new.md");
    store.create(&object, &Origin::Typed.label_mark()).unwrap();
    let stored = store.origin_of(&object).label_mark();
    assert_eq!(stored.label(), &Label::Personal);
    assert!(!stored.mark().is_marked());
}

#[test]
fn creating_an_object_that_already_has_a_record_is_refused() {
    let page = Origin::WebPage(Fetch::WithoutLogin(news())).label_mark();
    let mut store = LabelStore::default();
    let object = ObjectId::new("notes/today.md");
    store.create(&object, &page).unwrap();

    let refused = store.create(&object, &Origin::Typed.label_mark()).unwrap_err();
    assert_eq!(refused.object(), &object);
    assert_eq!(store.origin_of(&object).label_mark(), page, "a refused create changed the record");
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
        store.create(&ObjectId::new(object), label_mark).unwrap();
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
fn a_malformed_record_is_refused_and_never_read_as_public_or_unmarked() {
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
    store.create(&ObjectId::new("memory/findings"), findings.label_mark()).unwrap();
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
