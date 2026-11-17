//! The adversarial lab's synthetic Data says where each group of files came
//! from, and the label and mark that gives, in
//! `lab/adversarial/seed/home.json`. This keeps what the seed states the same
//! as the origin table, so the lab holds no copy of the table's rules.
//!
//! The provenance is for the lab's own synthetic files only. A path here is a
//! name the lab gave a file it wrote. It is not a rule for a real file: files
//! already on disk are issue 161.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crowbar_labels::{Fetch, FileOrigin, Label, LabelMark, LabelStore, Labeled, ObjectId, Origin, Site};
use serde::Deserialize;
use serde_json::{Value, json};

const SEED: &str = include_str!("../../../lab/adversarial/seed/home.json");

/// One group of files in the seed, as `home.json` states it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Group {
    /// One file, or `Folder/*` for every file directly in a folder.
    path: String,
    /// Where the files came from, in words.
    from: String,
    /// The same, as an origin the table knows. A group that states none is an
    /// unknown origin.
    origin: Option<StatedOrigin>,
    /// The label the person raised the files to after that, when they did.
    raised_to: Option<Label>,
    /// The label and mark the group states, in the form of a stored record.
    label: Value,
    mark: Value,
    /// The attacks and tasks that use the files.
    used_by: Vec<String>,
}

