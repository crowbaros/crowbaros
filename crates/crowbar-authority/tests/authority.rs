//! The authority domain records what the person granted, in a directory of
//! its own. What it holds is what was recorded there, and nothing else
//! changes it.

use crowbar_authority::{Authority, AuthorityStore, Change, Error, Grant, Holder, Party, Rule, Status};
use crowbar_capability::{Cost, EffectLimits};
use crowbar_labels::{Category, Label, Site};

mod common;
use common::{LATER, NOW, Scratch, morning_pages, sync_rule, writing_grant};

#[test]
fn a_grant_is_still_held_after_the_store_is_opened_again() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    let id = store.grant(writing_grant(), NOW).expect("recorded");
    drop(store);

    let authority = Authority::load(dir.path()).expect("loaded");
    let held: Vec<_> = authority.grants(&morning_pages()).collect();
    assert_eq!(held, vec![(id, &writing_grant())]);
}

#[test]
fn a_revoked_grant_is_not_held_now_or_after_the_store_is_opened_again() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    let id = store.grant(writing_grant(), NOW).expect("recorded");

    store.revoke(id, NOW).expect("revoked");
    assert_eq!(store.authority().expect("readable").grants(&morning_pages()).count(), 0);
    drop(store);

    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(authority.grants(&morning_pages()).count(), 0);
}

#[test]
fn granting_again_after_a_revocation_is_a_new_grant_and_the_old_one_stays_revoked() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    let first = store.grant(writing_grant(), NOW).expect("recorded");
    store.revoke(first, NOW).expect("revoked");

    let second = store.grant(writing_grant(), LATER).expect("recorded");
    assert_ne!(first, second);

    let authority = store.authority().expect("readable");
    assert_eq!(
        authority.grants(&morning_pages()).collect::<Vec<_>>(),
        vec![(second, &writing_grant())]
    );
    assert_eq!(authority.status(first), Some(Status::Revoked(NOW)));
    assert_eq!(authority.status(second), Some(Status::Held));
}

#[test]
fn revoking_an_id_that_names_nothing_is_refused_and_records_nothing() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    let id = store.grant(writing_grant(), NOW).expect("recorded");
    drop(store);

    // A second directory gives an id this store never recorded.
    let other = Scratch::new();
    let mut elsewhere = AuthorityStore::create(other.path()).expect("made");
    elsewhere.grant(writing_grant(), NOW).expect("recorded");
    let unknown = elsewhere.grant(writing_grant(), NOW).expect("recorded");

    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    let refused = store.revoke(unknown, NOW).unwrap_err();
    assert!(
        matches!(refused, Error::NotStanding(named) if named == unknown),
        "{refused}"
    );
    assert_eq!(store.authority().expect("readable").history().len(), 1);
    drop(store);

    let authority = Authority::load(dir.path()).expect("the refused change left the record whole");
    assert_eq!(
        authority.grants(&morning_pages()).collect::<Vec<_>>(),
        vec![(id, &writing_grant())]
    );
}

#[test]
fn revoking_twice_keeps_the_first_revocation() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    let id = store.grant(writing_grant(), NOW).expect("recorded");
    store.revoke(id, NOW).expect("revoked");

    store.revoke(id, LATER).expect("already revoked");
    assert_eq!(
        store.authority().expect("readable").status(id),
        Some(Status::Revoked(NOW))
    );
    assert_eq!(store.authority().expect("readable").history().len(), 2);
}

#[test]
fn a_standing_rule_is_held_until_it_is_revoked() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    let grant = store.grant(writing_grant(), NOW).expect("recorded");
    let rule = store.add_rule(sync_rule(), NOW).expect("recorded");
    assert_eq!(
        store
            .authority()
            .expect("readable")
            .rules(&morning_pages())
            .collect::<Vec<_>>(),
        vec![(rule, &sync_rule())]
    );

    store.revoke(rule, LATER).expect("revoked");
    drop(store);

    // The rule is gone and the grant is not: each is revoked on its own.
    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(authority.rules(&morning_pages()).count(), 0);
    assert_eq!(authority.status(rule), Some(Status::Revoked(LATER)));
    assert_eq!(
        authority.grants(&morning_pages()).collect::<Vec<_>>(),
        vec![(grant, &writing_grant())]
    );
}

#[test]
fn a_standing_rule_with_no_rate_limit_is_refused_and_records_nothing() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    let unlimited = Rule {
        limits: EffectLimits::new().count(5),
        ..sync_rule()
    };

    let refused = store.add_rule(unlimited, NOW).unwrap_err();
    assert!(matches!(refused, Error::RuleWithoutRate), "{refused}");
    assert_eq!(store.authority().expect("readable").rules(&morning_pages()).count(), 0);
    assert!(store.authority().expect("readable").history().is_empty());
}

