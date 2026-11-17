//! The origin table gives every object its label and mark from where it came
//! from. Each expected row is taken from ADR 0008, ADR 0011, and the vision;
//! where they leave a row open, the row fails closed and says so.

use std::collections::BTreeSet;

use crowbar_labels::{AdmittedSet, Category, Fetch, FileOrigin, Label, MailAccount, MarkSource, Origin, Site};

mod common;
use common::private;

fn marks<const N: usize>(list: [MarkSource; N]) -> BTreeSet<MarkSource> {
    BTreeSet::from(list)
}

#[test]
fn every_origin_gets_its_label_and_mark_from_the_table() {
    let news = Site::new("news.example");
    let bank = Site::new("bank.example");
    let me = MailAccount::new("me@mail.example");

    let own_note = Origin::Typed.label_mark();
    let bank_page = Origin::WebPage(Fetch::UnderLogin(bank.clone())).label_mark();
    let a_mail = Origin::Mail { account: me.clone() }.label_mark();

    use MarkSource as S;
    let cases: Vec<(&str, Origin, Label, BTreeSet<MarkSource>)> = vec![
        // ADR 0008: what the person types counts as personal by rule.
        ("typed text", Origin::Typed, Label::Personal, marks([])),
        // ADR 0008: a string a fixed template produced is not data.
        ("fixed template", Origin::Template, Label::Public, marks([])),
        // Vision: messages are private by default. ADR 0011: mail is open.
        (
            "mail",
            Origin::Mail { account: me.clone() },
            private(&[Category::Messages]),
            marks([S::Mail(me.clone())]),
        ),
        // ADR 0008: a web site's default is public. ADR 0011: web pages are open.
        (
            "web page",
            Origin::WebPage(Fetch::WithoutLogin(news.clone())),
            Label::Public,
            marks([S::Site(news.clone())]),
        ),
        // ADR 0008: content fetched under a login is private to that site.
        (
            "web page under a login",
            Origin::WebPage(Fetch::UnderLogin(bank.clone())),
            private(&[Category::Site(bank.clone())]),
            marks([S::Site(bank.clone())]),
        ),
        // A file takes the row of where its content came from.
        (
            "downloaded file",
            Origin::File(FileOrigin::Downloaded(Fetch::WithoutLogin(news.clone()))),
            Label::Public,
            marks([S::Site(news.clone())]),
        ),
        (
            "saved mail attachment",
            Origin::File(FileOrigin::Attachment { account: me.clone() }),
            private(&[Category::Messages]),
            marks([S::Mail(me.clone())]),
        ),
        // ADR 0019, ADR 0025: the person chose a file they bring in by hand,
        // so its label is theirs to decide. A stranger may have written it,
        // so it is marked.
        (
            "file the person brought in by hand",
            Origin::File(FileOrigin::BroughtInByHand),
            Label::Personal,
            marks([S::Outside]),
        ),
        // ADR 0011: files from outside are open. With no act of the person's
        // behind one, no record gives its label, so it fails closed.
        (
            "file from outside",
            Origin::File(FileOrigin::FromOutside),
            private(&[Category::Unknown]),
            marks([S::Outside]),
        ),
        // ADR 0011: admitted input does not mark.
        (
            "Base package snapshot",
            Origin::Admitted(AdmittedSet::BasePackages),
            Label::Public,
            marks([]),
        ),
        (
            "build dependency set",
            Origin::Admitted(AdmittedSet::BuildDependencies),
            Label::Public,
            marks([]),
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
            marks([]),
        ),
        (
            "paste of a logged-in page",
            Origin::Pasted {
                copied_from: Some(bank_page),
            },
            private(&[Category::Site(bank.clone())]),
            marks([S::Site(bank.clone())]),
        ),
        (
            "paste with no recorded source",
            Origin::Pasted { copied_from: None },
            private(&[Category::Unknown]),
            marks([S::Paste]),
        ),
        // Marks persist in storage: a stored result keeps what it was stored with.
        (
            "stored result",
            Origin::Stored(a_mail),
            private(&[Category::Messages]),
            marks([S::Mail(me.clone())]),
        ),
        // Nothing says what an unknown origin gets, so it fails closed.
        (
            "unknown origin",
            Origin::Unknown,
            private(&[Category::Unknown]),
            marks([S::Unknown]),
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
fn an_unknown_origin_is_above_every_public_and_personal_row_and_outside_every_named_category() {
    let unknown = Origin::Unknown.label_mark();
    for known in [
        Origin::Typed,
        Origin::Template,
        Origin::WebPage(Fetch::WithoutLogin(Site::new("news.example"))),
        Origin::Admitted(AdmittedSet::BasePackages),
    ] {
        let known = known.label_mark();
        assert!(known.label().is_at_most(unknown.label()), "{known:?} above {unknown:?}");
    }
    // An unknown origin is never cleared by a clearance that names only known
    // categories.
    let named = private(&[Category::Messages, Category::Health, Category::Finance]);
    assert!(!unknown.label().is_at_most(&named));
}
