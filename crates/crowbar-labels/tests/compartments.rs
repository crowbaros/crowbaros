//! A compartment that reads a value is at least at that value's label and
//! carries its mark, for as long as it lives, and so is everything it emits
//! (ADR 0008). Each start is a new compartment (ADR 0016).

use std::collections::BTreeSet;

use crowbar_labels::{
    AdmittedSet, Category, Compartment, FileOrigin, Label, LabelMark, MailAccount, MarkSource, Origin, Site,
};

fn me() -> MailAccount {
    MailAccount::new("me@mail.example")
}

fn bank() -> Site {
    Site::new("bank.example")
}

fn news() -> Site {
    Site::new("news.example")
}

fn sources(label_mark: &LabelMark) -> BTreeSet<MarkSource> {
    label_mark.mark().sources().cloned().collect()
}

fn reading(origins: &[Origin]) -> Compartment {
    let mut compartment = Compartment::fresh();
    for origin in origins {
        compartment
            .read(&origin.label_mark())
            .expect("a compartment that can grow reads anything");
    }
    compartment
}

#[test]
fn a_compartment_is_at_the_join_of_everything_it_has_read() {
    let mail = Origin::Mail { account: me() };
    let bank_page = Origin::WebPage {
        site: bank(),
        login: true,
    };
    let news_page = Origin::WebPage {
        site: news(),
        login: false,
    };
    let package = Origin::Admitted(AdmittedSet::BuildDependencies);

    // (name, what it read in order, label, mark sources)
    let cases = [
        ("a new start", vec![], Label::Public, BTreeSet::new()),
        (
            "the person's words",
            vec![Origin::Typed],
            Label::Personal,
            BTreeSet::new(),
        ),
        ("admitted input", vec![package.clone()], Label::Public, BTreeSet::new()),
        (
            "a public page",
            vec![news_page.clone()],
            Label::Public,
            BTreeSet::from([MarkSource::Site(news())]),
        ),
        (
            "words, then a mail, then words",
            vec![Origin::Typed, mail.clone(), Origin::Typed],
            Label::private(Category::Messages),
            BTreeSet::from([MarkSource::Mail(me())]),
        ),
        (
            "a mail and a logged-in page",
            vec![mail.clone(), bank_page.clone()],
            Label::private(Category::Messages).join(&Label::private(Category::Site(bank()))),
            BTreeSet::from([MarkSource::Mail(me()), MarkSource::Site(bank())]),
        ),
        (
            "a marked page, then only clean input",
            vec![news_page, Origin::Typed, package.clone(), Origin::Template, package],
            Label::Personal,
            BTreeSet::from([MarkSource::Site(news())]),
        ),
    ];
    for (name, origins, label, marks) in cases {
        let compartment = reading(&origins);
        assert_eq!(compartment.label_mark().label(), &label, "label after {name}");
        assert_eq!(sources(compartment.label_mark()), marks, "mark after {name}");
    }
}

#[test]
fn everything_a_compartment_emits_carries_its_label_and_mark() {
    let compartment = reading(&[Origin::Typed, Origin::Mail { account: me() }]);
    for emitted in [
        compartment.emit("a log line"),
        compartment.emit(""),
        compartment.emit("model request"),
    ] {
        assert_eq!(emitted.label_mark(), compartment.label_mark(), "{:?}", emitted.value());
    }
}

#[test]
fn a_compartment_started_by_another_runs_at_its_label_and_mark() {
    let marked = reading(&[
        Origin::Typed,
        Origin::WebPage {
            site: news(),
            login: false,
        },
    ]);
    let clean = reading(&[Origin::Typed]);

    // ADR 0008: a standing task started from a marked context runs marked.
    let child = Compartment::started_by(&marked);
    assert_eq!(child.label_mark(), marked.label_mark());
    assert_eq!(Compartment::started_by(&clean).label_mark(), clean.label_mark());

    // A new start that nothing marked started, and that has read nothing,
    // starts clean. Ending the marked one does not change that.
    drop(marked);
    let next = Compartment::fresh();
    assert!(!next.label_mark().mark().is_marked());
    assert_eq!(next.label_mark().label(), &Label::Public);
}

#[test]
fn a_native_compartment_is_fixed_at_the_join_of_its_view() {
    let note = Origin::Typed.label_mark();
    let download = Origin::File(FileOrigin::Downloaded {
        site: news(),
        login: false,
    })
    .label_mark();
    let mail = Origin::Mail { account: me() }.label_mark();

    // A view that holds one file from outside, which it never opens (attack
    // A13): the compartment is marked from its start.
    let mut native = Compartment::fixed([&note, &download]);
    assert_eq!(native.label_mark().label(), &Label::Personal);
    assert_eq!(sources(native.label_mark()), BTreeSet::from([MarkSource::Site(news())]));
    assert!(native.emit("result").label_mark().mark().is_marked());

    // (offered later, accepted)
    let cases = [
        (note.clone(), true),
        (download.clone(), true),
        (Origin::Template.label_mark(), true),
        // Above its start label: refused, not raised.
        (mail.clone(), false),
        // Same label, a new mark source: refused too.
        (
            Origin::WebPage {
                site: bank(),
                login: false,
            }
            .label_mark(),
            false,
        ),
    ];
    let start = native.label_mark().clone();
    for (offered, accepted) in cases {
        let result = native.read(&offered);
        assert_eq!(result.is_ok(), accepted, "{offered:?}");
        if let Err(refusal) = result {
            assert_eq!(refusal.offered(), &offered);
            assert_eq!(refusal.held(), &start);
        }
        assert_eq!(native.label_mark(), &start, "a fixed compartment never changes");
    }
}

#[test]
fn a_workbench_is_always_marked() {
    // ADR 0017: a workbench reads the open web all the time.
    let note = Origin::Typed.label_mark();
    for view in [vec![], vec![&note]] {
        let workbench = Compartment::workbench(view.iter().copied());
        assert!(
            workbench.label_mark().mark().is_marked(),
            "view of {} items",
            view.len()
        );
        assert!(sources(workbench.label_mark()).contains(&MarkSource::Workbench));
        assert!(workbench.emit("pushed commit").label_mark().mark().is_marked());
    }
}

#[test]
fn a_tool_started_on_marked_source_is_marked() {
    // ADR 0011: marks persist in a Tool's source. A research task's findings
    // became source; every later start that reads that source is marked.
    let mut build = Compartment::fresh();
    build.read(&Origin::Typed.label_mark()).unwrap();
    build
        .read(
            &Origin::WebPage {
                site: news(),
                login: false,
            }
            .label_mark(),
        )
        .unwrap();
    let source = build.emit("fn main() {}");

    let live = Compartment::fixed([source.label_mark()]);
    let preview = Compartment::fixed([source.label_mark()]);
    assert!(live.label_mark().mark().is_marked());
    assert!(preview.label_mark().mark().is_marked());
}