#[test]
fn a_standing_rules_cap_is_kept_after_the_store_is_opened_again() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    // "Pay the electric bill, up to $150."
    let capped = Rule {
        cap: Some(Cost::cents(15_000)),
        ..sync_rule()
    };
    let id = store.add_rule(capped.clone(), NOW).expect("recorded");
    drop(store);

    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(
        authority.rules(&morning_pages()).collect::<Vec<_>>(),
        vec![(id, &capped)]
    );
}

#[test]
fn a_rule_stored_with_no_cap_has_none_and_is_stored_the_same_way_again() {
    // A rule as it was stored before a rule could carry a cap.
    let stored = serde_json::json!({
        "holder": {"tool": "morning-pages"},
        "operation": "nextcloud.sync",
        "limits": {"rate": {"count": 1, "window_secs": 600}},
        "sources": [],
    });

    let rule: Rule = serde_json::from_value(stored.clone()).expect("read");
    assert_eq!(rule, sync_rule());
    assert_eq!(rule.cap, None);
    assert_eq!(serde_json::to_value(&rule).expect("written"), stored);
}

#[test]
fn a_standing_grant_keeps_the_label_its_approval_showed_after_the_store_is_opened_again() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    // "Morning Pages may read and write your writing", which the approval
    // showed as personal.
    let shown = Grant {
        shows: Some(Label::Personal),
        ..writing_grant()
    };
    let id = store.grant(shown.clone(), NOW).expect("recorded");
    drop(store);

    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(
        authority.grants(&morning_pages()).collect::<Vec<_>>(),
        vec![(id, &shown)]
    );
}

#[test]
fn a_grant_stored_with_no_label_shown_has_none_and_is_stored_the_same_way_again() {
    // A grant as it was stored before a grant could say what it showed.
    let stored = serde_json::json!({
        "holder": {"tool": "morning-pages"},
        "resource": {"data": ["writing/journal.md"]},
        "rights": ["read", "write"],
        "limits": {},
        "reason": "Keeps your morning pages.",
    });

    let grant: Grant = serde_json::from_value(stored.clone()).expect("read");
    assert_eq!(grant, writing_grant());
    assert_eq!(grant.shows, None);
    assert_eq!(serde_json::to_value(&grant).expect("written"), stored);
}

#[test]
fn authority_is_held_only_by_the_holder_it_was_recorded_for() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    store.grant(writing_grant(), NOW).expect("recorded");
    store.add_rule(sync_rule(), NOW).expect("recorded");

    // A standing task, a workbench, and installed software with the Tool's
    // name are not the Tool.
    for other in [
        Holder::Tool("evening-pages".to_string()),
        Holder::StandingTask("morning-pages".to_string()),
        Holder::Workbench("morning-pages".to_string()),
        Holder::Installed("morning-pages".to_string()),
    ] {
        assert_eq!(
            store.authority().expect("readable").grants(&other).count(),
            0,
            "{other}"
        );
        assert_eq!(store.authority().expect("readable").rules(&other).count(), 0, "{other}");
    }
}

fn provider() -> Party {
    Party::ModelProvider("claude".to_string())
}

fn bank() -> Party {
    Party::Site(Site::new("bank.example"))
}

#[test]
fn a_party_with_no_recorded_clearance_has_the_default_for_its_kind() {
    let dir = Scratch::new();
    drop(AuthorityStore::create(dir.path()).expect("made"));
    let authority = Authority::load(dir.path()).expect("loaded");

    // The glossary: a model provider's default is personal; a web site's is
    // public.
    assert_eq!(authority.clearance(&provider()), Label::Personal);
    assert_eq!(authority.clearance(&bank()), Label::Public);
}

#[test]
fn the_clearance_the_person_set_last_is_the_one_that_stands() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    store
        .set_clearance(provider(), Label::private(Category::Finance), NOW)
        .expect("raised");
    store.set_clearance(provider(), Label::Public, LATER).expect("lowered");
    store
        .set_clearance(bank(), Label::private(Category::Finance), LATER)
        .expect("raised");
    drop(store);

    // A clearance lowered below the default stays lowered: it is a record,
    // not the lack of one.
    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(authority.clearance(&provider()), Label::Public);
    assert_eq!(authority.clearance(&bank()), Label::private(Category::Finance));
    // Another site, and another provider, are still at their defaults.
    assert_eq!(
        authority.clearance(&Party::Site(Site::new("hostile.example"))),
        Label::Public
    );
    assert_eq!(
        authority.clearance(&Party::ModelProvider("other".to_string())),
        Label::Personal
    );
}

