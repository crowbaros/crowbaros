//! The origin table is written twice: as a table in the module docs of
//! `src/origin.rs`, and as code. This keeps the two the same.

use crowbar_labels::{AdmittedSet, FileOrigin, MailAccount, Origin, Site};

const SOURCE: &str = include_str!("../src/origin.rs");

/// The rows of the documented table: (origin, label, mark), backticks removed.
fn documented_rows() -> Vec<[String; 3]> {
    SOURCE
        .lines()
        .filter_map(|line| line.strip_prefix("//! |"))
        .skip(2) // the header and the separator
        .map(|row| {
            let cells: Vec<String> = row
                .trim_end()
                .trim_end_matches('|')
                .split(" | ")
                .map(|cell| cell.trim().replace('`', ""))
                .collect();
            cells.try_into().unwrap_or_else(|cells| panic!("a row needs three cells: {cells:?}"))
        })
        .collect()
}

/// The origin each documented row describes, built from its example values.
/// `None` for the stored-result row, whose label and mark are whatever was
/// stored.
fn origin_for(row: &str) -> Option<Origin> {
    let news = Site::new("news.example");
    let bank = Site::new("bank.example");
    let me = MailAccount::new("me@mail.example");
    Some(match row {
        "Text the person typed" => Origin::Typed,
        "Text a fixed template produced" => Origin::Template,
        "Mail in me@mail.example" => Origin::Mail { account: me },
        "A page from news.example" => Origin::WebPage { site: news, login: false },
        "A page from bank.example, under a login" => Origin::WebPage { site: bank, login: true },
        "A file downloaded from news.example" => Origin::File(FileOrigin::Downloaded { site: news, login: false }),
        "A file saved from an attachment in me@mail.example" => Origin::File(FileOrigin::Attachment { account: me }),
        "A file from outside the fleet, by any other way" => Origin::File(FileOrigin::FromOutside),
        "The Base package snapshot" => Origin::Admitted(AdmittedSet::BasePackages),
        "The dependency set for build tasks" => Origin::Admitted(AdmittedSet::BuildDependencies),
        "A paste of a note the person typed" => Origin::Pasted { copied_from: Some(Origin::Typed.label_mark()) },
        "A paste from a page of bank.example, under a login" => Origin::Pasted {
            copied_from: Some(Origin::WebPage { site: bank, login: true }.label_mark()),
        },
        "A paste whose source was not recorded" => Origin::Pasted { copied_from: None },
        "Anything else" => Origin::Unknown,
        "A stored result" => return None,
        other => panic!("the documented row {other:?} has no origin in this test"),
    })
}

#[test]
fn the_documented_table_matches_the_code() {
    let rows = documented_rows();
    assert_eq!(rows.len(), 15, "the table changed size; add the new rows here");
    for [row, label, mark] in rows {
        match origin_for(&row) {
            Some(origin) => {
                let got = origin.label_mark();
                assert_eq!(got.label().to_string(), label, "label of {row:?}");
                assert_eq!(got.mark().to_string(), mark, "mark of {row:?}");
            }
            None => assert_eq!([label.as_str(), mark.as_str()], ["as it was stored"; 2], "{row:?}"),
        }
    }
}
