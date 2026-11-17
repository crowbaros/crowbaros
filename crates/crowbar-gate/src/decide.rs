//! The gate's decision: allow, refuse, or ask.

use std::collections::BTreeSet;

use crowbar_authority::{Authority, Decision, Holder, Party, StandingId};
use crowbar_capability::{
    Answer, Ask, AskWhy, Cost, EffectLimits, GateRule, Held, Issue, Limit, Principal, Rate, Refuse, Resource, Time,
};
use crowbar_labels::{Label, LabelStore};

use crate::boundary;
use crate::operation::{Act, Caller, Ending, Operation, Presented, Use, UseCost};
use crate::usage::{More, Past, Usage, counts_for_the_agent};

/// How many asks one principal may have waiting before its next is held.
pub const OPEN_ASKS: u64 = 1;

/// How many asks one principal may put to the person in an hour before its
/// next is held.
pub const ASKS_AN_HOUR: Rate = Rate::per(10, HOUR);

/// How many asks the Agent and all its tasks together may put to the person
/// in an hour before their next is held. The asks that start tasks count.
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
/// value. The one exception is a standing rule with a cap: the answer is
/// then "allowed if within the cap", and the adapter checks the amount.
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
/// 3. **The effect limits**: which objects, how many, at what cost, and how
///    often. A copy's uses count against its original's limits too, so
///    passing a handle on never multiplies what it allows. A delegation's
///    copy must be within the original's limits. A cost that depends on a
///    value counts as nothing here, and check 6 decides it.
/// 4. **The label and mark at issuance**: a Data handle's label and mark
///    are at or above the join of its objects' origins. An object with no
///    record is the unknown row.
/// 5. **The clearance, on egress**: the caller's compartment's whole label
///    is at or below the party's clearance. With no authority to read, no
///    party is cleared above public.
/// 6. **The undo boundary**: an egress, and a use the boundary table does
///    not list as inside, crosses. A crossing needs a standing grant and a
///    standing rule the caller holds, within their limits, with the rule
///    for every source that marked the act. Without them it asks. Two kinds
///    of egress need neither, because the clearance check was the whole
///    decision (ADR 0011): a model request, and a connection to a host on
///    the open internet by a task or a workbench that holds only public
///    data and no login. With no authority to read, every crossing asks.
///
///    A use whose cost depends on a value needs a standing grant and a
///    standing rule with a cap, by the same checks. With them the answer
///    is [`Decision::AllowedWithinCap`]: the gate never reads the amount,
///    and the adapter checks it against the cap when it carries the act
///    out. The use counts as the whole cap against every limit on cost,
///    the handle's among them. Without them it asks, and the person sees
///    the value. A fixed cost over a rule's cap is not covered by that
///    rule.
/// 7. **The ask**, if the result is an ask: the answer it takes, and whether
///    it is held (ADR 0026). A use inside the undo boundary takes a plain
///    answer, and a crossing takes the confirm gesture. Installed software
///    that reaches past its approval asks for a signature. An ask past a limit
///    is still an ask, marked as held with the limit it passed:
///    [`AGENT_ASKS_AN_HOUR`] for the Agent and its tasks together, then
///    [`OPEN_ASKS`] waiting at a time and [`ASKS_AN_HOUR`] for one
///    principal. Nothing is refused for asking too often.
pub fn decide(authority: Option<&Authority>, usage: &Usage, operation: &Operation<'_>, now: Time) -> Decision {
    let principal = operation.caller.principal;
    if let Some(first) = operation.key.and_then(|key| usage.first(principal, key)) {
        return match first.digest == operation.digest() {
            true => first.decision.clone(),
            false => Decision::Refused(Refuse::by(GateRule::Idempotency)),
        };
    }
    match checks(authority, usage, operation, now) {
        Ok(Verdict::Allow(under)) => Decision::Allowed { under },
        Ok(Verdict::AllowWithinCap(under, cap)) => Decision::AllowedWithinCap { under, cap },
        Ok(Verdict::Ask(why, answer)) => Decision::Asked(Ask {
            why,
            answer,
            held: held_under(usage, principal, now),
        }),
        Err(refuse) => Decision::Refused(refuse),
    }
}

/// What the checks before the last give. An ask names the answer it takes.
enum Verdict {
    Allow(Vec<StandingId>),
    /// Allowed if the amount is within the cap of the standing rule the act
    /// goes under.
    AllowWithinCap(Vec<StandingId>, Cost),
    Ask(AskWhy, Answer),
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
            if let Some(until) = limits.ended_by(now) {
                return Err(Refuse::with(GateRule::Expired, Limit::Until(until)));
            }
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

