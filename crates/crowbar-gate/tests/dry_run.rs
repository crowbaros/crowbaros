//! A dry run gives the decision the real operation would get, and changes
//! nothing: it counts no use, opens no ask, records no idempotency key, and
//! adds nothing to the ledger. Every test of the gate's decisions also runs
//! as a dry run (see `common::Bench::decide`); these state what a dry run
//! must not do, one thing at a time.

use crowbar_capability::{EffectLimits, IdempotencyKey, Right};
use crowbar_gate::{Act, AskWhy, Caller, Operation, Ruling, Use, UseCost};
use crowbar_labels::Compartment;

mod common;
use common::{Bench, NOW, allowed, asked, asked_plainly, by, handle, reading, task, using};

#[test]
fn a_dry_run_counts_no_use() {
    let bench = Bench::new();
    let mut gate = bench.gate;
    let caller = task("sort-photos");
    let mut held = handle(1, caller.clone());
    held.limits = EffectLimits::new().count(1);
    let compartment = Compartment::fresh();
    let read = reading(1);
    let operation = by(
        &caller,
        &compartment,
        Act::Use {
            handle: Some(&held),
            using: &read,
        },
    );

    for _ in 0..5 {
        assert_eq!(gate.dry_run(bench.authority.as_ref(), &operation, NOW), allowed());
    }
    assert!(matches!(
        gate.decide(bench.authority.as_ref(), &operation, NOW),
        Ruling::Allowed(_)
    ));
}

#[test]
fn a_dry_run_opens_no_ask() {
    let bench = Bench::new();
    let mut gate = bench.gate;
    let caller = task("reply-to-landlord");
    let held = handle(1, caller.clone());
    let compartment = Compartment::fresh();
    let send = using("mail.send", Right::Read);
    let operation = by(
        &caller,
        &compartment,
        Act::Use {
            handle: Some(&held),
            using: &send,
        },
    );

    for _ in 0..20 {
        assert_eq!(
            gate.dry_run(bench.authority.as_ref(), &operation, NOW),
            asked(AskWhy::Crosses)
        );
    }
    assert!(matches!(
        gate.decide(bench.authority.as_ref(), &operation, NOW),
        Ruling::Asked(_)
    ));
    assert!(gate.ledger().entries().len() == 1);
}

#[test]
fn a_dry_run_records_no_idempotency_key() {
    let bench = Bench::new();
    let mut gate = bench.gate;
    let caller = task("sort-photos");
    let held = handle(1, caller.clone());
    let compartment = Compartment::fresh();
    let key = IdempotencyKey::new("move-1");
    let one = reading(1);
    let two = reading(2);
    let keyed = |what| Operation {
        caller: Caller {
            principal: &caller,
            standing_task: None,
            compartment: &compartment,
        },
        key: Some(&key),
        act: Act::Use {
            handle: Some(&held),
            using: what,
        },
    };

    assert_eq!(gate.dry_run(bench.authority.as_ref(), &keyed(&one), NOW), allowed());
    // The key is still free: the operation it is first used on is this one.
    assert!(matches!(
        gate.decide(bench.authority.as_ref(), &keyed(&two), NOW),
        Ruling::Allowed(_)
    ));
}

#[test]
fn a_dry_run_of_an_act_that_depends_on_a_value_says_only_that_it_asks() {
    // Attack A19: the dry run is no way to learn a value. The decision is
    // the same whatever the value is, because the gate never sees it.
    let mut bench = Bench::new();
    let caller = task("pay-bills");
    let held = handle(1, caller.clone());
    let compartment = Compartment::fresh();
    let pay = Use {
        cost: UseCost::DependsOnValue,
        ..using("data.write", Right::Write)
    };
    let operation = by(
        &caller,
        &compartment,
        Act::Use {
            handle: Some(&held),
            using: &pay,
        },
    );

    assert_eq!(bench.decide(&operation, NOW), asked_plainly(AskWhy::DependsOnValue));
}