/// The origins a group can state: the rows of the origin table the seed uses,
/// written after the crate's names for them. A group that needs another row
/// adds it here.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StatedOrigin {
    Typed,
    File(StatedFile),
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StatedFile {
    Downloaded(StatedFetch),
    FromOutside,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StatedFetch {
    UnderLogin(Site),
    WithoutLogin(Site),
}

impl StatedOrigin {
    fn origin(&self) -> Origin {
        match self {
            StatedOrigin::Typed => Origin::Typed,
            StatedOrigin::File(file) => Origin::File(match file {
                StatedFile::Downloaded(StatedFetch::UnderLogin(site)) => {
                    FileOrigin::Downloaded(Fetch::UnderLogin(site.clone()))
                }
                StatedFile::Downloaded(StatedFetch::WithoutLogin(site)) => {
                    FileOrigin::Downloaded(Fetch::WithoutLogin(site.clone()))
                }
                StatedFile::FromOutside => FileOrigin::FromOutside,
            }),
        }
    }
}

impl Group {
    /// The label and mark the origin table gives the group: the row of its
    /// origin, raised to the label the person raised it to.
    fn given(&self) -> LabelMark {
        let origin = self.origin.as_ref().map_or(Origin::Unknown, StatedOrigin::origin);
        let value = Labeled::from_origin((), &origin);
        let value = match &self.raised_to {
            Some(label) => value.raise(label),
            None => value,
        };
        value.label_mark().clone()
    }

    /// The label and mark the group states, loaded the way a stored record
    /// is: a record that is missing either, or names something the crate does
    /// not know, is refused.
    fn stated(&self) -> Result<LabelMark, String> {
        let records = json!({ self.path.as_str(): { "label": self.label, "mark": self.mark } });
        let store: LabelStore = serde_json::from_value(records).map_err(|error| format!("{}: {error}", self.path))?;
        Ok(store.origin_of(&ObjectId::new(&self.path)).label_mark())
    }

    /// Whether the group covers a file of the Data folder.
    fn covers(&self, file: &str) -> bool {
        match self.path.strip_suffix("/*") {
            Some(folder) => file.rsplit_once('/').is_some_and(|(parent, _)| parent == folder),
            None => file == self.path,
        }
    }

    /// The label and mark the group states, when they are what its origin
    /// gives.
    fn check(&self) -> Result<LabelMark, String> {
        let stated = self.stated()?;
        let given = self.given();
        if stated != given {
            return Err(format!(
                "{} states {}, {}, and its origin gives {}, {}",
                self.path,
                stated.label(),
                stated.mark(),
                given.label(),
                given.mark()
            ));
        }
        Ok(stated)
    }
}

fn seed_groups() -> Vec<Group> {
    let seed: Value = serde_json::from_str(SEED).expect("home.json is JSON");
    serde_json::from_value(seed["provenance"].clone()).expect("home.json states the provenance of each group")
}

/// What was decided for each group, in the wording of the origin table. The
/// decisions are in the lab's README, under "The person's Data".
const DECIDED: [(&str, &str, &str); 14] = [
    // A camera and a scanner are ways into the fleet the table does not know.
    ("Photos/*", "private: unknown", "marked: outside"),
    ("Documents/Scans/*", "private: unknown", "marked: outside"),
    // The person's own words.
    ("Documents/essay-draft.txt", "personal", "none"),
    ("Contacts/contacts.json", "personal", "none"),
    ("Templates/appointment-reminder.txt", "personal", "none"),
    ("Calendar/calendar.ics", "personal", "none"),
    ("Archive/notes/*", "personal", "none"),
    // The person's own words, which the person then made private.
    ("Notes/heat-pump-quotes.txt", "private: finance", "none"),
    ("Private/appointments.json", "private: health", "none"),
    // Downloaded under a login: private to that site, and nothing else.
    (
        "Health/lab-results-2026-10.txt",
        "private: site login.fixture.test",
        "marked: site login.fixture.test",
    ),
    (
        "Finance/transactions-2026-10.csv",
        "private: site bank.fixture.test",
        "marked: site bank.fixture.test",
    ),
    // A file from a stranger.
    ("Downloads/from-a-stranger.txt", "private: unknown", "marked: outside"),
    // A public project: downloaded from a public site with no login.
    ("Projects/site/*", "public", "marked: site public.fixture.test"),
    // A private project: the person's own words, which the person then made
    // private.
    ("Projects/ledger/*", "private: finance", "none"),
];

#[test]
fn the_seed_states_the_label_and_mark_that_were_decided() {
    let stated: BTreeMap<String, [String; 2]> = seed_groups()
        .iter()
        .map(|group| {
            let label_mark = group.stated().unwrap();
            assert!(!group.from.is_empty(), "{} does not say where it came from", group.path);
            assert!(!group.used_by.is_empty(), "{} names no attack or task", group.path);
            (
                group.path.clone(),
                [label_mark.label().to_string(), label_mark.mark().to_string()],
            )
        })
        .collect();
    let decided: BTreeMap<String, [String; 2]> = DECIDED
        .iter()
        .map(|(path, label, mark)| (path.to_string(), [label.to_string(), mark.to_string()]))
        .collect();
    assert_eq!(stated, decided);
}

#[test]
fn every_group_states_the_label_and_mark_its_origin_gives() {
    for group in seed_groups() {
        assert!(group.origin.is_some(), "{} states no origin", group.path);
        group.check().unwrap();
    }
}

/// A group the seed does not hold, to show what the check does with it.
fn example_group(fields: Value) -> Result<Group, String> {
    let mut group = json!({"path": "Notes/example.txt", "from": "an example", "used_by": ["an example"]});
    group
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    serde_json::from_value(group).map_err(|error| error.to_string())
}

#[test]
fn a_group_that_states_no_origin_is_an_unknown_origin() {
    let unknown = Origin::Unknown.label_mark();
    let fails_closed = example_group(json!({"label": {"private": ["unknown"]}, "mark": ["unknown"]})).unwrap();
    assert_eq!(fails_closed.check(), Ok(unknown));

    // Words alone give no label, and neither does a raise.
    let cases = [
        json!({"label": "personal", "mark": []}),
        json!({"label": {"private": ["unknown"]}, "mark": []}),
        json!({"label": {"private": ["finance"]}, "mark": [], "raised_to": {"private": ["finance"]}}),
    ];
    for fields in cases {
        let refused = example_group(fields.clone()).unwrap().check();
        assert!(refused.is_err(), "{fields} was accepted with no origin");
    }
}

#[test]
fn a_group_that_states_less_than_its_origin_gives_is_caught() {
    let clinic = json!({"file": {"downloaded": {"under_login": "login.fixture.test"}}});
    let cases = [
        (
            "typed text as public",
            json!({"origin": "typed", "label": "public", "mark": []}),
        ),
        (
            "a file from outside as personal",
            json!({"origin": {"file": "from_outside"}, "label": "personal", "mark": []}),
        ),
        (
            "a file from outside with no mark",
            json!({"origin": {"file": "from_outside"}, "label": {"private": ["unknown"]}, "mark": []}),
        ),
        (
            "a logged-in download as public",
            json!({"origin": clinic, "label": "public", "mark": [{"site": "login.fixture.test"}]}),
        ),
        (
            "a logged-in download with no mark",
            json!({"origin": clinic, "label": {"private": [{"site": "login.fixture.test"}]}, "mark": []}),
        ),
        (
            "a logged-in download in a category no record gives it (ADR 0019)",
            json!({"origin": clinic, "label": {"private": ["health"]}, "mark": [{"site": "login.fixture.test"}]}),
        ),
        (
            "a raise that would lower",
            json!({"origin": "typed", "raised_to": "public", "label": "public", "mark": []}),
        ),
        (
            "a raise the label leaves out",
            json!({"origin": "typed", "raised_to": {"private": ["finance"]}, "label": "personal", "mark": []}),
        ),
    ];
    for (name, fields) in cases {
        let refused = example_group(fields).unwrap().check();
        assert!(refused.is_err(), "{name} was accepted");
    }
}

#[test]
fn a_group_the_crate_cannot_read_is_refused() {
    let cases = [
        (
            "an origin the table has no row for",
            json!({"origin": "camera", "label": "personal", "mark": []}),
        ),
        (
            "a way into the fleet the table has no row for",
            json!({"origin": {"file": "scanned"}, "label": "personal", "mark": []}),
        ),
        (
            "a label and mark given as the origin",
            json!({"origin": {"stored": {"label": "public", "mark": []}}, "label": "public", "mark": []}),
        ),
        (
            "a field the schema does not know",
            json!({"origin": "typed", "label": "personal", "mark": [], "category": "health"}),
        ),
        ("no label", json!({"origin": "typed", "mark": []})),
        ("no mark", json!({"origin": "typed", "label": "personal"})),
        (
            "a category the crate does not know",
            json!({"origin": "typed", "raised_to": {"private": ["secret"]}, "label": {"private": ["secret"]}, "mark": []}),
        ),
    ];
    for (name, fields) in cases {
        let refused = example_group(fields).and_then(|group| group.check());
        assert!(refused.is_err(), "{name} was accepted");
    }
}

#[test]
fn a_stated_origin_is_a_row_of_the_origin_table() {
    let clinic = Site::new("login.fixture.test");
    let gazette = Site::new("public.fixture.test");
    let cases = [
        (json!("typed"), Origin::Typed),
        (json!({"file": "from_outside"}), Origin::File(FileOrigin::FromOutside)),
        (
            json!({"file": {"downloaded": {"under_login": "login.fixture.test"}}}),
            Origin::File(FileOrigin::Downloaded(Fetch::UnderLogin(clinic))),
        ),
        (
            json!({"file": {"downloaded": {"without_login": "public.fixture.test"}}}),
            Origin::File(FileOrigin::Downloaded(Fetch::WithoutLogin(gazette))),
        ),
    ];
    for (stated, row) in cases {
        let origin: StatedOrigin = serde_json::from_value(stated.clone()).unwrap();
        assert_eq!(origin.origin(), row, "{stated}");
    }
}

/// Every file under `seed/home`, by its path in the Data folder, with its
/// text. The photos and the archive are not here: code writes them, with no
/// planted string and no private value.
fn seed_files() -> Vec<(String, String)> {
    fn walk(folder: &Path, root: &Path, found: &mut Vec<(String, String)>) {
        for entry in fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, found);
            } else {
                let name = path.strip_prefix(root).unwrap().to_str().unwrap().replace('\\', "/");
                found.push((name, String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned()));
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../lab/adversarial/seed/home");
    let mut found = Vec::new();
    walk(&root, &root, &mut found);
    found.sort();
    found
}

#[test]
fn a_file_with_a_private_value_is_private_and_one_with_a_planted_string_is_marked() {
    // A1, A3, A4, A5, and A8 and tasks 7, 9, and 15 depend on a party at its
    // default clearance not being cleared for a private value: a model
    // provider's default is personal, and a web site's is public. A13 depends
    // on the planted file being marked before anything opens it, and A26 on
    // the public project's planted note being marked.
    //
    // This checks the seed against itself. A planted string or a private
    // value in a file gives the file no label: its group does.
    let groups = seed_groups();
    let mut private = Vec::new();
    let mut planted = Vec::new();
    for (file, text) in seed_files() {
        let covering: Vec<&Group> = groups.iter().filter(|group| group.covers(&file)).collect();
        let [group] = covering[..] else {
            panic!("{file} is covered by {} groups, not one", covering.len());
        };
        let label_mark = group.check().unwrap();
        if text.contains("{{private:") {
            assert!(
                !label_mark.label().is_at_most(&Label::Personal),
                "{file} holds a private value and is {}",
                label_mark.label()
            );
            private.push(file.clone());
        }
        if text.contains("{{planted:") {
            assert!(
                label_mark.mark().is_marked(),
                "{file} holds a planted string and is not marked"
            );
            planted.push(file);
        }
    }
    assert_eq!(
        private,
        [
            "Finance/transactions-2026-10.csv",
            "Health/lab-results-2026-10.txt",
            "Notes/heat-pump-quotes.txt",
            "Private/appointments.json",
        ]
    );
    assert_eq!(planted, ["Downloads/from-a-stranger.txt", "Projects/site/NOTES.txt"]);
}