    // A cost that depends on a value is not known here, so it counts as
    // nothing against the handle's limits.
    let depends_on_value = matches!(using.cost, UseCost::DependsOnValue);
    let cost = match using.cost {
        UseCost::Fixed(cost) => cost,
        UseCost::DependsOnValue => Cost::cents(0),
    };
    let more = More {
        units: using.units(),
        cost,
        at: now,
    };
    if let Resource::Data(named) = &handle.resource
        && !using.objects.is_subset(named)
    {
        return Err(Refuse::with(GateRule::Limits, Limit::Objects));
    }
    if !affects_only_allowed(&handle.limits, using) {
        return Err(Refuse::with(GateRule::Limits, Limit::Objects));
    }
    if let Some(past) = usage.handle(handle.id).past(&handle.limits, more) {
        return Err(beyond(past));
    }
    if let Some(original) = &handle.original {
        if !affects_only_allowed(&original.limits, using) {
            return Err(Refuse::with(GateRule::Limits, Limit::Objects));
        }
        if let Some(past) = usage.handle(original.id).past(&original.limits, more) {
            return Err(beyond(past));
        }
    }

    if let Some(party) = party {
        let clearance = authority.map_or(Label::Public, |authority| authority.clearance(party));
        if !caller.compartment.label_mark().label().is_at_most(&clearance) {
            return Err(Refuse::with(GateRule::Clearance, Limit::Clearance(clearance)));
        }
    }

    let crosses = party.is_some() || boundary::crosses(&using.operation);
    let mut standing = Standing::default();
    let mut ask = None;
    // A crossing needs standing authority, unless the person already gave
    // it. So does an act whose cost depends on a value: only a standing
    // rule with a cap lets it go ahead, and otherwise the person sees the
    // value.
    if depends_on_value || (crosses && !already_authorized(authority, caller, handle, party)) {
        match covered(authority, usage, caller, handle, using, now) {
            Ok(found) => standing = found,
            Err(_) if depends_on_value => ask = Some(AskWhy::DependsOnValue),
            Err(why) => ask = Some(why),
        }
    }
    // A plain answer is accepted only for what can be undone (ADR 0026).
    // Installed software reaches only what its approval showed, so letting
    // it past that changes the approval: authority that lasts (ADR 0017).
    let answer = match (crosses, caller.principal, ask) {
        (false, _, _) => Answer::Plain,
        (true, Principal::Installed(_), Some(AskWhy::Crosses)) => Answer::Signature,
        (true, _, _) => Answer::ConfirmGesture,
    };
    Ok(match (ask, standing.cap) {
        (Some(why), _) => Verdict::Ask(why, answer),
        (None, Some(cap)) => Verdict::AllowWithinCap(standing.under, cap),
        (None, None) => Verdict::Allow(standing.under),
    })
}

/// The ask for `operation` when its adapter found the amount over the cap
/// it was allowed within, and carried nothing out. It takes the answer the
/// act itself would: the confirm gesture for a crossing.
pub(crate) fn over_cap(usage: &Usage, operation: &Operation<'_>, now: Time) -> Ask {
    let crosses = match &operation.act {
        Act::Use { using, .. } => boundary::crosses(&using.operation),
        Act::Issue { .. } | Act::Delegate { .. } | Act::Egress { .. } => true,
    };
    Ask {
        why: AskWhy::OverCap,
        answer: match crosses {
            true => Answer::ConfirmGesture,
            false => Answer::Plain,
        },
        held: held_under(usage, operation.caller.principal, now),
    }
}

/// Whether an egress to `party` crosses on authority the person already
/// gave, so it needs no standing grant or rule and does not ask (ADR 0011).
/// The clearance check has passed by the time this is asked. With no
/// authority to read, every crossing asks.
fn already_authorized(
    authority: Option<&Authority>,
    caller: &Caller<'_>,
    handle: &Presented,
    party: Option<&Party>,
) -> bool {
    if authority.is_none() {
        return false;
    }
    match party {
        // A model request: the provider's clearance is the standing
        // authority for it, and it was the person's signature.
        Some(Party::ModelProvider(_)) => true,
        // A compartment that holds only public data and no login, reaching
        // a host on the open internet on a connection handle for that host.
        // Every outside party is cleared for public data, so the clearance
        // check was the whole decision. This is for a task and a workbench.
        // A Tool and installed software reach what the person approved for
        // them, and the Agent makes no connection of its own.
        Some(Party::Site(site)) => {
            matches!(caller.principal, Principal::Task(_) | Principal::Workbench(_))
                && *caller.compartment.label_mark().label() == Label::Public
                && !caller.holds_login
                && matches!(&handle.resource, Resource::Endpoint { host, .. } if host == site)
        }
        None => false,
    }
}

/// Whether every object `using` affects is one `limits` allows.
fn affects_only_allowed(limits: &EffectLimits, using: &Use) -> bool {
    limits
        .which_objects()
        .is_none_or(|allowed| using.objects.is_subset(allowed))
}

