//! The origin table gives every object its label and mark from where it came
//! from. Each expected row is taken from ADR 0008, ADR 0011, and the vision;
//! where they leave a row open, the row fails closed and says so.

use std::collections::BTreeSet;

use crowbar_labels::{AdmittedSet, Category, FileOrigin, Label, MailAccount, MarkSource, Origin, Site};

fn private(category: Category) -> Label {
    Label::private(category)
}

fn sources<const N: usize>(list: [MarkSource; N]) -> BTreeSet<MarkSource> {
    BTreeSet::from(list)
}

#[test]
fn every_origin_gets_its_label_and_mark_from_the_table() {
    let news = Site::new("news.example");
    let bank = Site::new("bank.example");
    let me = MailAccount::new("me@mail.example");

    let own_note = Origin::Typed.label_mark();
    let bank_page = Origin::WebPage {
        site: bank.clone(),
        login: true,
    }
    .label_mark();
    let a_mail = Origin::Mail { account: me.clone() }.label_mark();

    use MarkSource as S;
    let cases: Vec<(&str, Origin, Label, BTreeSet<MarkSource>)> = vec![
        // ADR 0008: what the person types counts as personal by rule.
        ("typed text", Origin::Typed, Label::Personal, sources([])),
        // ADR 0008: a string a fixed template produced is not data.
        ("fixed template", Origin::Template, Label::Public, sources([])),
        // Vision: messages are private by default. ADR 0011: mail is open.
        (
            "mail",
            Origin::Mail { account: me.clone() },
            private(Category::Messages),
            sources([S::Mail(me.clone())]),
        ),
        // ADR 0008: a web site's default is public. ADR 0011: web pages are open.
        (
            "web page",
            Origin::WebPage {
                site: news.clone(),
                login: false,
            },
            Label::Public,
            sources([S::Site(news.clone())]),
        ),
        // ADR 0008: content fetched under a login is private to that site.
        (
            "web page under a login",
            Origin::WebPage {
                site: bank.clone(),
                login: true,
            },
            private(Category::Site(bank.clone())),
            sources([S::Site(bank.clone())]),
        ),
        // A file takes the row of where its content came from.
        (
            "downloaded file",
            Origin::File(FileOrigin::Downloaded {
                site: news.clone(),
                login: false,
            }),
            Label::Public,
            sources([S::Site(news.clone())]),
        ),
        (
            "saved mail attachment",
            Origin::File(FileOrigin::Attachment { account: me.clone() }),
            private(Category::Messages),
            sources([S::Mail(me.clone())]),
        ),
        // ADR 0011: files from outside are open. No record gives their label,
        // so it fails closed.
        (
            "file from outside",
            Origin::File(FileOrigin::FromOutside),
            private(Category::Unknown),
            sources([S::Outside]),
        ),
        // ADR 0011: admitted input does not mark.
        (
            "Base package snapshot",
            Origin::Admitted(AdmittedSet::BasePackages),
            Label::Public,
            sources([]),
        ),
        (
            "build dependency set",
            Origin::Admitted(AdmittedSet::BuildDependencies),
            Label::Public,
            sources([]),
        ),
        // ADR 0008: a paste is a handle with its origin's label and mark, and
        // nothing more. A paste whose source was not recorded is an unknown
        // origin, and fails closed.
        (
            "paste of the person's own note",
            Origin::Pasted {
                copied_from: Some(own_note),
            },
            Label::Personal,
            sources([]),
        ),
        (
            "paste of a logged-in page",
            Origin::Pasted {
                copied_from: Some(bank_page),
            },
            private(Category::Site(bank.clone())),
            sources([S::Site(bank.clone())]),
        ),
        (
            "paste with no recorded source",
            Origin::Pasted { copied_from: None },
            private(Category::Unknown),
            sources([S::Paste]),
        ),
        // Marks persist in storage: a stored result keeps what it was stored with.
        (
            "stored result",
            Origin::Stored(a_mail),
            private(Category::Messages),
            sources([S::Mail(me.clone())]),
        ),
        // Nothing says what an unknown origin gets, so it fails closed.
        (
            "unknown origin",
            Origin::Unknown,
            private(Category::Unknown),
            sources([S::Unknown]),
        ),
    ];

    for (row, origin, label, marks) in cases {
        let got = origin.label_mark();
        assert_eq!(got.label(), &label, "label of {row}");
        assert_eq!(
            got.mark().sources().cloned().collect::<BTreeSet<_>>(),
            marks,
            "mark of {row}"
        );
        assert_eq!(got.mark().is_marked(), !marks.is_empty(), "is_marked of {row}");
        assert_eq!(origin.label_mark(), got, "{row} is not deterministic");
    }
}

#[test]
fn an_unknown_origin_is_held_to_no_lower_a_label_than_any_known_row() {
    let unknown = Origin::Unknown.label_mark();
    for known in [
        Origin::Typed,
        Origin::Template,
        Origin::WebPage {
            site: Site::new("news.example"),
            login: false,
        },
        Origin::Admitted(AdmittedSet::BasePackages),
    ] {
        let known = known.label_mark();
        assert!(known.label().is_at_most(unknown.label()), "{known:?} above {unknown:?}");
    }
    // An unknown origin is never cleared by a clearance that names only known
    // categories.
    let named = Label::private(Category::Messages)
        .join(&Label::private(Category::Health))
        .join(&Label::private(Category::Finance));
    assert!(!unknown.label().is_at_most(&named));
}
