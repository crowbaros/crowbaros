//! A task attaches an idempotency key so an operation is decided and
//! carried out once. A repeat gets the first decision again, counts nothing,
//! and carries no `Allowed`. The same key on a different operation is
//! refused. Each task has its own keys.

use crowbar_capability::{EffectLimits, IdempotencyKey, Principal, Right};
use crowbar_gate::{Act, AskWhy, Caller, Decision, GateRule, Limit, Operation, Presented, Ruling, Use};
use crowbar_labels::Compartment;

mod common;
use common::{Bench, NOW, allowed, asked, handle, later, reading, refused, task, using};

fn keyed<'a>(
    caller: &'a Principal,
    compartment: &'a Compartment,
    key: &'a IdempotencyKey,
    held: &'a Presented,
    what: &'a Use,
) -> Operation<'a> {
    Operation {
        caller: Caller {
            principal: caller,
            standing_task: None,
            holds_login: false,
            compartment,
        },
        key: Some(key),
        act: Act::Use {
            handle: Some(held),
            using: what,
        },
    }
}

#[test]
fn a_repeated_key_gets_the_first_decision_and_counts_nothing() {
    let mut bench = Bench::new();
    let caller = task("sort-photos");
    let mut held = handle(1, caller.clone());
    held.limits = EffectLimits::new().count(2);
    let compartment = Compartment::fresh();
    let key = IdempotencyKey::new("move-1");
    let read = reading(2);
    let operation = keyed(&caller, &compartment, &key, &held, &read);

    assert_eq!(bench.decide(&operation, NOW), allowed());
    // Without the key, this would be past the count of two.
    assert_eq!(bench.decide(&operation, later(1)), allowed());
    assert!(matches!(
        bench.gate.decide(bench.authority.as_ref(), &operation, later(2)),
        Ruling::Repeated(Decision::Allowed { .. })
    ));
    assert_eq!(bench.gate.ledger().entries().len(), 1);
}

#[test]
fn a_repeated_ask_is_the_same_ask_and_opens_no_other() {
    let mut bench = Bench::new();
    let caller = task("reply-to-landlord");
    let held = handle(1, caller.clone());
    let compartment = Compartment::fresh();
    let key = IdempotencyKey::new("send-1");
    let send = using("mail.send", Right::Read);
    let operation = keyed(&caller, &compartment, &key, &held, &send);

    assert_eq!(bench.decide(&operation, NOW), asked(AskWhy::Crosses));
    assert_eq!(bench.decide(&operation, later(1)), asked(AskWhy::Crosses));
}

#[test]
fn a_repeated_refusal_stays_a_refusal() {
    let mut bench = Bench::new();
    let caller = task("sort-photos");
    let mut held = handle(1, caller.clone());
    held.limits = EffectLimits::new().count(1);
    let compartment = Compartment::fresh();
    let key = IdempotencyKey::new("move-1");
    let read = reading(2);
    let operation = keyed(&caller, &compartment, &key, &held, &read);

    assert_eq!(
        bench.decide(&operation, NOW),
        refused(GateRule::Limits, Limit::Count(1))
    );
    assert_eq!(
        bench.decide(&operation, later(1)),
        refused(GateRule::Limits, Limit::Count(1))
    );
}

#[test]
fn a_key_reused_on_a_different_operation_is_refused() {
    let mut bench = Bench::new();
    let caller = task("sort-photos");
    let held = handle(1, caller.clone());
    let compartment = Compartment::fresh();
    let key = IdempotencyKey::new("move-1");
    let one = reading(1);
    let two = reading(2);

    assert_eq!(
        bench.decide(&keyed(&caller, &compartment, &key, &held, &one), NOW),
        allowed()
    );
    assert_eq!(
        bench.decide(&keyed(&caller, &compartment, &key, &held, &two), NOW),
        refused(GateRule::Idempotency, Limit::None)
    );
    // The refusal does not replace the first decision.
    assert_eq!(
        bench.decide(&keyed(&caller, &compartment, &key, &held, &one), NOW),
        allowed()
    );
}

#[test]
fn each_task_has_its_own_keys() {
    let mut bench = Bench::new();
    let compartment = Compartment::fresh();
    let key = IdempotencyKey::new("move-1");
    let read = reading(1);
    let first = task("sort-photos");
    let second = task("sort-more-photos");
    let first_handle = handle(1, first.clone());
    let second_handle = handle(2, second.clone());

    assert_eq!(
        bench.decide(&keyed(&first, &compartment, &key, &first_handle, &read), NOW),
        allowed()
    );
    assert!(matches!(
        bench.gate.decide(
            bench.authority.as_ref(),
            &keyed(&second, &compartment, &key, &second_handle, &read),
            NOW
        ),
        Ruling::Allowed(_)
    ));
}

#[test]
fn an_operation_without_a_key_is_decided_and_counted_each_time() {
    let mut bench = Bench::new();
    let caller = task("sort-photos");
    let mut held = handle(1, caller.clone());
    held.limits = EffectLimits::new().count(2);
    let compartment = Compartment::fresh();
    let read = reading(1);
    let operation = common::by(
        &caller,
        &compartment,
        Act::Use {
            handle: Some(&held),
            using: &read,
        },
    );

    assert_eq!(bench.decide(&operation, NOW), allowed());
    assert_eq!(bench.decide(&operation, NOW), allowed());
    assert_eq!(
        bench.decide(&operation, NOW),
        refused(GateRule::Limits, Limit::Count(2))
    );
}
