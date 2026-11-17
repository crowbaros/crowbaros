//! Effect limits hold on every use: how many, at what cost, and how often.
//! A copy's uses count against its original too, so passing a handle on
//! never multiplies what it allows. A cost the gate cannot see asks.

use crowbar_capability::{Cost, EffectLimits, Rate, Right};
use crowbar_gate::{Act, AskWhy, GateRule, Limit, Original, Presented, Use, UseCost};
use crowbar_labels::Compartment;

mod common;
use common::{Bench, NOW, allowed, asked_plainly, by, handle, later, photo, reading, refused, task, using};

fn use_at(bench: &mut Bench, held: &Presented, what: &Use, now: crowbar_capability::Time) -> crowbar_gate::Decision {
    let compartment = Compartment::fresh();
    bench.decide(
        &by(
            &held.holder,
            &compartment,
            Act::Use {
                handle: Some(held),
                using: what,
            },
        ),
        now,
    )
}

fn costing(cents: u64) -> Use {
    Use {
        cost: UseCost::Fixed(Cost::cents(cents)),
        ..reading(1)
    }
}

#[test]
fn how_many_counts_objects_and_allows_exactly_the_limit() {
    let mut bench = Bench::new();
    let mut held = handle(1, task("sort-photos"));
    held.limits = EffectLimits::new().count(5);

    assert_eq!(use_at(&mut bench, &held, &reading(3), NOW), allowed());
    assert_eq!(use_at(&mut bench, &held, &reading(2), NOW), allowed());
    assert_eq!(
        use_at(&mut bench, &held, &reading(1), NOW),
        refused(GateRule::Limits, Limit::Count(5))
    );
}

#[test]
fn a_use_that_affects_no_object_counts_as_one_operation() {
    let mut bench = Bench::new();
    let mut held = handle(1, task("sort-photos"));
    held.limits = EffectLimits::new().count(2);

    assert_eq!(use_at(&mut bench, &held, &reading(0), NOW), allowed());
    assert_eq!(use_at(&mut bench, &held, &reading(0), NOW), allowed());
    assert_eq!(
        use_at(&mut bench, &held, &reading(0), NOW),
        refused(GateRule::Limits, Limit::Count(2))
    );
}

#[test]
fn at_what_cost_adds_up_and_allows_exactly_the_limit() {
    let mut bench = Bench::new();
    let mut held = handle(1, task("sort-photos"));
    held.limits = EffectLimits::new().cost(Cost::cents(200));

    assert_eq!(use_at(&mut bench, &held, &costing(150), NOW), allowed());
    assert_eq!(
        use_at(&mut bench, &held, &costing(51), NOW),
        refused(GateRule::Limits, Limit::Cost(Cost::cents(200)))
    );
    assert_eq!(use_at(&mut bench, &held, &costing(50), NOW), allowed());
}

#[test]
fn how_often_holds_in_any_window_and_frees_up_as_the_window_moves() {
    let mut bench = Bench::new();
    let mut held = handle(1, task("sort-photos"));
    held.limits = EffectLimits::new().rate(Rate::per(2, 60));

    assert_eq!(use_at(&mut bench, &held, &reading(1), NOW), allowed());
    assert_eq!(use_at(&mut bench, &held, &reading(1), later(30)), allowed());
    assert_eq!(
        use_at(&mut bench, &held, &reading(1), later(59)),
        refused(GateRule::Limits, Limit::Rate(Rate::per(2, 60)))
    );
    // A minute after the first use, it has left the window.
    assert_eq!(use_at(&mut bench, &held, &reading(1), later(60)), allowed());
    assert_eq!(
        use_at(&mut bench, &held, &reading(1), later(61)),
        refused(GateRule::Limits, Limit::Rate(Rate::per(2, 60)))
    );
}

/// `n` copies of `original`, each with `limits`, held by tasks of their own.
fn copies(original: &Presented, limits: EffectLimits, n: u128) -> Vec<Presented> {
    (0..n)
        .map(|i| {
            let mut copy = handle(100 + i, task(&format!("helper-{i}")));
            copy.limits = limits.clone();
            copy.original = Some(Original {
                id: original.id,
                limits: original.limits.clone(),
                ended: false,
            });
            copy
        })
        .collect()
}

#[test]
fn copies_together_never_exceed_their_originals_count() {
    let mut bench = Bench::new();
    let mut original = handle(1, task("sort-photos"));
    original.limits = EffectLimits::new().count(4);
    // Each copy may use up to its own ceiling of three.
    let copies = copies(&original, EffectLimits::new().count(3), 3);

    let mut allowed_uses = 0;
    for copy in &copies {
        for _ in 0..3 {
            if use_at(&mut bench, copy, &reading(1), NOW) == allowed() {
                allowed_uses += 1;
            }
        }
    }
    assert_eq!(allowed_uses, 4);
    // The original's holder has nothing left either.
    assert_eq!(
        use_at(&mut bench, &original, &reading(1), NOW),
        refused(GateRule::Limits, Limit::Count(4))
    );
}

