//! On egress, the compartment's whole label must be at or below the party's
//! clearance. The label is the broker's record of the compartment, never
//! the task's word. No party is cleared for `unknown` unless the person names
//! it, and a refusal here is final: only a release lets a value past.

use crowbar_authority::{Change, Party};
use crowbar_capability::{Resource, Right, Rights};
use crowbar_gate::{Act, AskWhy, GateRule, Limit, Presented};
use crowbar_labels::{Category, Compartment, Fetch, Label, LabelMark, Origin, Site};

mod common;
use common::{Bench, NOW, asked, by, handle, personal, refused, task, using};

fn provider() -> Party {
    Party::ModelProvider("provider.example".to_string())
}

fn site() -> Party {
    Party::Site(Site::new("news.example"))
}

/// A connection handle held by `task`.
fn connection(holder: crowbar_capability::Principal) -> Presented {
    Presented {
        resource: Resource::Endpoint {
            host: Site::new("news.example"),
            port: 443,
        },
        rights: Rights::from([Right::Connect]),
        ..handle(1, holder)
    }
}

/// A compartment that has read something at `label_mark`.
fn having_read(label_mark: &LabelMark) -> Compartment {
    let mut compartment = Compartment::fresh();
    compartment.read(label_mark).expect("a task's compartment grows");
    compartment
}

/// A page of the bank's site, read under a login: private to that site.
fn bank() -> LabelMark {
    Origin::WebPage(Fetch::UnderLogin(Site::new("bank.example"))).label_mark()
}

fn egress(bench: &mut Bench, compartment: &Compartment, party: &Party) -> crowbar_gate::Decision {
    let caller = task("research");
    let held = connection(caller.clone());
    let get = using("http.get", Right::Connect);
    bench.decide(
        &by(
            &caller,
            compartment,
            Act::Egress {
                handle: Some(&held),
                using: &get,
                party,
            },
        ),
        NOW,
    )
}

#[test]
fn a_compartment_at_or_below_the_partys_clearance_passes_the_clearance_check() {
    // An egress crosses, and an ad hoc task holds no standing rule, so it
    // asks: the clearance did not refuse it.
    assert_eq!(
        egress(&mut Bench::new(), &Compartment::fresh(), &site()),
        asked(AskWhy::Crosses)
    );
    assert_eq!(
        egress(&mut Bench::new(), &having_read(&personal()), &provider()),
        asked(AskWhy::Crosses)
    );
}

#[test]
fn a_compartment_above_the_partys_clearance_is_refused_naming_the_clearance() {
    let mut bench = Bench::new();
    assert_eq!(
        egress(&mut bench, &having_read(&personal()), &site()),
        refused(GateRule::Clearance, Limit::Clearance(Label::Public))
    );
    assert_eq!(
        egress(&mut bench, &having_read(&bank()), &provider()),
        refused(GateRule::Clearance, Limit::Clearance(Label::Personal))
    );
}

#[test]
fn a_clearance_the_person_set_replaces_the_default() {
    let cleared = Label::private(Category::Site(Site::new("bank.example")));
    let mut bench = Bench::with(vec![Change::ClearanceSet {
        party: provider(),
        clearance: cleared,
    }]);
    assert_eq!(
        egress(&mut bench, &having_read(&bank()), &provider()),
        asked(AskWhy::Crosses)
    );

    let mut lowered = Bench::with(vec![Change::ClearanceSet {
        party: provider(),
        clearance: Label::Public,
    }]);
    assert_eq!(
        egress(&mut lowered, &having_read(&personal()), &provider()),
        refused(GateRule::Clearance, Limit::Clearance(Label::Public))
    );
}

#[test]
fn no_party_is_cleared_for_unknown_unless_the_person_names_it() {
    let unknown = Origin::Unknown.label_mark();
    let mut bench = Bench::with(vec![Change::ClearanceSet {
        party: provider(),
        clearance: Label::private(Category::Messages),
    }]);
    assert_eq!(
        egress(&mut bench, &having_read(&unknown), &provider()),
        refused(
            GateRule::Clearance,
            Limit::Clearance(Label::private(Category::Messages))
        )
    );

    let mut named = Bench::with(vec![Change::ClearanceSet {
        party: provider(),
        clearance: Label::private(Category::Unknown),
    }]);
    assert_eq!(
        egress(&mut named, &having_read(&unknown), &provider()),
        asked(AskWhy::Crosses)
    );
}

#[test]
fn with_no_authority_to_read_no_party_is_cleared_above_public() {
    let mut bench = Bench::without_authority();
    // The default would clear the provider for personal; the person may have
    // lowered it, and there is no record to say.
    assert_eq!(
        egress(&mut bench, &having_read(&personal()), &provider()),
        refused(GateRule::Clearance, Limit::Clearance(Label::Public))
    );
    assert_eq!(
        egress(&mut bench, &Compartment::fresh(), &provider()),
        asked(AskWhy::NoAuthority)
    );
}
