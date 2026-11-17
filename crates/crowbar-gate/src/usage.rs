//! The gate's count of uses, asks, and idempotency keys.

use std::collections::HashMap;

use crowbar_authority::{Decision, Event, LedgerEntry, Recorded, StandingId};
use crowbar_capability::{Cost, EffectLimits, HandleId, IdempotencyKey, Principal, Time};

/// What the gate has counted: the uses of each handle and of each standing
/// grant and rule, the asks each principal has raised, and the first
/// decision for each idempotency key.
///
/// The gate counts from the ledger. It rebuilds the count from the ledger's
/// history when it starts ([`Usage::rebuild`]), then adds each entry it
/// writes ([`Usage::record`]), so a restart does not reset a limit such as
/// "5 an hour". Both go through the same function, so what a restart
/// rebuilds is what was counted before it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    handles: HashMap<HandleId, Tally>,
    standing: HashMap<StandingId, Tally>,
    keys: HashMap<(Principal, IdempotencyKey), First>,
    asks: HashMap<Principal, Asks>,
    /// The asks of the Agent and all its tasks together.
    agent_asks: Vec<Time>,
}

/// What one handle, grant, or rule has been used for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Tally {
    /// Objects or operations, against a "how many" limit.
    units: u64,
    cost: Cost,
    /// When each use was allowed, against a rate.
    times: Vec<Time>,
}

/// The first decision for one idempotency key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct First {
    pub digest: String,
    pub decision: Decision,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Asks {
    open: u64,
    times: Vec<Time>,
}

/// One more use: how many units, at what cost, and when.
#[derive(Debug, Clone, Copy)]
pub(crate) struct More {
    pub units: u64,
    pub cost: Cost,
    pub at: Time,
}

/// The dimension of a limit set one more use would go past, with the limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Past {
    Count(u64),
    Cost(Cost),
    Rate(crowbar_capability::Rate),
    Until(Time),
}

impl Tally {
    fn add(&mut self, more: More) {
        self.units = self.units.saturating_add(more.units);
        self.cost = Cost::cents(self.cost.as_cents().saturating_add(more.cost.as_cents()));
        self.times.push(more.at);
    }

    /// The first dimension of `limits` one more use would go past: its time,
    /// how many, the cost, then how often.
    pub fn past(&self, limits: &EffectLimits, more: More) -> Option<Past> {
        if let Some(until) = limits.ended_by(more.at) {
            return Some(Past::Until(until));
        }
        if let Some(count) = limits.how_many()
            && self.units.saturating_add(more.units) > count
        {
            return Some(Past::Count(count));
        }
        if let Some(cost) = limits.max_cost()
            && self.cost.as_cents().saturating_add(more.cost.as_cents()) > cost.as_cents()
        {
            return Some(Past::Cost(cost));
        }
        if let Some(rate) = limits.how_often()
            && in_window(&self.times, rate.window_secs(), more.at).count() as u64 >= rate.count()
        {
            return Some(Past::Rate(rate));
        }
        None
    }
}

/// The times in the window of `window_secs` that ends at `now`.
fn in_window(times: &[Time], window_secs: u64, now: Time) -> impl Iterator<Item = Time> + '_ {
    times
        .iter()
        .copied()
        .filter(move |time| *time <= now && time.as_secs().saturating_add(window_secs) > now.as_secs())
}

static NONE: Tally = Tally {
    units: 0,
    cost: Cost::cents(0),
    times: Vec::new(),
};

impl Usage {
    /// Nothing counted: a gate with an empty ledger.
    pub fn new() -> Self {
        Usage::default()
    }

    /// The count the ledger's history adds up to.
    pub fn rebuild(history: &[Recorded<LedgerEntry>]) -> Self {
        let mut usage = Usage::new();
        for recorded in history {
            usage.record(&recorded.entry, recorded.time);
        }
        usage
    }

    /// Adds one ledger entry, written at `time`.
    ///
    /// - An allowed use counts against its handle, the handle's original,
    ///   and each standing grant and rule it crossed under.
    /// - An ask counts against the principal's waiting asks, held or not.
    /// - An ask that is not held also counts against the principal's asks
    ///   in the hour, and, for the Agent and its tasks, against theirs
    ///   together. The hour counts the asks that were put to the person one
    ///   at a time, so one principal's flood cannot use up a limit that
    ///   others share.
    /// - A closed ask is no longer waiting.
    /// - The first decision for an idempotency key is kept. A later entry
    ///   with the same key changes nothing.
    pub fn record(&mut self, entry: &LedgerEntry, time: Time) {
        let decision = match &entry.event {
            Event::Decided(decision) => decision,
            Event::AskClosed => {
                let asks = self.asks.entry(entry.principal.clone()).or_default();
                asks.open = asks.open.saturating_sub(1);
                return;
            }
            Event::Effect(_) => return,
        };
        if let Some(keyed) = &entry.key {
            self.keys
                .entry((entry.principal.clone(), keyed.key.clone()))
                .or_insert_with(|| First {
                    digest: keyed.digest.clone(),
                    decision: decision.clone(),
                });
        }
        match decision {
            Decision::Allowed { under } => {
                let Some(handle) = entry.handle else { return };
                let more = More {
                    units: entry.objects.unwrap_or(1).max(1),
                    cost: entry.cost.unwrap_or(Cost::cents(0)),
                    at: time,
                };
                self.handles.entry(handle).or_default().add(more);
                if let Some(original) = entry.original {
                    self.handles.entry(original).or_default().add(more);
                }
                for id in under {
                    self.standing.entry(*id).or_default().add(more);
                }
            }
            Decision::Asked(ask) => {
                let asks = self.asks.entry(entry.principal.clone()).or_default();
                asks.open = asks.open.saturating_add(1);
                if ask.held.is_some() {
                    return;
                }
                asks.times.push(time);
                if counts_for_the_agent(&entry.principal) {
                    self.agent_asks.push(time);
                }
            }
            Decision::Refused(_) => {}
        }
    }

    pub(crate) fn handle(&self, id: HandleId) -> &Tally {
        self.handles.get(&id).unwrap_or(&NONE)
    }

    pub(crate) fn standing(&self, id: StandingId) -> &Tally {
        self.standing.get(&id).unwrap_or(&NONE)
    }

    pub(crate) fn first(&self, principal: &Principal, key: &IdempotencyKey) -> Option<&First> {
        self.keys.get(&(principal.clone(), key.clone()))
    }

    pub(crate) fn open_asks(&self, principal: &Principal) -> u64 {
        self.asks.get(principal).map_or(0, |asks| asks.open)
    }

    /// The times of `principal`'s asks in the window that ends at `now`,
    /// oldest first.
    pub(crate) fn asks_in_window(&self, principal: &Principal, window_secs: u64, now: Time) -> Vec<Time> {
        let times = self.asks.get(principal).map_or(&[][..], |asks| &asks.times);
        in_window(times, window_secs, now).collect()
    }

    /// The times of the asks of the Agent and its tasks together in the
    /// window that ends at `now`, oldest first.
    pub(crate) fn agent_asks_in_window(&self, window_secs: u64, now: Time) -> Vec<Time> {
        in_window(&self.agent_asks, window_secs, now).collect()
    }
}

/// Whether `principal`'s asks count toward the limit on the Agent and all
/// its tasks together. The asks that start tasks are the Agent's.
pub(crate) fn counts_for_the_agent(principal: &Principal) -> bool {
    matches!(principal, Principal::Agent | Principal::Task(_))
}
