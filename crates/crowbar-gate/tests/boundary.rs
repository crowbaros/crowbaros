//! An operation inside the undo boundary needs nothing more. One that
//! crosses needs a standing grant and a standing rule the caller holds,
//! within their limits, with the rule for every source that marked the act
//! (ADR 0011). Without them it asks. With no authority to read, every
//! crossing asks, and a refusal stays a refusal.

use crowbar_authority::{Authority, Change, Grant, Holder, Rule, StandingId};
use crowbar_capability::{EffectLimits, Principal, Rate, Resource, Right, Rights, TaskId};
use crowbar_gate::{Act, AskWhy, Caller, Decision, GateRule, Limit, Operation, Presented, Use};
use crowbar_labels::{Compartment, Fetch, MailAccount, MarkSource, ObjectId, Origin, Site};

mod common;
use common::{Bench, NOW, allowed, asked, by, handle, later, personal, refused, task, tool, using};

fn morning_pages() -> Holder {
    Holder::Tool("morning-pages".to_string())
}

fn writing() -> Resource {
    Resource::Data([ObjectId::new("writing/journal.md")].into())
}

/// "Morning Pages may read and write your writing."
fn grant() -> Grant {
    Grant {
        holder: morning_pages(),
        resource: writing(),
        rights: Rights::from([Right::Read, Right::Write]),
        limits: EffectLimits::new(),
        reason: "Keeps your morning pages.".to_string(),
    }
}

/// "Morning Pages may sync to your Nextcloud, at most once every ten
/// minutes."
fn rule() -> Rule {
    Rule {
        holder: morning_pages(),
        operation: "nextcloud.sync".to_string(),
        limits: EffectLimits::new().rate(Rate::per(1, 600)),
        sources: [].into(),
    }
}

/// The handle `holder` holds on the writing.
fn on_writing(holder: Principal) -> Presented {
    Presented {
        resource: writing(),
        ..handle(1, holder)
    }
}

fn sync() -> Use {
    using("nextcloud.sync", Right::Read)
}

/// The ids of the grant and the rule `holder` holds, in that order.
fn standing(authority: &Authority, holder: &Holder) -> Vec<StandingId> {
    let grants = authority.grants(holder).map(|(id, _)| id);
    grants.chain(authority.rules(holder).map(|(id, _)| id)).collect()
}

fn sync_by(
    bench: &mut Bench,
    caller: &Principal,
    compartment: &Compartment,
    held: &Presented,
    now: crowbar_capability::Time,
) -> Decision {
    let what = sync();
    bench.decide(
        &by(
            caller,
            compartment,
            Act::Use {
                handle: Some(held),
                using: &what,
            },
        ),
        now,
    )
}

#[test]
fn an_operation_inside_the_boundary_needs_no_standing_authority() {
    let mut bench = Bench::without_authority();
    let holder = task("sort-photos");
    let mut held = handle(1, holder.clone());
    held.rights = Rights::from([Right::Read, Right::Write, Right::Create]);
    let compartment = Compartment::fresh();

    for (operation, right) in [
        ("data.read", Right::Read),
        ("data.write", Right::Write),
        ("data.create", Right::Create),
    ] {
        let inside = using(operation, right);
        let decision = bench.decide(
            &by(
                &holder,
                &compartment,
                Act::Use {
                    handle: Some(&held),
                    using: &inside,
                },
            ),
            NOW,
        );
        assert_eq!(decision, allowed(), "{operation}");
    }
}

#[test]
fn a_crossing_under_a_standing_grant_and_rule_is_allowed_and_names_both() {
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(rule())]);
    let under = standing(bench.authority.as_ref().expect("readable"), &morning_pages());
    let caller = tool("morning-pages");

    let decision = sync_by(
        &mut bench,
        &caller,
        &Compartment::fresh(),
        &on_writing(caller.clone()),
        NOW,
    );
    assert_eq!(decision, Decision::Allowed { under });
}

