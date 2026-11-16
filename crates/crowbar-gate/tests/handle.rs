//! The handle comes first: the gate refuses a handle it does not know, one
//! the caller does not hold, one that has ended or run out of time, and a
//! right it does not carry. What it does not know, it refuses. A holder
//! passes a handle on only narrowed, and never to the Agent.

use crowbar_capability::{EffectLimits, HandleId, Principal, Right, Rights};
use crowbar_gate::{Act, Ending, GateRule, Limit, Original};
use crowbar_labels::Compartment;

mod common;
use common::{Bench, NOW, allowed, at, by, handle, reading, refused, task, tool, using};

#[test]
fn a_live_handle_its_holder_uses_within_its_rights_is_allowed() {
    let mut bench = Bench::new();
    let holder = task("sort-photos");
    let held = handle(1, holder.clone());
    let compartment = Compartment::fresh();

    let read = reading(1);
    let decision = bench.decide(
        &by(
            &holder,
            &compartment,
            Act::Use {
                handle: Some(&held),
                using: &read,
            },
        ),
        NOW,
    );
    assert_eq!(decision, allowed());
}

#[test]
fn a_handle_the_broker_did_not_issue_is_refused() {
    let mut bench = Bench::new();
    let caller = task("sort-photos");
    let compartment = Compartment::fresh();

    let read = reading(1);
    let decision = bench.decide(
        &by(
            &caller,
            &compartment,
            Act::Use {
                handle: None,
                using: &read,
            },
        ),
        NOW,
    );
    assert_eq!(decision, refused(GateRule::Unknown, Limit::None));
}

#[test]
fn a_caller_that_is_not_the_holder_is_refused_and_learns_nothing_of_the_handles_state() {
    let mut bench = Bench::new();
    let mut held = handle(1, task("sort-photos"));
    held.ended = Some(Ending::Revoked);
    let stranger = task("someone-else");
    let compartment = Compartment::fresh();

    let read = reading(1);
    let decision = bench.decide(
        &by(
            &stranger,
            &compartment,
            Act::Use {
                handle: Some(&held),
                using: &read,
            },
        ),
        NOW,
    );
    // Not "revoked": the stranger is told only that the handle is not theirs.
    assert_eq!(decision, refused(GateRule::Holder, Limit::None));
}

#[test]
fn a_revoked_handle_an_ended_holder_and_an_ended_original_are_refused() {
    let mut bench = Bench::new();
    let holder = task("sort-photos");
    let compartment = Compartment::fresh();
    let read = reading(1);

    let mut revoked = handle(1, holder.clone());
    revoked.ended = Some(Ending::Revoked);
    let mut holder_ended = handle(2, holder.clone());
    holder_ended.ended = Some(Ending::HolderEnded);
    let mut copy = handle(3, holder.clone());
    copy.original = Some(Original {
        id: HandleId::from_bits(4),
        limits: EffectLimits::new(),
        ended: true,
    });

    for (held, rule) in [
        (&revoked, GateRule::Revoked),
        (&holder_ended, GateRule::Ended),
        (&copy, GateRule::Ended),
    ] {
        let decision = bench.decide(
            &by(
                &holder,
                &compartment,
                Act::Use {
                    handle: Some(held),
                    using: &read,
                },
            ),
            NOW,
        );
        assert_eq!(decision, refused(rule, Limit::None));
    }
}

#[test]
fn a_handle_is_refused_from_the_moment_its_time_runs_out() {
    let mut bench = Bench::new();
    let holder = task("sort-photos");
    let mut held = handle(1, holder.clone());
    held.limits = EffectLimits::new().until(at(10_060));
    let compartment = Compartment::fresh();
    let read = reading(1);
    let operation = by(
        &holder,
        &compartment,
        Act::Use {
            handle: Some(&held),
            using: &read,
        },
    );

    assert_eq!(bench.decide(&operation, at(10_059)), allowed());
    assert_eq!(
        bench.decide(&operation, at(10_060)),
        refused(GateRule::Expired, Limit::Until(at(10_060)))
    );
}

#[test]
fn a_right_the_handle_does_not_carry_is_refused_and_named() {
    let mut bench = Bench::new();
    let holder = task("sort-photos");
    let held = handle(1, holder.clone());
    let compartment = Compartment::fresh();

    let create = using("data.create", Right::Create);
    let decision = bench.decide(
        &by(
            &holder,
            &compartment,
            Act::Use {
                handle: Some(&held),
                using: &create,
            },
        ),
        NOW,
    );
    assert_eq!(decision, refused(GateRule::Right, Limit::Right(Right::Create)));
}

#[test]
fn an_unknown_principal_or_operation_is_refused() {
    let mut bench = Bench::new();
    let nameless = task("");
    let held = handle(1, nameless.clone());
    let compartment = Compartment::fresh();
    let read = reading(1);
    let decision = bench.decide(
        &by(
            &nameless,
            &compartment,
            Act::Use {
                handle: Some(&held),
                using: &read,
            },
        ),
        NOW,
    );
    assert_eq!(decision, refused(GateRule::Unknown, Limit::None));

    let holder = task("sort-photos");
    let held = handle(2, holder.clone());
    for operation in ["", "read", "data.", ".read", "Data.Read", "data read", "data.read\n"] {
        let named = using(operation, Right::Read);
        let decision = bench.decide(
            &by(
                &holder,
                &compartment,
                Act::Use {
                    handle: Some(&held),
                    using: &named,
                },
            ),
            NOW,
        );
        assert_eq!(decision, refused(GateRule::Unknown, Limit::None), "{operation:?}");
    }
}