#[test]
fn there_is_no_ceiling_until_the_person_sets_one_and_then_the_last_one_stands() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    assert_eq!(store.authority().expect("readable").ceiling(), None);

    store.set_ceiling(EffectLimits::new().count(500), NOW).expect("set");
    store
        .set_ceiling(EffectLimits::new().count(50), LATER)
        .expect("lowered");
    drop(store);

    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(authority.ceiling(), Some(&EffectLimits::new().count(50)));
}

#[test]
fn the_history_keeps_every_change_in_order_with_its_time() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    let grant = store.grant(writing_grant(), NOW).expect("recorded");
    store.set_clearance(provider(), Label::Public, NOW).expect("set");
    store.revoke(grant, LATER).expect("revoked");
    drop(store);

    let authority = Authority::load(dir.path()).expect("loaded");
    let history: Vec<_> = authority
        .history()
        .iter()
        .map(|recorded| (recorded.seq, recorded.time, &recorded.entry))
        .collect();
    assert_eq!(
        history,
        vec![
            (1, NOW, &Change::Granted(writing_grant())),
            (
                2,
                NOW,
                &Change::ClearanceSet {
                    party: provider(),
                    clearance: Label::Public
                }
            ),
            (3, LATER, &Change::Revoked(grant)),
        ]
    );
}

#[test]
fn a_missing_record_is_refused_and_not_read_as_an_empty_one() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    store.set_clearance(provider(), Label::Public, NOW).expect("lowered");
    drop(store);

    // The record goes missing. Read as empty, it would put the provider
    // back at its default, above what the person set.
    std::fs::remove_file(dir.path().join("changes")).expect("removed");

    for refused in [
        Authority::load(dir.path()).map(|_| ()).unwrap_err(),
        AuthorityStore::open(dir.path()).map(|_| ()).unwrap_err(),
    ] {
        assert!(matches!(refused, Error::Missing { .. }), "{refused}");
    }
    // Opening did not make one in its place.
    assert!(!dir.path().join("changes").exists());
}

#[test]
fn a_record_is_made_once_and_making_it_again_is_refused() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    let id = store.grant(writing_grant(), NOW).expect("recorded");
    drop(store);

    let refused = AuthorityStore::create(dir.path()).map(|_| ()).unwrap_err();
    assert!(matches!(refused, Error::AlreadyThere { .. }), "{refused}");

    let authority = Authority::load(dir.path()).expect("the record is as it was");
    assert_eq!(
        authority.grants(&morning_pages()).collect::<Vec<_>>(),
        vec![(id, &writing_grant())]
    );
}

#[test]
fn an_id_that_names_nothing_is_not_read_as_a_grant_that_is_held() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    let id = store.grant(writing_grant(), NOW).expect("recorded");
    let other = Scratch::new();
    let mut elsewhere = AuthorityStore::create(other.path()).expect("made");
    elsewhere.grant(writing_grant(), NOW).expect("recorded");
    let unknown = elsewhere.grant(writing_grant(), NOW).expect("recorded");

    let authority = store.authority().expect("readable");
    assert_eq!(authority.status(id), Some(Status::Held));
    assert_eq!(authority.status(unknown), None);
}

#[test]
fn authority_made_in_memory_holds_what_the_same_changes_hold_when_recorded() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::create(dir.path()).expect("made");
    let grant = store.grant(writing_grant(), NOW).expect("recorded");
    let rule = store.add_rule(sync_rule(), NOW).expect("recorded");
    store.revoke(grant, LATER).expect("revoked");
    let recorded = store.authority().expect("readable");

    let in_memory = Authority::from_changes([
        (NOW, Change::Granted(writing_grant())),
        (NOW, Change::RuleAdded(sync_rule())),
        (LATER, Change::Revoked(grant)),
    ])
    .expect("admitted");

    assert_eq!(in_memory.history(), recorded.history());
    assert_eq!(in_memory.status(grant), Some(Status::Revoked(LATER)));
    assert_eq!(
        in_memory.rules(&morning_pages()).collect::<Vec<_>>(),
        vec![(rule, &sync_rule())]
    );
}

#[test]
fn authority_made_in_memory_refuses_a_change_a_record_would_refuse() {
    let unrated = Rule {
        limits: EffectLimits::new(),
        ..sync_rule()
    };
    assert!(matches!(
        Authority::from_changes([(NOW, Change::RuleAdded(unrated))]),
        Err(Error::RuleWithoutRate)
    ));
}
