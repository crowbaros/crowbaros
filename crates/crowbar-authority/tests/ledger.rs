//! The ledger is the history of what the gate decided and what was done. It
//! is only ever added to, and one bad line never hides the rest.

use std::path::{Path, PathBuf};

use crowbar_authority::{Damage, Decision, Event, Keyed, Ledger, LedgerEntry, Outcome};
use crowbar_capability::{
    Answer, Ask, AskWhy, Cost, GateRule, HandleId, IdempotencyKey, Limit, Principal, Refuse, TaskId,
};

mod common;
use common::{LATER, NOW, Scratch};

fn file(dir: &Path) -> PathBuf {
    dir.join("entries")
}

fn task() -> Principal {
    Principal::Task(TaskId::new("reply-to-landlord"))
}

fn entry(operation: &str, event: Event) -> LedgerEntry {
    LedgerEntry::new(task(), operation, event)
}

fn refused() -> LedgerEntry {
    entry(
        "http.get",
        Event::Decided(Decision::Refused(Refuse::with(
            GateRule::Clearance,
            Limit::Clearance(crowbar_labels::Label::Public),
        ))),
    )
}

fn asked() -> LedgerEntry {
    entry(
        "mail.send",
        Event::Decided(Decision::Asked(Ask {
            why: AskWhy::Crosses,
            answer: Answer::ConfirmGesture,
            held: None,
        })),
    )
}

fn sent() -> LedgerEntry {
    entry("mail.send", Event::Effect(Outcome::Done))
}

#[test]
fn decisions_and_effects_are_read_back_in_the_order_they_happened() {
    let dir = Scratch::new();
    let mut ledger = Ledger::open(dir.path()).expect("opened");
    assert_eq!(ledger.append(&refused(), NOW).expect("recorded"), 1);
    assert_eq!(ledger.append(&asked(), NOW).expect("recorded"), 2);
    drop(ledger);

    // A later run of the broker goes on from where the last one stopped.
    let mut ledger = Ledger::open(dir.path()).expect("opened");
    assert_eq!(ledger.append(&sent(), LATER).expect("recorded"), 3);
    drop(ledger);

    let history = Ledger::read(dir.path()).expect("read");
    let read: Vec<_> = history
        .entries
        .iter()
        .map(|recorded| (recorded.seq, recorded.time, &recorded.entry))
        .collect();
    assert_eq!(
        read,
        vec![(1, NOW, &refused()), (2, NOW, &asked()), (3, LATER, &sent())]
    );
    assert_eq!(history.damaged, vec![]);
    assert_eq!(history.unfinished_write, None);
}

#[test]
fn a_use_is_read_back_with_its_handle_original_task_objects_cost_and_key() {
    let dir = Scratch::new();
    let used = LedgerEntry {
        handle: Some("0123456789abcdef0123456789abcdef".parse::<HandleId>().expect("an id")),
        original: Some("fedcba9876543210fedcba9876543210".parse::<HandleId>().expect("an id")),
        task: Some(TaskId::new("reply-to-landlord")),
        objects: Some(3),
        cost: Some(Cost::cents(25)),
        key: Some(Keyed {
            key: IdempotencyKey::new("send-1"),
            digest: "ab".repeat(32),
        }),
        ..entry("mail.send", Event::Decided(Decision::Allowed { under: vec![] }))
    };
    let mut ledger = Ledger::open(dir.path()).expect("opened");
    ledger.append(&used, NOW).expect("recorded");
    ledger
        .append(&entry("mail.send", Event::AskClosed), LATER)
        .expect("recorded");
    drop(ledger);

    let history = Ledger::read(dir.path()).expect("read");
    let read: Vec<_> = history.entries.iter().map(|recorded| &recorded.entry).collect();
    assert_eq!(read, vec![&used, &entry("mail.send", Event::AskClosed)]);
}

#[test]
fn an_entry_is_added_and_nothing_before_it_is_rewritten() {
    let dir = Scratch::new();
    let mut ledger = Ledger::open(dir.path()).expect("opened");
    ledger.append(&refused(), NOW).expect("recorded");
    let before = std::fs::read(file(dir.path())).expect("the ledger");

    ledger.append(&asked(), NOW).expect("recorded");
    drop(ledger);
    Ledger::open(dir.path())
        .expect("opened")
        .append(&sent(), LATER)
        .expect("recorded");

    assert!(
        std::fs::read(file(dir.path()))
            .expect("the ledger")
            .starts_with(&before)
    );
}