#[test]
fn the_first_failing_check_decides() {
    let mut bench = Bench::new();
    let holder = task("sort-photos");
    // Expired, and asked for a right it does not carry: the handle comes
    // first, so the refusal says nothing of the right.
    let mut held = handle(1, holder.clone());
    held.limits = EffectLimits::new().until(NOW);
    let compartment = Compartment::fresh();

    let create = using("data.create", Right::Create);
    let decision = bench.decide(
        &by(
            &holder,
            &compartment,
            Act::Use {
                handle: Some(&held),
                using: &create,
            },
        ),
        NOW,
    );
    assert_eq!(decision, refused(GateRule::Expired, Limit::Until(NOW)));
}

/// `holder` passes `held` to `to` with these rights and limits.
fn pass(
    bench: &mut Bench,
    holder: &Principal,
    held: &crowbar_gate::Presented,
    to: &Principal,
    rights: Rights,
    limits: EffectLimits,
) -> crowbar_gate::Decision {
    let compartment = Compartment::fresh();
    bench.decide(
        &by(
            holder,
            &compartment,
            Act::Delegate {
                handle: Some(held),
                to,
                rights: &rights,
                limits: &limits,
            },
        ),
        NOW,
    )
}

#[test]
fn a_delegable_handle_passes_on_narrowed() {
    let mut bench = Bench::new();
    let holder = task("sort-photos");
    let mut held = handle(1, holder.clone());
    held.delegable = true;
    held.limits = EffectLimits::new().count(10);

    let decision = pass(
        &mut bench,
        &holder,
        &held,
        &tool("photo-sorter"),
        Rights::from([Right::Read]),
        EffectLimits::new().count(5),
    );
    assert_eq!(decision, allowed());
}

#[test]
fn a_handle_passes_on_only_from_its_holder_only_if_delegable_and_never_to_the_agent() {
    let mut bench = Bench::new();
    let holder = task("sort-photos");
    let read_only = || Rights::from([Right::Read]);
    let mut delegable = handle(1, holder.clone());
    delegable.delegable = true;

    assert_eq!(
        pass(
            &mut bench,
            &task("someone-else"),
            &delegable,
            &tool("photo-sorter"),
            read_only(),
            EffectLimits::new()
        ),
        refused(GateRule::Holder, Limit::None)
    );
    assert_eq!(
        pass(
            &mut bench,
            &holder,
            &handle(2, holder.clone()),
            &tool("photo-sorter"),
            read_only(),
            EffectLimits::new()
        ),
        refused(GateRule::NotDelegable, Limit::None)
    );
    // A copy is never passed on again.
    let mut copy = delegable.clone();
    copy.id = HandleId::from_bits(3);
    copy.original = Some(Original {
        id: delegable.id,
        limits: EffectLimits::new(),
        ended: false,
    });
    assert_eq!(
        pass(
            &mut bench,
            &holder,
            &copy,
            &tool("photo-sorter"),
            read_only(),
            EffectLimits::new()
        ),
        refused(GateRule::NotDelegable, Limit::None)
    );
    assert_eq!(
        pass(
            &mut bench,
            &holder,
            &delegable,
            &Principal::Agent,
            read_only(),
            EffectLimits::new()
        ),
        refused(GateRule::ToAgent, Limit::None)
    );
    assert_eq!(
        pass(
            &mut bench,
            &holder,
            &delegable,
            &tool(""),
            read_only(),
            EffectLimits::new()
        ),
        refused(GateRule::Unknown, Limit::None)
    );
}

#[test]
fn a_copy_that_would_widen_its_original_is_refused_naming_the_originals_limit() {
    let mut bench = Bench::new();
    let holder = task("sort-photos");
    let mut held = handle(1, holder.clone());
    held.delegable = true;
    held.limits = EffectLimits::new().count(10).until(at(20_000));
    let to = tool("photo-sorter");

    assert_eq!(
        pass(
            &mut bench,
            &holder,
            &held,
            &to,
            Rights::from([Right::Read, Right::Exec]),
            held.limits.clone()
        ),
        refused(GateRule::Right, Limit::None)
    );
    assert_eq!(
        pass(
            &mut bench,
            &holder,
            &held,
            &to,
            Rights::from([Right::Read]),
            EffectLimits::new().count(11).until(at(20_000))
        ),
        refused(GateRule::Limits, Limit::Count(10))
    );
    // No end is later than any end.
    assert_eq!(
        pass(
            &mut bench,
            &holder,
            &held,
            &to,
            Rights::from([Right::Read]),
            EffectLimits::new().count(10)
        ),
        refused(GateRule::Limits, Limit::Until(at(20_000)))
    );
}