/// Whether a grant on `granted` covers a handle on `held`: the same
/// resource, or for Data, some of the granted objects.
fn grant_covers(granted: &Resource, held: &Resource) -> bool {
    match (granted, held) {
        (Resource::Data(granted), Resource::Data(held)) => held.is_subset(granted),
        _ => granted == held,
    }
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

/// The standing authority a use goes under.
#[derive(Default)]
struct Standing {
    /// The standing grant and the standing rule, in that order.
    under: Vec<StandingId>,
    /// The rule's cap, when the cost depends on a value: the use is allowed
    /// only if the amount is within it.
    cap: Option<Cost>,
}

/// The standing grant and standing rule a use goes under, or why it asks.
///
/// A rule covers the use when it is for the operation, the use is within
/// the rule's limits and within those of a grant that covers the handle,
/// and the rule is for every source that marked the act. A use whose cost
/// depends on a value is covered only by a rule with a cap, and counts as
/// the whole cap, because the gate cannot read the amount.
fn covered(
    authority: Option<&Authority>,
    usage: &Usage,
    caller: &Caller<'_>,
    handle: &Presented,
    using: &Use,
    now: Time,
) -> Result<Standing, AskWhy> {
    let authority = authority.ok_or(AskWhy::NoAuthority)?;
    let holder = standing_holder(caller).ok_or(AskWhy::Crosses)?;
    // The act depends on everything the compartment read and on what the
    // handle names, so it carries both marks.
    let marks: BTreeSet<_> = caller
        .compartment
        .label_mark()
        .mark()
        .sources()
        .chain(handle.label_mark.mark().sources())
        .collect();
    let mut why = AskWhy::Crosses;
    for (rule_id, rule) in authority.rules(&holder) {
        if rule.operation != using.operation {
            continue;
        }
        let (cost, cap) = match (using.cost, rule.cap) {
            // The gate can check a fixed cost against the cap itself.
            (UseCost::Fixed(cost), Some(cap)) if cost > cap => continue,
            (UseCost::Fixed(cost), _) => (cost, None),
            (UseCost::DependsOnValue, Some(cap)) => (cap, Some(cap)),
            (UseCost::DependsOnValue, None) => continue,
        };
        let more = More {
            units: using.units(),
            cost,
            at: now,
        };
        let within = |id: StandingId, limits| {
            affects_only_allowed(limits, using) && usage.standing(id).past(limits, more).is_none()
        };
        if !within(rule_id, &rule.limits) {
            continue;
        }
        let grant = authority.grants(&holder).find(|(id, grant)| {
            grant_covers(&grant.resource, &handle.resource)
                && grant.rights.contains(using.right)
                && within(*id, &grant.limits)
        });
        let Some((grant_id, _)) = grant else { continue };
        // The handle's limits were checked with no cost, because the amount
        // is not known. Counted as the whole cap, the use must be within
        // them too.
        if cap.is_some() && !handle_within(usage, handle, more) {
            continue;
        }
        if !marks.iter().all(|source| rule.sources.contains(source)) {
            why = AskWhy::Marked;
            continue;
        }
        return Ok(Standing {
            under: vec![grant_id, rule_id],
            cap,
        });
    }
    Err(why)
}

/// Whether one more use is within the limits of `handle`, and of the handle
/// it was passed on from.
fn handle_within(usage: &Usage, handle: &Presented, more: More) -> bool {
    let within = |id, limits| usage.handle(id).past(limits, more).is_none();
    within(handle.id, &handle.limits)
        && handle
            .original
            .as_ref()
            .is_none_or(|original| within(original.id, &original.limits))
}

/// What holds standing authority for `caller`: a Tool, a workbench,
/// installed software, or a run of a standing task. The Agent and an ad hoc
/// task hold none (ADR 0011).
fn standing_holder(caller: &Caller<'_>) -> Option<Holder> {
    match (caller.principal, caller.standing_task) {
        (Principal::Tool(name), _) => Some(Holder::Tool(name.clone())),
        (Principal::Workbench(name), _) => Some(Holder::Workbench(name.clone())),
        (Principal::Installed(name), _) => Some(Holder::Installed(name.clone())),
        (Principal::Task(_), Some(name)) => Some(Holder::StandingTask(name.to_string())),
        _ => None,
    }
}

/// Check 7: whether one more ask from `principal` at `now` is held, and under
/// which limit. The limit on the Agent and all its tasks together is checked
/// first: an ask past it goes in their one line, whatever its own
/// principal's count, so many tasks cannot make many lines.
fn held_under(usage: &Usage, principal: &Principal, now: Time) -> Option<Held> {
    if counts_for_the_agent(principal)
        && let Some(resets_at) = resets_at(&usage.agent_asks_in_window(HOUR, now), AGENT_ASKS_AN_HOUR)
    {
        return Some(Held::AgentRate {
            rate: AGENT_ASKS_AN_HOUR,
            resets_at,
        });
    }
    if usage.open_asks(principal) >= OPEN_ASKS {
        return Some(Held::Waiting(OPEN_ASKS));
    }
    let resets_at = resets_at(&usage.asks_in_window(principal, HOUR, now), ASKS_AN_HOUR)?;
    Some(Held::Rate {
        rate: ASKS_AN_HOUR,
        resets_at,
    })
}

/// When `rate`'s window lets one more ask through, if the asks already in
/// it leave no room for one: when the oldest leaves the window.
fn resets_at(in_window: &[Time], rate: Rate) -> Option<Time> {
    if (in_window.len() as u64) < rate.count() {
        return None;
    }
    let oldest = in_window.iter().min().map_or(0, |time| time.as_secs());
    Some(Time::from_secs(oldest.saturating_add(rate.window_secs())))
}