#[test]
fn a_damaged_line_is_reported_by_number_and_the_rest_is_still_read() {
    let dir = Scratch::new();
    let mut ledger = Ledger::open(dir.path()).expect("opened");
    for entry in [refused(), asked(), sent()] {
        ledger.append(&entry, NOW).expect("recorded");
    }
    drop(ledger);

    // Storage goes bad in the second entry.
    let mut bytes = std::fs::read(file(dir.path())).expect("the ledger");
    let at = bytes.windows(5).position(|w| w == b"asked").expect("the decision");
    bytes[at] = b'u';
    std::fs::write(file(dir.path()), &bytes).expect("written");

    let history = Ledger::read(dir.path()).expect("read");
    let read: Vec<_> = history
        .entries
        .iter()
        .map(|recorded| (recorded.seq, &recorded.entry))
        .collect();
    assert_eq!(read, vec![(1, &refused()), (3, &sent())]);
    assert_eq!(history.damaged, vec![(2, Damage::Unreadable)]);

    // The history goes on after it.
    let mut ledger = Ledger::open(dir.path()).expect("opened");
    assert_eq!(ledger.append(&refused(), LATER).expect("recorded"), 4);
    drop(ledger);
    let history = Ledger::read(dir.path()).expect("read");
    assert_eq!(history.entries.len(), 3);
    assert_eq!(history.damaged, vec![(2, Damage::Unreadable)]);
}

#[test]
fn a_removed_entry_is_reported_once_and_the_entries_after_it_are_still_read() {
    let dir = Scratch::new();
    let mut ledger = Ledger::open(dir.path()).expect("opened");
    for entry in [refused(), asked(), sent(), refused()] {
        ledger.append(&entry, NOW).expect("recorded");
    }
    drop(ledger);

    // Someone takes the ask out of the history.
    let bytes = std::fs::read(file(dir.path())).expect("the ledger");
    let mut lines: Vec<_> = bytes.split_inclusive(|byte| *byte == b'\n').collect();
    lines.remove(1);
    std::fs::write(file(dir.path()), lines.concat()).expect("written");

    // The entry after the gap says where the gap is, and keeps its number.
    let history = Ledger::read(dir.path()).expect("read");
    let read: Vec<_> = history
        .entries
        .iter()
        .map(|recorded| (recorded.seq, &recorded.entry))
        .collect();
    assert_eq!(read, vec![(1, &refused()), (3, &sent()), (4, &refused())]);
    assert_eq!(history.damaged, vec![(2, Damage::OutOfPlace)]);

    // The next entry takes the next number, not a used one.
    let mut ledger = Ledger::open(dir.path()).expect("opened");
    assert_eq!(ledger.append(&asked(), LATER).expect("recorded"), 5);
}

#[test]
fn a_write_cut_short_by_a_crash_loses_only_that_entry() {
    let dir = Scratch::new();
    let mut ledger = Ledger::open(dir.path()).expect("opened");
    ledger.append(&refused(), NOW).expect("recorded");
    drop(ledger);

    let mut bytes = std::fs::read(file(dir.path())).expect("the ledger");
    let half = bytes[..bytes.len() / 2].to_vec();
    bytes.extend_from_slice(&half);
    std::fs::write(file(dir.path()), &bytes).expect("written");

    let history = Ledger::read(dir.path()).expect("read");
    assert_eq!(history.entries.len(), 1);
    assert_eq!(history.damaged, vec![]);
    assert_eq!(history.unfinished_write, Some(half.len()));

    // The writer that removes it says so, because nothing on disk will.
    let mut ledger = Ledger::open(dir.path()).expect("opened");
    assert_eq!(ledger.unfinished_write(), Some(half.len()));
    assert_eq!(ledger.append(&sent(), LATER).expect("recorded"), 2);
    drop(ledger);
    let history = Ledger::read(dir.path()).expect("read");
    assert_eq!(history.entries.len(), 2);
    assert_eq!(history.unfinished_write, None);
    assert_eq!(Ledger::open(dir.path()).expect("opened").unfinished_write(), None);
}
