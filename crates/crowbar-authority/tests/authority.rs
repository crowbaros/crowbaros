//! The authority domain records what the person granted, in a directory of
//! its own. What it holds is what was recorded there, and nothing else
//! changes it.

use crowbar_authority::{Authority, AuthorityStore, Change, Error, Holder, Party, Rule};
use crowbar_broker::EffectLimits;
use crowbar_labels::{Category, Label, Site};

mod common;
use common::{LATER, NOW, Scratch, morning_pages, sync_rule, writing_grant};

#[test]
fn a_grant_is_still_held_after_the_store_is_opened_again() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    let id = store.grant(writing_grant(), NOW).expect("recorded");
    drop(store);

    let authority = Authority::load(dir.path()).expect("loaded");
    let held: Vec<_> = authority.grants(&morning_pages()).collect();
    assert_eq!(held, vec![(id, &writing_grant())]);
}

#[test]
fn a_revoked_grant_is_not_held_now_or_after_the_store_is_opened_again() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    let id = store.grant(writing_grant(), NOW).expect("recorded");

    store.revoke(id, NOW).expect("revoked");
    assert_eq!(store.authority().grants(&morning_pages()).count(), 0);
    drop(store);

    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(authority.grants(&morning_pages()).count(), 0);
}

#[test]
fn granting_again_after_a_revocation_is_a_new_grant_and_the_old_one_stays_revoked() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    let first = store.grant(writing_grant(), NOW).expect("recorded");
    store.revoke(first, NOW).expect("revoked");

    let second = store.grant(writing_grant(), LATER).expect("recorded");
    assert_ne!(first, second);

    let authority = store.authority();
    assert_eq!(
        authority.grants(&morning_pages()).collect::<Vec<_>>(),
        vec![(second, &writing_grant())]
    );
    assert_eq!(authority.revoked_at(first), Some(NOW));
    assert_eq!(authority.revoked_at(second), None);
}

#[test]
fn revoking_an_id_that_names_nothing_is_refused_and_records_nothing() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    let id = store.grant(writing_grant(), NOW).expect("recorded");
    drop(store);

    // A second directory gives an id this store never recorded.
    let other = Scratch::new();
    let mut elsewhere = AuthorityStore::open(other.path()).expect("opened");
    elsewhere.grant(writing_grant(), NOW).expect("recorded");
    let unknown = elsewhere.grant(writing_grant(), NOW).expect("recorded");

    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    let refused = store.revoke(unknown, NOW).unwrap_err();
    assert!(
        matches!(refused, Error::NotStanding(named) if named == unknown),
        "{refused}"
    );
    assert_eq!(store.authority().history().len(), 1);
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
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    let id = store.grant(writing_grant(), NOW).expect("recorded");
    store.revoke(id, NOW).expect("revoked");

    store.revoke(id, LATER).expect("already revoked");
    assert_eq!(store.authority().revoked_at(id), Some(NOW));
    assert_eq!(store.authority().history().len(), 2);
}

#[test]
fn a_standing_rule_is_held_until_it_is_revoked() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    let grant = store.grant(writing_grant(), NOW).expect("recorded");
    let rule = store.rule(sync_rule(), NOW).expect("recorded");
    assert_eq!(
        store.authority().rules(&morning_pages()).collect::<Vec<_>>(),
        vec![(rule, &sync_rule())]
    );

    store.revoke(rule, LATER).expect("revoked");
    drop(store);

    // The rule is gone and the grant is not: each is revoked on its own.
    let authority = Authority::load(dir.path()).expect("loaded");
    assert_eq!(authority.rules(&morning_pages()).count(), 0);
    assert_eq!(authority.revoked_at(rule), Some(LATER));
    assert_eq!(
        authority.grants(&morning_pages()).collect::<Vec<_>>(),
        vec![(grant, &writing_grant())]
    );
}

#[test]
fn a_standing_rule_with_no_rate_limit_is_refused_and_records_nothing() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    let unlimited = Rule {
        limits: EffectLimits::new().count(5),
        ..sync_rule()
    };

    let refused = store.rule(unlimited, NOW).unwrap_err();
    assert!(matches!(refused, Error::RuleWithoutRate), "{refused}");
    assert_eq!(store.authority().rules(&morning_pages()).count(), 0);
    assert!(store.authority().history().is_empty());
}

#[test]
fn authority_is_held_only_by_the_holder_it_was_recorded_for() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    store.grant(writing_grant(), NOW).expect("recorded");
    store.rule(sync_rule(), NOW).expect("recorded");

    // A standing task and a workbench with the Tool's name are not the Tool.
    for other in [
        Holder::Tool("evening-pages".to_string()),
        Holder::StandingTask("morning-pages".to_string()),
        Holder::Workbench("morning-pages".to_string()),
    ] {
        assert_eq!(store.authority().grants(&other).count(), 0, "{other}");
        assert_eq!(store.authority().rules(&other).count(), 0, "{other}");
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
    let authority = Authority::load(dir.path()).expect("loaded");

    // The glossary: a model provider's default is personal; a web site's is
    // public.
    assert_eq!(authority.clearance(&provider()), Label::Personal);
    assert_eq!(authority.clearance(&bank()), Label::Public);
}

#[test]
fn the_clearance_the_person_set_last_is_the_one_that_stands() {
    let dir = Scratch::new();
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
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
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
    assert_eq!(store.authority().ceiling(), None);

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
    let mut store = AuthorityStore::open(dir.path()).expect("opened");
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
