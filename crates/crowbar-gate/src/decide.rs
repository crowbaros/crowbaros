//! The gate's decision: allow, refuse, or ask.

use std::collections::BTreeSet;

use crowbar_authority::{Authority, Decision, Holder, Party, StandingId};
use crowbar_capability::{Ask, AskWhy, Cost, GateRule, Issue, Limit, Principal, Rate, Refuse, Resource, Time};
use crowbar_labels::{Label, LabelStore};

use crate::boundary;
use crate::operation::{Act, Caller, Ending, Operation, Presented, Use, UseCost};
use crate::usage::{More, Past, Usage, counts_for_the_agent};

/// How many asks one principal may have open at a time.
pub const OPEN_ASKS: u64 = 1;

/// How many asks one principal may raise in an hour.
pub const ASKS_AN_HOUR: Rate = Rate::per(10, HOUR);

/// How many asks the Agent and all its tasks together may raise in an hour.
/// The asks that start tasks count.
pub const AGENT_ASKS_AN_HOUR: Rate = Rate::per(30, HOUR);

const HOUR: u64 = 3_600;

/// Decides `operation` at `now`: allow, refuse, or ask.
///
/// This is pure and deterministic. It reads no file, no clock, and no
/// network: the broker passes the time in, the use count is `usage`, and
/// `authority` is what the person granted, or `None` when the authority
/// record cannot be read. The same arguments always give the same decision,
/// and nothing here changes anything, so a dry run is a call to this.
///
/// It decides only from metadata: handles, labels, marks, limits,
/// authority, and the declared shape of the operation. A check that would
/// depend on a value the task cannot read asks, so the person sees the
/// value.
///
/// An operation with an idempotency key the caller used before gets the
/// first decision again. The same key on a different operation is refused
/// under [`GateRule::Idempotency`].
///
/// Otherwise the checks run in this order, and the first that fails
/// decides, so a refusal never shows what a later check would have said.
/// Any refusal wins over an ask.
///
/// 1. **The handle**: known, held by the caller, live, and not expired. An
///    unknown principal or operation is refused here too.
/// 2. **The right**, and for a delegation, that the handle may be passed
///    on, and not to the Agent.
/// 3. **The effect limits**: how many, at what cost, and how often. A
///    copy's uses count against its original's limits too, so passing a
///    handle on never multiplies what it allows. A delegation's copy must
///    be within the original's limits. A cost that depends on a value asks.
/// 4. **The label and mark at issuance**: a Data handle's label and mark
///    are at or above the join of its objects' origins. An object with no
///    record is the unknown row.
/// 5. **The clearance, on egress**: the caller's compartment's whole label
///    is at or below the party's clearance. With no authority to read, no
///    party is cleared above public.
/// 6. **The undo boundary**: an egress, and a use the boundary table does
///    not list as inside, crosses. A crossing needs a standing grant and a
///    standing rule the caller holds, within their limits, with the rule
///    for every source that marked the act. Without them it asks. With no
///    authority to read, every crossing asks.
/// 7. **The ask rate**, if the result is an ask: [`OPEN_ASKS`] open at a
///    time and [`ASKS_AN_HOUR`] for one principal, and
///    [`AGENT_ASKS_AN_HOUR`] for the Agent and its tasks together. Over a
///    limit, the gate refuses and names when the window lets one more
///    through. Nothing is queued.
pub fn decide(authority: Option<&Authority>, usage: &Usage, operation: &Operation<'_>, now: Time) -> Decision {
    let principal = operation.caller.principal;
    if let Some(first) = operation.key.and_then(|key| usage.first(principal, key)) {
        return match first.digest == operation.digest() {
            true => first.decision.clone(),
            false => Decision::Refused(Refuse::by(GateRule::Idempotency)),
        };
    }
    let outcome = checks(authority, usage, operation, now).and_then(|verdict| match verdict {
        Verdict::Allow(under) => Ok(Decision::Allowed { under }),
        Verdict::Ask(why) => ask_rate(usage, principal, now).map(|()| Decision::Asked(Ask { why })),
    });
    outcome.unwrap_or_else(Decision::Refused)
}

/// What the checks before the ask rate give.
enum Verdict {
    Allow(Vec<StandingId>),
    Ask(AskWhy),
}