#[test]
fn copies_together_never_exceed_their_originals_cost() {
    let mut bench = Bench::new();
    let mut original = handle(1, task("sort-photos"));
    original.limits = EffectLimits::new().cost(Cost::cents(100));
    let copies = copies(&original, EffectLimits::new().cost(Cost::cents(60)), 3);

    assert_eq!(use_at(&mut bench, &copies[0], &costing(60), NOW), allowed());
    assert_eq!(
        use_at(&mut bench, &copies[1], &costing(60), NOW),
        refused(GateRule::Limits, Limit::Cost(Cost::cents(100)))
    );
    assert_eq!(use_at(&mut bench, &copies[2], &costing(40), NOW), allowed());
    assert_eq!(
        use_at(&mut bench, &copies[1], &costing(1), NOW),
        refused(GateRule::Limits, Limit::Cost(Cost::cents(100)))
    );
}

#[test]
fn copies_together_never_exceed_their_originals_rate() {
    let mut bench = Bench::new();
    let mut original = handle(1, task("sort-photos"));
    original.limits = EffectLimits::new().rate(Rate::per(2, 3_600));
    let copies = copies(&original, EffectLimits::new().rate(Rate::per(2, 3_600)), 3);

    assert_eq!(use_at(&mut bench, &copies[0], &reading(1), NOW), allowed());
    assert_eq!(use_at(&mut bench, &copies[1], &reading(1), later(1)), allowed());
    assert_eq!(
        use_at(&mut bench, &copies[2], &reading(1), later(2)),
        refused(GateRule::Limits, Limit::Rate(Rate::per(2, 3_600)))
    );
}

#[test]
fn a_copy_is_held_to_its_own_limits_as_well() {
    let mut bench = Bench::new();
    let mut original = handle(1, task("sort-photos"));
    original.limits = EffectLimits::new().count(10);
    let copy = &copies(&original, EffectLimits::new().count(1), 1)[0];

    assert_eq!(use_at(&mut bench, copy, &reading(1), NOW), allowed());
    assert_eq!(
        use_at(&mut bench, copy, &reading(1), NOW),
        refused(GateRule::Limits, Limit::Count(1))
    );
    assert_eq!(use_at(&mut bench, &original, &reading(1), NOW), allowed());
}

/// Reading the photo `name`.
fn reading_photo(name: &str) -> Use {
    Use {
        objects: [photo(name)].into(),
        ..reading(0)
    }
}

#[test]
fn a_use_affects_only_the_objects_its_handle_names() {
    let mut bench = Bench::new();
    let held = handle(1, task("sort-photos"));
    assert_eq!(
        use_at(&mut bench, &held, &reading_photo("bird.jpg"), NOW),
        refused(GateRule::Limits, Limit::Objects)
    );
}

#[test]
fn a_use_affects_only_the_objects_its_handle_and_its_original_allow() {
    let mut bench = Bench::new();
    let original = handle(1, task("sort-photos"));
    let copy = &copies(&original, EffectLimits::new().objects([photo("cat.jpg")]), 1)[0];
    assert_eq!(use_at(&mut bench, copy, &reading_photo("cat.jpg"), NOW), allowed());
    assert_eq!(
        use_at(&mut bench, copy, &reading_photo("dog.jpg"), NOW),
        refused(GateRule::Limits, Limit::Objects)
    );
    assert_eq!(use_at(&mut bench, &original, &reading_photo("dog.jpg"), NOW), allowed());

    let mut narrowed = handle(2, task("sort-photos"));
    narrowed.limits = EffectLimits::new().objects([photo("cat.jpg")]);
    let copy = &copies(&narrowed, EffectLimits::new(), 1)[0];
    assert_eq!(
        use_at(&mut bench, copy, &reading_photo("dog.jpg"), NOW),
        refused(GateRule::Limits, Limit::Objects)
    );
}

#[test]
fn a_cost_that_depends_on_a_value_asks() {
    let mut bench = Bench::new();
    let mut held = handle(1, task("pay-bills"));
    held.limits = EffectLimits::new().cost(Cost::cents(10_000));
    let pay = Use {
        cost: UseCost::DependsOnValue,
        ..using("data.write", Right::Write)
    };

    assert_eq!(
        use_at(&mut bench, &held, &pay, NOW),
        asked_plainly(AskWhy::DependsOnValue)
    );
}

#[test]
fn a_refusal_wins_over_an_ask() {
    let mut bench = Bench::new();
    let mut held = handle(1, task("pay-bills"));
    held.limits = EffectLimits::new().count(1);
    assert_eq!(use_at(&mut bench, &held, &reading(1), NOW), allowed());

    let pay = Use {
        cost: UseCost::DependsOnValue,
        ..reading(1)
    };
    assert_eq!(
        use_at(&mut bench, &held, &pay, NOW),
        refused(GateRule::Limits, Limit::Count(1))
    );
}

#[test]
fn a_refused_use_counts_nothing() {
    let mut bench = Bench::new();
    let mut held = handle(1, task("sort-photos"));
    held.limits = EffectLimits::new().count(2);

    assert_eq!(
        use_at(&mut bench, &held, &reading(3), NOW),
        refused(GateRule::Limits, Limit::Count(2))
    );
    assert_eq!(use_at(&mut bench, &held, &reading(2), NOW), allowed());
}
