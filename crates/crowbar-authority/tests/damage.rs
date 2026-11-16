//! What is on disk is the record. A write that a crash cut short loses only
//! itself, and a record that was changed, cut, or reordered is not read at
//! all: a store that cannot be trusted grants nothing.

use std::path::{Path, PathBuf};

use crowbar_authority::{Authority, AuthorityStore, Damage, Error, Grant, Party};
use crowbar_broker::EffectLimits;
use crowbar_labels::{Label, Site};

mod common;
use common::{LATER, NOW, Scratch, morning_pages, sync_rule, writing_grant};

fn file(dir: &Path) -> PathBuf {
    dir.join("authority.log")
}

/// The lines of the record, each with its line break.
fn lines(dir: &Path) -> Vec<Vec<u8>> {
    let bytes = std::fs::read(file(dir)).expect("the record");
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .map(<[u8]>::to_vec)
        .collect()
}

fn write_lines(dir: &Path, lines: &[Vec<u8>]) {
    std::fs::write(file(dir), lines.concat()).expect("written");
}

#[test]
fn a_write_cut_short_by_a_crash_loses_only_that_change() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    let id = store.grant(writing_grant(), NOW).expect("recorded");
    store.revoke(id, NOW).expect("revoked");
    drop(store);

    // The crash: the first half of a third change reaches the disk.
    let mut cut = lines(dir.path());
    let half = cut[0][..cut[0].len() / 2].to_vec();
    cut.push(half.clone());
    write_lines(dir.path(), &cut);

    // The two changes before it stand, and the reader says what it left out.
    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(authority.revoked_at(id), Some(NOW));
    assert_eq!(authority.history().len(), 2);
    assert_eq!(authority.unfinished_write(), Some(half.len()));

    // The next change is recorded whole, after the two that stand.
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    assert_eq!(store.authority().unfinished_write(), Some(half.len()));
    let again = store.grant(writing_grant(), LATER).expect("recorded");
    drop(store);

    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(authority.unfinished_write(), None);
    assert_eq!(authority.history().len(), 3);
    assert_eq!(
        authority.grants(&morning_pages()).collect::<Vec<_>>(),
        vec![(again, &writing_grant())]
    );
}

#[test]
fn a_change_that_lost_only_its_line_break_still_stands() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    let id = store.grant(writing_grant(), NOW).expect("recorded");
    store.revoke(id, NOW).expect("revoked");
    drop(store);

    // Every byte of the revocation reached the disk but the last.
    let mut whole = std::fs::read(file(dir.path())).expect("the record");
    assert_eq!(whole.pop(), Some(b'\n'));
    std::fs::write(file(dir.path()), &whole).expect("written");

    // It is a whole change that checks out, so it stands: dropping it would
    // bring the grant back.
    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(authority.revoked_at(id), Some(NOW));
    assert_eq!(authority.unfinished_write(), None);

    // The next change starts on a line of its own.
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    store.grant(writing_grant(), LATER).expect("recorded");
    drop(store);
    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(authority.history().len(), 3);
    assert_eq!(authority.revoked_at(id), Some(NOW));
}

/// A record of three changes: a grant limited to 5 objects, a rule, and the
/// grant's revocation.
fn three_changes(dir: &Path) {
    let mut store = AuthorityStore::open(dir).expect("opened");
    let limited = Grant {
        limits: EffectLimits::new().count(5),
        ..writing_grant()
    };
    let id = store.grant(limited, NOW).expect("recorded");
    store.rule(sync_rule(), NOW).expect("recorded");
    store.revoke(id, LATER).expect("revoked");
}

/// Both ways in refuse the record and name the line.
fn assert_damaged(dir: &Path, line: usize, damage: Damage) {
    for refused in [
        Authority::load(dir).map(|_| ()).unwrap_err(),
        AuthorityStore::open(dir).map(|_| ()).unwrap_err(),
    ] {
        match refused {
            Error::Damaged {
                line: found,
                damage: what,
                ..
            } => assert_eq!((found, what), (line, damage), "{refused}"),
            other => panic!("expected a damaged record, got: {other}"),
        }
    }
}

#[test]
fn a_changed_byte_in_a_recorded_change_is_refused_and_not_read_as_a_wider_grant() {
    let dir = Scratch::new();
    three_changes(dir.path());

    // Storage goes bad in the one place that would matter most: the grant's
    // limit of 5 objects reads as 9. The line is still well-formed.
    let mut changed = lines(dir.path());
    let at = changed[0]
        .windows(9)
        .position(|w| w == b"\"count\":5")
        .expect("the limit");
    changed[0][at + 8] = b'9';
    write_lines(dir.path(), &changed);

    assert_damaged(dir.path(), 1, Damage::Unreadable);
}

#[test]
fn a_changed_byte_in_the_last_change_is_refused_too() {
    let dir = Scratch::new();
    three_changes(dir.path());

    // The last line has no line after it to vouch for it. The revocation
    // names grant 1; it now names rule 2, which would bring the grant back.
    let mut changed = lines(dir.path());
    let at = changed[2]
        .windows(11)
        .position(|w| w == b"\"revoked\":1")
        .expect("the id");
    changed[2][at + 10] = b'2';
    write_lines(dir.path(), &changed);

    assert_damaged(dir.path(), 3, Damage::Unreadable);
}