fn checks(
    authority: Option<&Authority>,
    usage: &Usage,
    operation: &Operation<'_>,
    now: Time,
) -> Result<Verdict, Refuse> {
    let caller = &operation.caller;
    known(caller.principal)?;
    match &operation.act {
        Act::Issue { issue, labels } => issuance(issue, labels, now),
        Act::Delegate {
            handle,
            to,
            rights,
            limits,
        } => {
            known(to)?;
            let handle = held(*handle, caller.principal, now)?;
            if !handle.delegable || handle.original.is_some() {
                return Err(Refuse::by(GateRule::NotDelegable));
            }
            if **to == Principal::Agent {
                return Err(Refuse::by(GateRule::ToAgent));
            }
            if !rights.is_subset(&handle.rights) {
                return Err(Refuse::by(GateRule::Right));
            }
            limits.within(&handle.limits).map_err(Refuse::from)?;
            Ok(Verdict::Allow(Vec::new()))
        }
        Act::Use { handle, using } => use_of(authority, usage, caller, *handle, using, None, now),
        Act::Egress { handle, using, party } => use_of(authority, usage, caller, *handle, using, Some(party), now),
    }
}

/// A principal the gate knows: the Agent, or one with a name.
fn known(principal: &Principal) -> Result<(), Refuse> {
    let named = match principal {
        Principal::Agent => true,
        Principal::Task(task) => !task.as_str().is_empty(),
        Principal::Tool(name) | Principal::Installed(name) | Principal::Workbench(name) => !name.is_empty(),
    };
    match named {
        true => Ok(()),
        false => Err(Refuse::by(GateRule::Unknown)),
    }
}

/// Check 1: the handle is known, held by `caller`, live, and not expired.
/// A caller that is not the holder learns nothing about the handle's state.
fn held<'a>(handle: Option<&'a Presented>, caller: &Principal, now: Time) -> Result<&'a Presented, Refuse> {
    let handle = handle.ok_or(Refuse::by(GateRule::Unknown))?;
    if &handle.holder != caller {
        return Err(Refuse::by(GateRule::Holder));
    }
    match handle.ended {
        Some(Ending::Revoked) => return Err(Refuse::by(GateRule::Revoked)),
        Some(Ending::HolderEnded) => return Err(Refuse::by(GateRule::Ended)),
        None => {}
    }
    if let Some(until) = handle.limits.ended_by(now) {
        return Err(Refuse::with(GateRule::Expired, Limit::Until(until)));
    }
    if handle.original.as_ref().is_some_and(|original| original.ended) {
        return Err(Refuse::by(GateRule::Ended));
    }
    Ok(handle)
}

/// The checks on a new handle.
fn issuance(issue: &Issue, labels: &LabelStore, now: Time) -> Result<Verdict, Refuse> {
    known(&issue.holder)?;
    if let Some(until) = issue.limits.ended_by(now) {
        return Err(Refuse::with(GateRule::Expired, Limit::Until(until)));
    }
    if issue.holder == Principal::Agent && issue.delegable {
        return Err(Refuse::by(GateRule::ToAgent));
    }
    if let Resource::Data(objects) = &issue.resource {
        let least = objects
            .iter()
            .map(|object| labels.origin_of(object).label_mark())
            .reduce(|held, next| held.join(&next));
        if let Some(least) = least
            && !least.is_at_most(&issue.label_mark)
        {
            return Err(Refuse::with(GateRule::Label, Limit::origin(&least)));
        }
    }
    Ok(Verdict::Allow(Vec::new()))
}

/// The checks on a use, and on an egress when `party` is set.
fn use_of(
    authority: Option<&Authority>,
    usage: &Usage,
    caller: &Caller<'_>,
    handle: Option<&Presented>,
    using: &Use,
    party: Option<&Party>,
    now: Time,
) -> Result<Verdict, Refuse> {
    if !boundary::is_named(&using.operation) {
        return Err(Refuse::by(GateRule::Unknown));
    }
    let handle = held(handle, caller.principal, now)?;
    if !handle.rights.contains(using.right) {
        return Err(Refuse::with(GateRule::Right, Limit::Right(using.right)));
    }

    let mut ask = None;
    let cost = match using.cost {
        UseCost::Fixed(cost) => cost,
        UseCost::DependsOnValue => {
            ask.get_or_insert(AskWhy::DependsOnValue);
            Cost::cents(0)
        }
    };
    let more = More {
        units: using.units(),
        cost,
        at: now,
    };
    if let Some(past) = usage.handle(handle.id).past(&handle.limits, more) {
        return Err(beyond(past));
    }
    if let Some(original) = &handle.original
        && let Some(past) = usage.handle(original.id).past(&original.limits, more)
    {
        return Err(beyond(past));
    }

    if let Some(party) = party {
        let clearance = authority.map_or(Label::Public, |authority| authority.clearance(party));
        if !caller.compartment.label_mark().label().is_at_most(&clearance) {
            return Err(Refuse::with(GateRule::Clearance, Limit::Clearance(clearance)));
        }
    }

    let mut under = Vec::new();
    if party.is_some() || boundary::crosses(&using.operation) {
        match covered(authority, usage, caller, handle, using, more) {
            Ok(standing) => under = standing,
            Err(why) => {
                ask.get_or_insert(why);
            }
        }
    }
    Ok(match ask {
        Some(why) => Verdict::Ask(why),
        None => Verdict::Allow(under),
    })
}

