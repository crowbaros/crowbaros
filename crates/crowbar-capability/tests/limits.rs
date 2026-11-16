//! Effect limits only narrow. A limit set is within a bound when it allows
//! nothing the bound does not, in every dimension: which objects, how many,
//! for how long, at what cost, and how often. A task cannot widen its own.

use crowbar_capability::{Cost, EffectLimits, Rate, Time, Widening};
use crowbar_labels::ObjectId;

fn at(secs: u64) -> Time {
    Time::from_secs(secs)
}

fn objects(names: &[&str]) -> Vec<ObjectId> {
    names.iter().map(|name| ObjectId::new(name)).collect()
}

#[test]
fn which_objects_narrows_to_a_subset_and_never_adds_one() {
    let bound = EffectLimits::new().objects(objects(&["a", "b"]));

    assert_eq!(EffectLimits::new().objects(objects(&["a", "b"])).within(&bound), Ok(()));
    assert_eq!(EffectLimits::new().objects(objects(&["b"])).within(&bound), Ok(()));
    assert_eq!(
        EffectLimits::new().objects(objects(&["a", "c"])).within(&bound),
        Err(Widening::Objects)
    );
    assert_eq!(EffectLimits::new().within(&bound), Err(Widening::Objects));
}

#[test]
fn for_how_long_narrows_to_an_earlier_end_and_never_to_a_later_one() {
    let bound = EffectLimits::new().until(at(100));

    assert_eq!(EffectLimits::new().until(at(100)).within(&bound), Ok(()));
    assert_eq!(EffectLimits::new().until(at(50)).within(&bound), Ok(()));
    assert_eq!(
        EffectLimits::new().until(at(101)).within(&bound),
        Err(Widening::Until(at(100)))
    );
    // No end at all is the widest there is.
    assert_eq!(EffectLimits::new().within(&bound), Err(Widening::Until(at(100))));
    // A bound with no end allows any end.
    assert_eq!(EffectLimits::new().until(at(5)).within(&EffectLimits::new()), Ok(()));
}

#[test]
fn how_many_narrows_to_fewer_and_never_to_more() {
    let bound = EffectLimits::new().count(500);

    assert_eq!(EffectLimits::new().count(500).within(&bound), Ok(()));
    assert_eq!(EffectLimits::new().count(10).within(&bound), Ok(()));
    assert_eq!(EffectLimits::new().count(501).within(&bound), Err(Widening::Count(500)));
    assert_eq!(EffectLimits::new().within(&bound), Err(Widening::Count(500)));
}

#[test]
fn at_what_cost_narrows_to_less_and_never_to_more() {
    let bound = EffectLimits::new().cost(Cost::cents(200));

    assert_eq!(EffectLimits::new().cost(Cost::cents(200)).within(&bound), Ok(()));
    assert_eq!(EffectLimits::new().cost(Cost::cents(0)).within(&bound), Ok(()));
    assert_eq!(
        EffectLimits::new().cost(Cost::cents(201)).within(&bound),
        Err(Widening::Cost(Cost::cents(200)))
    );
    assert_eq!(
        EffectLimits::new().within(&bound),
        Err(Widening::Cost(Cost::cents(200)))
    );
}

#[test]
fn how_often_narrows_only_to_a_rate_that_allows_no_more_in_any_window() {
    const HOUR: u64 = 3_600;
    let bound = EffectLimits::new().rate(Rate::per(5, HOUR));
    let rate = |count, window| EffectLimits::new().rate(Rate::per(count, window));

    assert_eq!(rate(5, HOUR).within(&bound), Ok(()));
    assert_eq!(rate(3, HOUR).within(&bound), Ok(()));
    // Five every two hours is never more than five in one hour.
    assert_eq!(rate(5, 2 * HOUR).within(&bound), Ok(()));

    assert_eq!(rate(6, HOUR).within(&bound), Err(Widening::Rate(Rate::per(5, HOUR))));
    // One a minute is sixty in an hour.
    assert_eq!(rate(1, 60).within(&bound), Err(Widening::Rate(Rate::per(5, HOUR))));
    // Ten every two hours can all fall in one hour.
    assert_eq!(
        rate(10, 2 * HOUR).within(&bound),
        Err(Widening::Rate(Rate::per(5, HOUR)))
    );
    assert_eq!(
        EffectLimits::new().within(&bound),
        Err(Widening::Rate(Rate::per(5, HOUR)))
    );
}

#[test]
fn a_limit_set_is_within_its_bound_only_in_every_dimension_at_once() {
    let bound = EffectLimits::new()
        .objects(objects(&["a", "b"]))
        .count(10)
        .until(at(100))
        .cost(Cost::cents(200))
        .rate(Rate::per(5, 3_600));
    let narrow = EffectLimits::new()
        .objects(objects(&["a"]))
        .count(2)
        .until(at(50))
        .cost(Cost::cents(0))
        .rate(Rate::per(1, 3_600));

    assert_eq!(narrow.within(&bound), Ok(()));
    assert_eq!(bound.within(&bound), Ok(()));
    // Narrow in four dimensions and wide in one is still a widening.
    assert_eq!(narrow.clone().count(11).within(&bound), Err(Widening::Count(10)));
    // Everything is within no limits at all, and no limits are within nothing
    // else.
    assert_eq!(bound.within(&EffectLimits::new()), Ok(()));
    assert_eq!(EffectLimits::new().within(&EffectLimits::new()), Ok(()));
}
