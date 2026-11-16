//! Effect limits: what a task, a grant, or a handle may affect.

use std::collections::BTreeSet;
use std::fmt;

use crowbar_labels::ObjectId;
use serde::{Deserialize, Serialize};

/// A moment, in whole seconds, on the broker's clock. The broker passes the
/// time in, so a rehearsal can run on a simulated clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Time(u64);

impl Time {
    pub const fn from_secs(secs: u64) -> Self {
        Time(secs)
    }

    pub fn as_secs(self) -> u64 {
        self.0
    }
}

/// An amount of money, in hundredths of the person's currency.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Cost(u64);

impl Cost {
    pub const fn cents(cents: u64) -> Self {
        Cost(cents)
    }

    pub fn as_cents(self) -> u64 {
        self.0
    }
}

/// How often: at most `count` in any window of `window_secs` seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rate {
    count: u64,
    window_secs: u64,
}

impl Rate {
    pub const fn per(count: u64, window_secs: u64) -> Self {
        Rate { count, window_secs }
    }

    pub fn count(self) -> u64 {
        self.count
    }

    pub fn window_secs(self) -> u64 {
        self.window_secs
    }

    /// Whether this rate never allows more than `bound` does, in any window
    /// of the bound's length. That holds when it allows no more at a time and
    /// its window is no shorter. Any other pair is refused, even where some
    /// arithmetic could show it fits.
    fn is_within(&self, bound: &Rate) -> bool {
        self.count <= bound.count && self.window_secs >= bound.window_secs
    }
}

/// Effect limits: what a task, a grant, or a handle may affect. Which
/// objects, how many, for how long, at what cost, and how often.
///
/// A dimension that is not set is not limited. Every dimension narrows the
/// same way: a limit set is [`EffectLimits::within`] a bound when it allows
/// nothing the bound does not, so a task, a copy of a handle, or anything
/// else bounded by these limits can narrow them and never widen them.
///
/// This holds the limits. The gate checks each operation against them.
///
/// Stored, a dimension that is not set is left out, and a field the limits
/// do not have is refused when they are loaded.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    objects: Option<BTreeSet<ObjectId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    until: Option<Time>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost: Option<Cost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rate: Option<Rate>,
}

impl EffectLimits {
    pub fn new() -> Self {
        EffectLimits::default()
    }

    /// Which objects: these, and no others.
    pub fn objects(self, objects: impl IntoIterator<Item = ObjectId>) -> Self {
        EffectLimits {
            objects: Some(objects.into_iter().collect()),
            ..self
        }
    }

    /// How many: at most this many objects or operations.
    pub fn count(self, count: u64) -> Self {
        EffectLimits {
            count: Some(count),
            ..self
        }
    }

    /// For how long: nothing is affected at or after `time`.
    pub fn until(self, time: Time) -> Self {
        EffectLimits {
            until: Some(time),
            ..self
        }
    }

    /// At what cost: at most this much money, across everything the limits
    /// cover.
    pub fn cost(self, cost: Cost) -> Self {
        EffectLimits {
            cost: Some(cost),
            ..self
        }
    }

    /// How often: at most this rate.
    pub fn rate(self, rate: Rate) -> Self {
        EffectLimits {
            rate: Some(rate),
            ..self
        }
    }

    /// Which objects, if that is limited.
    pub fn which_objects(&self) -> Option<&BTreeSet<ObjectId>> {
        self.objects.as_ref()
    }

    /// How many, if that is limited.
    pub fn how_many(&self) -> Option<u64> {
        self.count
    }

    /// When the time limit ends, if there is one.
    pub fn ends_at(&self) -> Option<Time> {
        self.until
    }

    /// At what cost, if that is limited.
    pub fn max_cost(&self) -> Option<Cost> {
        self.cost
    }

    /// How often, if that is limited.
    pub fn how_often(&self) -> Option<Rate> {
        self.rate
    }

    /// Whether these limits allow nothing `bound` does not. Where they do,
    /// the error names the dimension and the bound's limit in it.
    pub fn within(&self, bound: &EffectLimits) -> Result<(), Widening> {
        if !fits(&self.objects, &bound.objects, |limit, bound| limit.is_subset(bound)) {
            return Err(Widening::Objects);
        }
        if let (false, Some(count)) = (
            fits(&self.count, &bound.count, |limit, bound| limit <= bound),
            bound.count,
        ) {
            return Err(Widening::Count(count));
        }
        if let (false, Some(until)) = (
            fits(&self.until, &bound.until, |limit, bound| limit <= bound),
            bound.until,
        ) {
            return Err(Widening::Until(until));
        }
        if let (false, Some(cost)) = (fits(&self.cost, &bound.cost, |limit, bound| limit <= bound), bound.cost) {
            return Err(Widening::Cost(cost));
        }
        if let (false, Some(rate)) = (fits(&self.rate, &bound.rate, Rate::is_within), bound.rate) {
            return Err(Widening::Rate(rate));
        }
        Ok(())
    }

    /// The end of the time limit, if it has come by `now`: nothing is
    /// affected at or after it.
    pub fn ended_by(&self, now: Time) -> Option<Time> {
        self.until.filter(|until| now >= *until)
    }
}

/// Whether one dimension of a limit set is within the same dimension of its
/// bound. No limit is the widest there is: it fits only a bound with no limit.
fn fits<T>(limit: &Option<T>, bound: &Option<T>, narrower: impl Fn(&T, &T) -> bool) -> bool {
    match (limit, bound) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(limit), Some(bound)) => narrower(limit, bound),
    }
}

/// The dimension in which a limit set allows more than its bound, with the
/// bound's limit in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Widening {
    /// A right the bound does not carry. Rights belong to a handle, not to
    /// effect limits, and widen the same way.
    Rights,
    Objects,
    Count(u64),
    Until(Time),
    Cost(Cost),
    Rate(Rate),
}

impl fmt::Display for Widening {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Widening::Rights => f.write_str("a right the original does not carry"),
            Widening::Objects => f.write_str("objects beyond the original's"),
            Widening::Count(count) => write!(f, "more than the original's count of {count}"),
            Widening::Until(until) => write!(f, "a later end than the original's, at {}", until.as_secs()),
            Widening::Cost(cost) => write!(f, "more than the original's cost of {} cents", cost.as_cents()),
            Widening::Rate(rate) => write!(
                f,
                "a rate the original's {} in {} seconds does not allow",
                rate.count(),
                rate.window_secs()
            ),
        }
    }
}

impl std::error::Error for Widening {}