/// The refusal for a use that would go past a limit.
fn beyond(past: Past) -> Refuse {
    let limit = match past {
        Past::Count(count) => Limit::Count(count),
        Past::Cost(cost) => Limit::Cost(cost),
        Past::Rate(rate) => Limit::Rate(rate),
        Past::Until(until) => Limit::Until(until),
    };
    Refuse::with(GateRule::Limits, limit)
}

/// The standing grant and standing rule a crossing goes under, or why it
/// asks.
fn covered(
    authority: Option<&Authority>,
    usage: &Usage,
    caller: &Caller<'_>,
    handle: &Presented,
    using: &Use,
    more: More,
) -> Result<Vec<StandingId>, AskWhy> {
    let authority = authority.ok_or(AskWhy::NoAuthority)?;
    let holder = standing_holder(caller).ok_or(AskWhy::Crosses)?;
    let within = |id: StandingId, limits| usage.standing(id).past(limits, more).is_none();
    let (grant, _) = authority
        .grants(&holder)
        .find(|(id, grant)| {
            grant.resource == handle.resource && grant.rights.contains(using.right) && within(*id, &grant.limits)
        })
        .ok_or(AskWhy::Crosses)?;
    let mut rules = authority
        .rules(&holder)
        .filter(|(id, rule)| rule.operation == using.operation && within(*id, &rule.limits))
        .peekable();
    rules.peek().ok_or(AskWhy::Crosses)?;
    // The act depends on everything the compartment read and on what the
    // handle names, so it carries both marks.
    let marks: BTreeSet<_> = caller
        .compartment
        .label_mark()
        .mark()
        .sources()
        .chain(handle.label_mark.mark().sources())
        .collect();
    let (rule, _) = rules
        .find(|(_, rule)| marks.iter().all(|source| rule.sources.contains(source)))
        .ok_or(AskWhy::Marked)?;
    Ok(vec![grant, rule])
}

/// What holds standing authority for `caller`: a Tool, a workbench, or a
/// run of a standing task. The Agent, an ad hoc task, and installed
/// software hold none (ADR 0011).
fn standing_holder(caller: &Caller<'_>) -> Option<Holder> {
    match (caller.principal, caller.standing_task) {
        (Principal::Tool(name), _) => Some(Holder::Tool(name.clone())),
        (Principal::Workbench(name), _) => Some(Holder::Workbench(name.clone())),
        (Principal::Task(_), Some(name)) => Some(Holder::StandingTask(name.to_string())),
        _ => None,
    }
}

/// Check 7: whether `principal` may raise one more ask at `now`.
fn ask_rate(usage: &Usage, principal: &Principal, now: Time) -> Result<(), Refuse> {
    if usage.open_asks(principal) >= OPEN_ASKS {
        return Err(Refuse::with(GateRule::AskRate, Limit::OpenAsks(OPEN_ASKS)));
    }
    within_rate(&usage.asks_in_window(principal, HOUR, now), ASKS_AN_HOUR)?;
    if counts_for_the_agent(principal) {
        within_rate(&usage.agent_asks_in_window(HOUR, now), AGENT_ASKS_AN_HOUR)?;
    }
    Ok(())
}

/// Whether one more ask fits `rate`, given the asks already in its window.
/// When it does not, the refusal names when the oldest leaves the window.
fn within_rate(in_window: &[Time], rate: Rate) -> Result<(), Refuse> {
    if (in_window.len() as u64) < rate.count() {
        return Ok(());
    }
    let oldest = in_window.iter().min().map_or(0, |time| time.as_secs());
    let resets_at = Time::from_secs(oldest.saturating_add(rate.window_secs()));
    Err(Refuse::with(GateRule::AskRate, Limit::AskRate { rate, resets_at }))
}
