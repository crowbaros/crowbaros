//! A standing rule with a cap lets an act whose cost depends on a value go
//! ahead without an ask. The gate never reads the amount: it answers
//! "allowed if within the cap", and the adapter checks the amount against
//! the cap when it carries the act out. Over the cap nothing is carried out,
//! and the act asks (ADR 0021, attack A30).

use crowbar_authority::{Authority, Change, Event, Grant, Holder, Rule, StandingId};
use crowbar_capability::{Cost, EffectLimits, Principal, Rate, Resource, Right, Rights};
use crowbar_gate::{
    Act, Allowed, Answer, Ask, AskWhy, Caller, Decision, GateRule, Held, Operation, Presented, Refuse, Ruling, Use,
    UseCost,
};
use crowbar_labels::{Compartment, Fetch, MarkSource, Origin, Site};

mod common;
use common::{Bench, NOW, asked, asked_but_held, by, handle, task, using};

/// Thirty days, in seconds.
const MONTH: u64 = 2_592_000;

/// The cap of the standing rule: $150.
const CAP: Cost = Cost::cents(15_000);

/// The standing task the person set up: "pay the electric bill".
const STANDING_TASK: &str = "pay-the-electric-bill";

fn bill_payer() -> Holder {
    Holder::StandingTask(STANDING_TASK.to_string())
}

/// The person's account with the utility, through its adapter.
fn utility() -> Resource {
    Resource::Service("utility".to_string())
}

/// "Pay the electric bill may use your account with the utility."
fn grant() -> Grant {
    Grant {
        holder: bill_payer(),
        resource: utility(),
        rights: Rights::from([Right::Write]),
        limits: EffectLimits::new(),
        reason: "Pays your electric bill.".to_string(),
    }
}

/// "Pay the electric bill, up to $150, at most once a month."
fn rule() -> Rule {
    Rule {
        holder: bill_payer(),
        operation: "bills.pay".to_string(),
        limits: EffectLimits::new().rate(Rate::per(1, MONTH)),
        sources: [].into(),
        cap: Some(CAP),
    }
}

/// The handle one run of the standing task holds on the account.
fn on_the_account(run: &Principal) -> Presented {
    Presented {
        resource: utility(),
        ..handle(1, run.clone())
    }
}

/// Paying the bill. The amount is in the bill, which the task cannot read,
/// so nothing here states it.
fn pay() -> Use {
    Use {
        cost: UseCost::DependsOnValue,
        ..using("bills.pay", Right::Write)
    }
}

/// `run`, a run of the standing task, asking to pay the bill.
fn paying<'a>(run: &'a Principal, compartment: &'a Compartment, held: &'a Presented, pay: &'a Use) -> Operation<'a> {
    Operation {
        caller: Caller {
            principal: run,
            standing_task: Some(STANDING_TASK),
            holds_login: false,
            compartment,
        },
        key: None,
        act: Act::Use {
            handle: Some(held),
            using: pay,
        },
    }
}

/// The ids of the grant and the rule the standing task holds, in that order.
fn standing(authority: &Authority) -> Vec<StandingId> {
    let grants = authority.grants(&bill_payer()).map(|(id, _)| id);
    grants.chain(authority.rules(&bill_payer()).map(|(id, _)| id)).collect()
}

#[test]
fn under_a_rule_with_a_cap_an_act_whose_cost_depends_on_a_value_is_allowed_if_within_the_cap() {
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(rule())]);
    let under = standing(bench.authority.as_ref().expect("read"));
    assert_eq!(under.len(), 2);
    let run = task("run-41");
    let held = on_the_account(&run);

    assert_eq!(
        bench.decide(&paying(&run, &Compartment::fresh(), &held, &pay()), NOW),
        Decision::AllowedWithinCap { under, cap: CAP }
    );
}

#[test]
fn under_a_rule_with_no_cap_the_same_act_still_asks() {
    let uncapped = Rule { cap: None, ..rule() };
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(uncapped)]);
    let run = task("run-41");
    let held = on_the_account(&run);

    assert_eq!(
        bench.decide(&paying(&run, &Compartment::fresh(), &held, &pay()), NOW),
        asked(AskWhy::DependsOnValue)
    );
}

#[test]
fn a_cap_fires_only_for_the_holder_of_the_rule() {
    // An ad hoc task holds no standing authority, whatever it is named.
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(rule())]);
    let ad_hoc = task(STANDING_TASK);
    let held = on_the_account(&ad_hoc);
    let pay = pay();
    let act = Act::Use {
        handle: Some(&held),
        using: &pay,
    };

    assert_eq!(
        bench.decide(&by(&ad_hoc, &Compartment::fresh(), act), NOW),
        asked(AskWhy::DependsOnValue)
    );
}

#[test]
fn a_cap_fires_for_a_marked_act_only_under_a_rule_for_every_source_that_marked_it() {
    let news = Origin::WebPage(Fetch::WithoutLogin(Site::new("news.example"))).label_mark();
    let mut marked = Compartment::fresh();
    marked.read(&news).expect("a task's compartment grows");
    let run = task("run-41");
    let held = on_the_account(&run);

    let mut for_unmarked_work = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(rule())]);
    assert_eq!(
        for_unmarked_work.decide(&paying(&run, &marked, &held, &pay()), NOW),
        asked(AskWhy::DependsOnValue)
    );

    let for_news = Rule {
        sources: [MarkSource::Site(Site::new("news.example"))].into(),
        ..rule()
    };
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(for_news)]);
    let under = standing(bench.authority.as_ref().expect("read"));
    assert_eq!(
        bench.decide(&paying(&run, &marked, &held, &pay()), NOW),
        Decision::AllowedWithinCap { under, cap: CAP }
    );
}