/// The lines of a record, for a test to tamper with.
type Lines = Vec<Vec<u8>>;

#[test]
fn a_record_with_a_change_removed_or_out_of_order_is_refused() {
    let cases: [(&str, fn(&mut Lines), usize); 4] = [
        // Without its second change, the third no longer follows the first.
        ("a change removed from the middle", |lines| drop(lines.remove(1)), 2),
        ("the first change removed", |lines| drop(lines.remove(0)), 1),
        ("two changes swapped", |lines| lines.swap(1, 2), 2),
        ("a change recorded twice", |lines| lines.push(lines[2].clone()), 4),
    ];
    for (case, tamper, line) in cases {
        let dir = Scratch::new();
        three_changes(dir.path());
        let mut tampered = lines(dir.path());
        tamper(&mut tampered);
        write_lines(dir.path(), &tampered);

        // Each line is still the change that was written. It is its place
        // that is wrong.
        println!("{case}");
        assert_damaged(dir.path(), line, Damage::OutOfPlace);
    }
}

#[test]
fn a_change_from_another_record_is_refused() {
    let dir = Scratch::new();
    three_changes(dir.path());

    // Another store's first change is a whole, well-formed line. Put in
    // place of this record's revocation, it would bring the grant back.
    let other = Scratch::new();
    AuthorityStore::open(other.path())
        .expect("opened")
        .grant(writing_grant(), NOW)
        .expect("recorded");
    let mut spliced = lines(dir.path());
    spliced[2] = lines(other.path()).remove(0);
    write_lines(dir.path(), &spliced);

    assert_damaged(dir.path(), 3, Damage::OutOfPlace);
}

#[test]
fn a_damaged_record_is_left_as_it_was_found() {
    let dir = Scratch::new();
    three_changes(dir.path());
    let mut changed = lines(dir.path());
    changed[1][70] ^= 0x01;
    changed.push(b"half a chan".to_vec());
    write_lines(dir.path(), &changed);
    let before = std::fs::read(file(dir.path())).expect("the record");

    assert_damaged(dir.path(), 2, Damage::Unreadable);
    assert_eq!(std::fs::read(file(dir.path())).expect("the record"), before);
}

#[test]
fn only_one_writer_has_the_record_open() {
    let dir = Scratch::new();
    let mut first = AuthorityStore::open(dir.path()).expect("opened");

    let refused = AuthorityStore::open(dir.path()).map(|_| ()).unwrap_err();
    assert!(matches!(refused, Error::Locked { .. }), "{refused}");

    // The gate reads while the writer has it open, and the writer goes on.
    let id = first.grant(writing_grant(), NOW).expect("recorded");
    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(
        authority.grants(&morning_pages()).collect::<Vec<_>>(),
        vec![(id, &writing_grant())]
    );

    drop(first);
    AuthorityStore::open(dir.path()).expect("the first writer is gone");
}

/// The SHA-256 of `bytes`, as the record writes it.
fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn a_well_formed_line_that_revokes_what_was_never_granted_is_refused() {
    let dir = Scratch::new();
    AuthorityStore::open(dir.path())
        .expect("opened")
        .grant(writing_grant(), NOW)
        .expect("recorded");

    // Written by hand, to the format: the hash of the JSON, a space, and the
    // JSON, which names its place and the hash of the whole line before it.
    let mut forged = lines(dir.path());
    let first = forged[0].strip_suffix(b"\n").expect("a whole line");
    let json = format!(
        r#"{{"seq":2,"prev":"{}","time":1000,"entry":{{"revoked":7}}}}"#,
        sha256(first)
    );
    forged.push(format!("{} {json}\n", sha256(json.as_bytes())).into_bytes());
    write_lines(dir.path(), &forged);

    assert_damaged(dir.path(), 2, Damage::NotAdmitted);
}

#[test]
fn a_change_is_added_to_the_record_and_nothing_before_it_is_rewritten() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    let id = store.grant(writing_grant(), NOW).expect("recorded");
    let granted = std::fs::read(file(dir.path())).expect("the record");

    store.revoke(id, LATER).expect("revoked");
    store
        .set_clearance(Party::Site(Site::new("bank.example")), Label::Personal, LATER)
        .expect("set");

    // The grant is still there, byte for byte. Revoking it added a line.
    let after = std::fs::read(file(dir.path())).expect("the record");
    assert!(after.starts_with(&granted));
    assert_eq!(lines(dir.path()).len(), 3);
}

#[test]
fn the_record_is_made_for_its_owner_alone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Scratch::new();
    let domain = dir.path().join("crowbar/authority");
    AuthorityStore::open(&domain).expect("opened");

    let mode = |path: &Path| std::fs::metadata(path).expect("made").permissions().mode() & 0o777;
    assert_eq!(mode(&domain), 0o700);
    assert_eq!(mode(&file(&domain)), 0o600);
}