#[test]
fn a_grant_on_a_set_of_data_objects_covers_a_handle_on_some_of_them() {
    let wider = Grant {
        resource: Resource::Data([ObjectId::new("writing/journal.md"), ObjectId::new("writing/notes.md")].into()),
        ..grant()
    };
    let mut bench = Bench::with(vec![Change::Granted(wider), Change::RuleAdded(rule())]);
    let under = standing(bench.authority.as_ref().expect("readable"), &morning_pages());
    let caller = tool("morning-pages");

    let decision = sync_by(
        &mut bench,
        &caller,
        &Compartment::fresh(),
        &on_writing(caller.clone()),
        NOW,
    );
    assert_eq!(decision, Decision::Allowed { under });
}

#[test]
fn an_operation_the_table_does_not_list_crosses_and_asks_without_standing_authority() {
    let mut bench = Bench::with(vec![Change::Granted(grant())]);
    let caller = tool("morning-pages");

    let decision = sync_by(
        &mut bench,
        &caller,
        &Compartment::fresh(),
        &on_writing(caller.clone()),
        NOW,
    );
    assert_eq!(decision, asked(AskWhy::Crosses));
}

#[test]
fn a_rule_without_a_grant_for_the_resource_and_the_right_asks() {
    let other = Grant {
        resource: Resource::Data([ObjectId::new("writing/other.md")].into()),
        ..grant()
    };
    let read_only = Grant {
        rights: Rights::from([Right::Write]),
        ..grant()
    };
    for grants in [vec![], vec![Change::Granted(other)], vec![Change::Granted(read_only)]] {
        let mut changes = grants;
        changes.push(Change::RuleAdded(rule()));
        let mut bench = Bench::with(changes);
        let caller = tool("morning-pages");

        let decision = sync_by(
            &mut bench,
            &caller,
            &Compartment::fresh(),
            &on_writing(caller.clone()),
            NOW,
        );
        assert_eq!(decision, asked(AskWhy::Crosses));
    }
}

#[test]
fn a_revoked_rule_covers_nothing() {
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(rule())]);
    let rule_id = standing(bench.authority.as_ref().expect("readable"), &morning_pages())[1];
    let mut revoked = Bench::with(vec![
        Change::Granted(grant()),
        Change::RuleAdded(rule()),
        Change::Revoked(rule_id),
    ]);
    let caller = tool("morning-pages");

    assert_ne!(
        sync_by(
            &mut bench,
            &caller,
            &Compartment::fresh(),
            &on_writing(caller.clone()),
            NOW
        ),
        asked(AskWhy::Crosses)
    );
    assert_eq!(
        sync_by(
            &mut revoked,
            &caller,
            &Compartment::fresh(),
            &on_writing(caller.clone()),
            NOW
        ),
        asked(AskWhy::Crosses)
    );
}

#[test]
fn past_its_rate_a_rule_covers_nothing_and_the_act_asks() {
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(rule())]);
    let under = standing(bench.authority.as_ref().expect("readable"), &morning_pages());
    let caller = tool("morning-pages");
    let held = on_writing(caller.clone());

    assert_eq!(
        sync_by(&mut bench, &caller, &Compartment::fresh(), &held, NOW),
        Decision::Allowed { under: under.clone() }
    );
    assert_eq!(
        sync_by(&mut bench, &caller, &Compartment::fresh(), &held, later(599)),
        asked(AskWhy::Crosses)
    );
    // An ask is open now. Once it closes, and the ten minutes are up, the
    // rule covers the sync again.
    bench.gate.ask_closed(&caller, later(600)).expect("recorded");
    assert_eq!(
        sync_by(&mut bench, &caller, &Compartment::fresh(), &held, later(600)),
        Decision::Allowed { under }
    );
}