/// What the gate gives the broker for `operation`, which it allows.
fn allowed_for(bench: &mut Bench, operation: &Operation<'_>) -> Allowed {
    match bench.gate.decide(bench.authority.as_ref(), operation, NOW) {
        Ruling::Allowed(allowed) => allowed,
        other => panic!("not allowed: {other:?}"),
    }
}

#[test]
fn what_the_gate_gives_the_broker_carries_the_cap_and_covers_only_an_amount_within_it() {
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(rule())]);
    let run = task("run-41");
    let held = on_the_account(&run);

    let allowed = allowed_for(&mut bench, &paying(&run, &Compartment::fresh(), &held, &pay()));

    assert_eq!(allowed.cap(), Some(CAP));
    // The adapter reads the amount when it carries the act out.
    assert!(allowed.covers_use_costing(held.id, "bills.pay", Cost::cents(14_999)));
    assert!(allowed.covers_use_costing(held.id, "bills.pay", CAP));
    assert!(!allowed.covers_use_costing(held.id, "bills.pay", Cost::cents(15_001)));
    assert!(!allowed.covers_use_costing(held.id, "bills.refund", Cost::cents(1)));
    // Without an amount it covers nothing: an adapter that does not check
    // the amount carries nothing out.
    assert!(!allowed.covers_use(held.id, "bills.pay"));
}

#[test]
fn what_the_gate_gives_for_a_use_under_no_cap_covers_it_whatever_it_costs() {
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(rule())]);
    let run = task("run-41");
    let held = on_the_account(&run);
    // The adapter states this cost before the use, so the gate checked it.
    let fixed = Use {
        cost: UseCost::Fixed(Cost::cents(9_000)),
        ..pay()
    };

    let allowed = allowed_for(&mut bench, &paying(&run, &Compartment::fresh(), &held, &fixed));

    assert_eq!(allowed.cap(), None);
    assert!(allowed.covers_use(held.id, "bills.pay"));
    assert!(allowed.covers_use_costing(held.id, "bills.pay", Cost::cents(9_000)));
}

#[test]
fn over_the_cap_the_act_asks_and_the_ask_is_recorded_and_waits_like_any_other() {
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(rule())]);
    let run = task("run-41");
    let held = on_the_account(&run);
    let (compartment, pay) = (Compartment::fresh(), pay());
    let operation = paying(&run, &compartment, &held, &pay);
    let allowed = allowed_for(&mut bench, &operation);

    // The adapter read the amount, $151, and carries nothing out.
    assert!(!allowed.covers_use_costing(held.id, "bills.pay", Cost::cents(15_100)));
    let ask = bench.gate.over_cap(allowed, &operation, NOW).expect("recorded");

    // Paying a bill crosses the undo boundary, so the ask takes the confirm
    // gesture.
    assert_eq!(
        ask,
        Ask {
            why: AskWhy::OverCap,
            answer: Answer::ConfirmGesture,
            held: None
        }
    );
    // The ledger holds the decision, then the ask. Neither holds the amount.
    let entries = bench.gate.ledger().entries();
    assert_eq!(entries.len(), 2);
    let recorded = &entries[1].entry;
    assert_eq!(recorded.event, Event::Decided(Decision::Asked(ask)));
    assert_eq!(
        (&recorded.principal, recorded.operation.as_str(), recorded.handle),
        (&run, "bills.pay", Some(held.id))
    );
    assert_eq!(recorded.cost, None);
    // It waits: the run's next ask is held behind it.
    let send = using("mail.send", Right::Write);
    assert_eq!(
        bench.decide(&paying(&run, &compartment, &held, &send), NOW),
        asked_but_held(AskWhy::Crosses, Held::Waiting(1))
    );
}

#[test]
fn only_what_the_gate_gave_for_that_operation_with_a_cap_can_be_given_back_as_over_it() {
    let often = Rule {
        limits: EffectLimits::new().rate(Rate::per(10, MONTH)),
        ..rule()
    };
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(often)]);
    let run = task("run-41");
    let held = on_the_account(&run);
    let (compartment, pay) = (Compartment::fresh(), pay());
    let capped = paying(&run, &compartment, &held, &pay);
    let fixed = Use {
        cost: UseCost::Fixed(Cost::cents(9_000)),
        ..pay.clone()
    };
    let uncapped = paying(&run, &compartment, &held, &fixed);
    let refund = Use {
        operation: "bills.refund".to_string(),
        ..pay.clone()
    };
    let another = paying(&run, &compartment, &held, &refund);

    // One with no cap.
    let allowed = allowed_for(&mut bench, &uncapped);
    assert_eq!(
        bench.gate.over_cap(allowed, &uncapped, NOW),
        Err(Refuse::by(GateRule::Unknown))
    );
    // One given for another operation.
    let allowed = allowed_for(&mut bench, &capped);
    assert_eq!(
        bench.gate.over_cap(allowed, &another, NOW),
        Err(Refuse::by(GateRule::Unknown))
    );
    // Nothing was recorded but the two decisions.
    assert_eq!(bench.gate.ledger().entries().len(), 2);
}
