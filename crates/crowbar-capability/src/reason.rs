//! The gate's reasons: one closed list, shared by the gate, the ledger, and
//! the harness.

use std::collections::BTreeSet;
use std::fmt;

use crowbar_labels::{Label, LabelMark, MarkSource};
use serde::{Deserialize, Serialize};

use crate::limits::{Cost, Rate, Time, Widening};
use crate::resource::Right;

/// The rule a refusal names. This list is closed: nothing refuses under a
/// rule that is not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateRule {
    /// The broker did not issue the handle, or the principal or operation is
    /// not one the gate knows.
    Unknown,
    /// Only a handle's holder may use it or pass it on.
    Holder,
    /// The handle was revoked.
    Revoked,
    /// The holder ended, or the handle a copy was passed on from has ended.
    Ended,
    /// The handle's time ran out. The limit names when.
    Expired,
    /// The handle does not carry the right, or a copy would carry a right the
    /// original does not.
    Right,
    /// The handle was not issued delegable, or is itself a copy.
    NotDelegable,
    /// No handle passes to the Agent, and the Agent is never issued one it
    /// could pass on (ADR 0011).
    ToAgent,
    /// The operation is outside the effect limits. The limit names which.
    Limits,
    /// A handle's label and mark are below its resource's origin.
    Label,
    /// The party is not cleared for everything the compartment holds.
    Clearance,
    /// The principal has too many asks open, or has asked too often.
    AskRate,
    /// The idempotency key was used before on a different operation.
    Idempotency,
    /// The ledger could not be added to, so nothing is allowed.
    Ledger,
    /// The broker was given no `Allowed` from the gate that covers this.
    NotAllowed,
}

impl fmt::Display for GateRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            GateRule::Unknown => "unknown",
            GateRule::Holder => "holder",
            GateRule::Revoked => "revoked",
            GateRule::Ended => "ended",
            GateRule::Expired => "expired",
            GateRule::Right => "right",
            GateRule::NotDelegable => "not-delegable",
            GateRule::ToAgent => "to-agent",
            GateRule::Limits => "limits",
            GateRule::Label => "label",
            GateRule::Clearance => "clearance",
            GateRule::AskRate => "ask-rate",
            GateRule::Idempotency => "idempotency",
            GateRule::Ledger => "ledger",
            GateRule::NotAllowed => "not-allowed",
        })
    }
}

/// The limit that applied when the gate refused. It is a limit the person
/// or the gate set, never a value the task cannot read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Limit {
    /// The rule is the whole limit, such as a revoked handle.
    None,
    /// The right the handle does not carry.
    Right(Right),
    /// Which objects.
    Objects,
    /// How many objects or operations.
    Count(u64),
    /// For how long: the time the limit ended.
    Until(Time),
    /// At what cost.
    Cost(Cost),
    /// How often.
    Rate(Rate),
    /// The least label and mark the handle needed: the join of its objects'
    /// origins.
    Origin { label: Label, mark: BTreeSet<MarkSource> },
    /// The party's clearance.
    Clearance(Label),
    /// How many asks one principal may have open at a time.
    OpenAsks(u64),
    /// How many asks in a window, and when the window lets one more through.
    AskRate { rate: Rate, resets_at: Time },
}

impl Limit {
    /// The origin limit for a handle that needed `least`.
    pub fn origin(least: &LabelMark) -> Self {
        Limit::Origin {
            label: least.label().clone(),
            mark: least.mark().sources().cloned().collect(),
        }
    }
}

/// A refusal: the rule, and the limit that applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refuse {
    pub rule: GateRule,
    pub limit: Limit,
}

impl Refuse {
    /// A refusal under `rule`, where the rule is the whole limit.
    pub fn by(rule: GateRule) -> Self {
        Refuse {
            rule,
            limit: Limit::None,
        }
    }

    pub fn with(rule: GateRule, limit: Limit) -> Self {
        Refuse { rule, limit }
    }
}

impl fmt::Display for Refuse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "refused under the rule {}", self.rule)?;
        match &self.limit {
            Limit::None => Ok(()),
            Limit::Right(right) => write!(f, ": the right to {right}"),
            Limit::Objects => f.write_str(": which objects"),
            Limit::Count(count) => write!(f, ": at most {count}"),
            Limit::Until(until) => write!(f, ": until {}", until.as_secs()),
            Limit::Cost(cost) => write!(f, ": at most {} cents", cost.as_cents()),
            Limit::Rate(rate) => write!(f, ": at most {} in {} seconds", rate.count(), rate.window_secs()),
            Limit::Origin { label, mark } => {
                write!(f, ": at least {label}")?;
                for source in mark {
                    write!(f, ", marked by {source}")?;
                }
                Ok(())
            }
            Limit::Clearance(label) => write!(f, ": cleared for {label}"),
            Limit::OpenAsks(count) => write!(f, ": {count} open at a time"),
            Limit::AskRate { rate, resets_at } => write!(
                f,
                ": at most {} in {} seconds, until {}",
                rate.count(),
                rate.window_secs(),
                resets_at.as_secs()
            ),
        }
    }
}

impl std::error::Error for Refuse {}

/// A copy that would widen its original is refused: a right under
/// [`GateRule::Right`], and any other dimension under [`GateRule::Limits`],
/// naming the original's limit.
impl From<Widening> for Refuse {
    fn from(widening: Widening) -> Self {
        let limit = match widening {
            Widening::Rights => return Refuse::by(GateRule::Right),
            Widening::Objects => Limit::Objects,
            Widening::Count(count) => Limit::Count(count),
            Widening::Until(until) => Limit::Until(until),
            Widening::Cost(cost) => Limit::Cost(cost),
            Widening::Rate(rate) => Limit::Rate(rate),
        };
        Refuse::with(GateRule::Limits, limit)
    }
}

/// Why the gate asks the person rather than allowing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AskWhy {
    /// The act crosses the undo boundary, and no standing grant and standing
    /// rule the holder holds cover it.
    Crosses,
    /// A standing grant and rule cover the act, but the rule is not for every
    /// source that marked it (ADR 0011).
    Marked,
    /// The check depends on a value the task cannot read, such as an amount.
    /// The person sees the value.
    DependsOnValue,
    /// The authority record cannot be read, so nothing crosses unasked.
    NoAuthority,
}

/// An ask: the operation goes to the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ask {
    pub why: AskWhy,
}