#[test]
fn past_its_grants_limits_the_act_asks() {
    let counted = Grant {
        limits: EffectLimits::new().count(1),
        ..grant()
    };
    let hourly = Rule {
        limits: EffectLimits::new().rate(Rate::per(10, 3_600)),
        ..rule()
    };
    let mut bench = Bench::with(vec![Change::Granted(counted), Change::RuleAdded(hourly)]);
    let caller = tool("morning-pages");
    let held = on_writing(caller.clone());

    assert!(matches!(
        sync_by(&mut bench, &caller, &Compartment::fresh(), &held, NOW),
        Decision::Allowed { .. }
    ));
    assert_eq!(
        sync_by(&mut bench, &caller, &Compartment::fresh(), &held, NOW),
        asked(AskWhy::Crosses)
    );
}

#[test]
fn a_marked_act_crosses_only_under_a_rule_for_every_source_that_marked_it() {
    let news = Origin::WebPage(Fetch::WithoutLogin(Site::new("news.example"))).label_mark();
    let mut marked = Compartment::fresh();
    marked.read(&news).expect("a task's compartment grows");
    let caller = tool("morning-pages");

    let mut unmarked_rule = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(rule())]);
    assert_eq!(
        sync_by(&mut unmarked_rule, &caller, &marked, &on_writing(caller.clone()), NOW),
        asked(AskWhy::Marked)
    );

    let for_news = Rule {
        sources: [MarkSource::Site(Site::new("news.example"))].into(),
        ..rule()
    };
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(for_news)]);
    assert!(matches!(
        sync_by(&mut bench, &caller, &marked, &on_writing(caller.clone()), NOW),
        Decision::Allowed { .. }
    ));
}

#[test]
fn what_the_handle_names_marks_the_act_too() {
    let mail = Origin::Mail {
        account: MailAccount::new("me@mail.example"),
    }
    .label_mark();
    let mut held = on_writing(tool("morning-pages"));
    held.label_mark = mail.join(&personal());
    let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(rule())]);

    assert_eq!(
        sync_by(&mut bench, &tool("morning-pages"), &Compartment::fresh(), &held, NOW),
        asked(AskWhy::Marked)
    );
}

#[test]
fn the_agent_and_an_ad_hoc_task_hold_no_standing_authority() {
    for caller in [Principal::Agent, task("one-off")] {
        let mut bench = Bench::with(vec![Change::Granted(grant()), Change::RuleAdded(rule())]);
        let decision = sync_by(
            &mut bench,
            &caller,
            &Compartment::fresh(),
            &on_writing(caller.clone()),
            NOW,
        );
        assert_eq!(decision, asked(AskWhy::Crosses));
    }
}

#[test]
fn a_run_of_a_standing_task_crosses_under_the_standing_tasks_grant_and_rule() {
    let nightly = Holder::StandingTask("nightly-sync".to_string());
    let mut bench = Bench::with(vec![
        Change::Granted(Grant {
            holder: nightly.clone(),
            ..grant()
        }),
        Change::RuleAdded(Rule {
            holder: nightly.clone(),
            ..rule()
        }),
    ]);
    let under = standing(bench.authority.as_ref().expect("readable"), &nightly);
    let run = Principal::Task(TaskId::new("nightly-sync-2026-11-15"));
    let held = on_writing(run.clone());
    let compartment = Compartment::fresh();
    let what = sync();

    let operation = Operation {
        caller: Caller {
            principal: &run,
            standing_task: Some("nightly-sync"),
            compartment: &compartment,
        },
        key: None,
        act: Act::Use {
            handle: Some(&held),
            using: &what,
        },
    };
    assert_eq!(bench.decide(&operation, NOW), Decision::Allowed { under });
}

#[test]
fn with_no_authority_to_read_every_crossing_asks_and_a_refusal_stays_a_refusal() {
    let mut bench = Bench::without_authority();
    let caller = tool("morning-pages");
    let mut held = on_writing(caller.clone());

    assert_eq!(
        sync_by(&mut bench, &caller, &Compartment::fresh(), &held, NOW),
        asked(AskWhy::NoAuthority)
    );

    held.rights = Rights::from([Right::Write]);
    assert_eq!(
        sync_by(&mut bench, &caller, &Compartment::fresh(), &held, NOW),
        refused(GateRule::Right, Limit::Right(Right::Read))
    );
}
