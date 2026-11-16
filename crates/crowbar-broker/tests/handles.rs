//! A handle is a right for one principal to use one resource. The broker's
//! table is the source of truth: a handle works only for its holder, only for
//! the rights it names, and only while it is live.

use crowbar_broker::{
    Delegation, EffectLimits, HandleId, HandleTable, Principal, Refusal, ResourceKind, Right, Rights, Time,
};
use crowbar_capability::{GateRule, Limit, Refuse};
use crowbar_gate::Ending;
use crowbar_gate::Ruling;
use crowbar_labels::Compartment;

mod common;
use common::{NOW, allow, delegate, gate_on_delegation, gate_on_issue, personal, photos_for, spare, task};

#[test]
fn the_holder_uses_its_handle_and_another_principal_cannot() {
    let mut table = HandleTable::new();
    let handle = table
        .issue(allow(&photos_for(task("sort-photos"))), NOW)
        .expect("issued");

    let used = table
        .exercise(&handle, &task("sort-photos"), Right::Read, NOW)
        .expect("the holder reads");
    assert_eq!(used.rights(), &[Right::Read, Right::Write].into());

    assert_eq!(
        table
            .exercise(&handle, &task("other-task"), Right::Read, NOW)
            .unwrap_err(),
        Refusal::NotHolder
    );
}

#[test]
fn a_handle_this_table_did_not_issue_is_refused() {
    let mut table = HandleTable::new();
    table
        .issue(allow(&photos_for(task("sort-photos"))), NOW)
        .expect("issued");

    // Another table issues the same request, as a forger who knows the
    // request would. Its handle confers nothing here.
    let mut other = HandleTable::new();
    let forged = other
        .issue(allow(&photos_for(task("sort-photos"))), NOW)
        .expect("issued");

    assert_eq!(
        table
            .exercise(&forged, &task("sort-photos"), Right::Read, NOW)
            .unwrap_err(),
        Refusal::Unknown
    );
}

#[test]
fn the_id_shown_to_the_agent_names_a_handle_and_confers_nothing() {
    let mut table = HandleTable::new();
    let handle = table
        .issue(allow(&photos_for(task("sort-photos"))), NOW)
        .expect("issued");

    // The Agent sees the id as text and sends it back as text.
    let shown = handle.id().to_string();
    let id: HandleId = shown.parse().expect("an id reads back from its text");
    assert_eq!(id, handle.id());

    // From the id, the table states the kind, the label, and the mark. It
    // takes no id where it takes a handle to use, so the id is never the
    // handle itself.
    let view = table.inspect(&id).expect("the table knows the id");
    assert_eq!(view.kind(), ResourceKind::Data);
    assert_eq!(view.label_mark(), &personal());

    // An id the table never issued shows nothing.
    let mut other = HandleTable::new();
    let elsewhere = other
        .issue(allow(&photos_for(task("sort-photos"))), NOW)
        .expect("issued");
    assert!(table.inspect(&elsewhere.id()).is_none());
    assert!("not-an-id".parse::<HandleId>().is_err());
}

#[test]
fn the_holder_uses_only_the_rights_the_handle_carries() {
    let mut table = HandleTable::new();
    let handle = table
        .issue(allow(&photos_for(task("sort-photos"))), NOW)
        .expect("issued");

    table
        .exercise(&handle, &task("sort-photos"), Right::Write, NOW)
        .expect("the holder writes");
    assert_eq!(
        table
            .exercise(&handle, &task("sort-photos"), Right::Create, NOW)
            .unwrap_err(),
        Refusal::RightNotHeld(Right::Create)
    );
}

#[test]
fn a_handle_stops_working_at_the_end_of_its_time() {
    let until = Time::from_secs(NOW.as_secs() + 60);
    let mut table = HandleTable::new();
    let mut issue = photos_for(task("sort-photos"));
    issue.limits = EffectLimits::new().until(until);
    let handle = table.issue(allow(&issue), NOW).expect("issued");

    let before = Time::from_secs(until.as_secs() - 1);
    table
        .exercise(&handle, &task("sort-photos"), Right::Read, before)
        .expect("in time");
    assert_eq!(
        table
            .exercise(&handle, &task("sort-photos"), Right::Read, until)
            .unwrap_err(),
        Refusal::Expired(until)
    );
}

#[test]
fn a_handle_that_has_already_expired_is_not_issued() {
    let mut issue = photos_for(task("sort-photos"));
    issue.limits = EffectLimits::new().until(NOW);
    assert!(matches!(
        gate_on_issue(&issue, NOW),
        Ruling::Refused(Refuse { rule: GateRule::Expired, limit: Limit::Until(until) }) if until == NOW
    ));

    // An `Allowed` the broker holds until the time has run out is refused
    // by the table on its own.
    let until = Time::from_secs(NOW.as_secs() + 60);
    issue.limits = EffectLimits::new().until(until);
    let mut table = HandleTable::new();
    assert_eq!(table.issue(allow(&issue), until).unwrap_err(), Refusal::Expired(until));
}

#[test]
fn what_the_gate_allowed_covers_only_that_operation() {
    let mut table = HandleTable::new();
    let mut delegable = photos_for(task("sort-photos"));
    delegable.delegable = true;
    let original = table.issue(allow(&delegable), NOW).expect("issued");
    let compartment = Compartment::fresh();
    let holder = task("sort-photos");
    let to_tool = || Delegation {
        from: &holder,
        compartment: &compartment,
        to: Principal::Tool("photo-sorter".to_string()),
        rights: Rights::from([Right::Read]),
        limits: EffectLimits::new(),
    };
    let Ruling::Allowed(to_pass_on) = gate_on_delegation(&table, &original, &to_tool(), NOW) else {
        panic!("the gate allows the delegation");
    };

    // What allows passing a handle on does not allow issuing one.
    assert_eq!(table.issue(to_pass_on, NOW).unwrap_err(), Refusal::NotAllowed);
    // What allows issuing does not allow passing a handle on.
    assert_eq!(
        table.delegate(&original, to_tool(), spare(), NOW).unwrap_err(),
        Refusal::NotAllowed
    );
    // Nor does what allows passing it on to someone else.
    let Ruling::Allowed(narrower) = gate_on_delegation(
        &table,
        &original,
        &Delegation {
            to: task("rename-files"),
            ..to_tool()
        },
        NOW,
    ) else {
        panic!("the gate allows the delegation");
    };
    assert_eq!(
        table.delegate(&original, to_tool(), narrower, NOW).unwrap_err(),
        Refusal::NotAllowed
    );
}

#[test]
fn a_presented_handle_shows_the_gate_what_the_table_knows() {
    let mut table = HandleTable::new();
    let mut delegable = photos_for(task("sort-photos"));
    delegable.delegable = true;
    delegable.limits = EffectLimits::new().count(5);
    let original = table.issue(allow(&delegable), NOW).expect("issued");
    let compartment = Compartment::fresh();
    let copy = delegate(
        &mut table,
        &original,
        Delegation {
            from: &task("sort-photos"),
            compartment: &compartment,
            to: Principal::Tool("photo-sorter".to_string()),
            rights: Rights::from([Right::Read]),
            limits: EffectLimits::new().count(2),
        },
        NOW,
    )
    .expect("delegated");

    let presented = table.presented(&copy).expect("the table issued it");
    assert_eq!(presented.id, copy.id());
    assert_eq!(presented.holder, Principal::Tool("photo-sorter".to_string()));
    assert_eq!(presented.rights, Rights::from([Right::Read]));
    assert_eq!(presented.limits, EffectLimits::new().count(2));
    assert!(!presented.delegable);
    assert_eq!(presented.ended, None);
    let of = presented.original.expect("a copy names its original");
    assert_eq!(
        (of.id, of.limits, of.ended),
        (original.id(), EffectLimits::new().count(5), false)
    );

    table.revoke(&original.id()).expect("the table knows the original");
    assert_eq!(
        table.presented(&original).expect("still recorded").ended,
        Some(Ending::Revoked)
    );
    assert!(
        table
            .presented(&copy)
            .expect("still recorded")
            .original
            .expect("a copy")
            .ended
    );

    let mut other = HandleTable::new();
    let elsewhere = other.issue(spare(), NOW).expect("issued");
    assert!(table.presented(&elsewhere).is_none());
}

#[test]
fn a_revoked_handle_stops_working_at_once() {
    let mut table = HandleTable::new();
    let handle = table
        .issue(allow(&photos_for(task("sort-photos"))), NOW)
        .expect("issued");

    // The ledger names the handle by its id.
    table.revoke(&handle.id()).expect("the table knows the id");

    assert_eq!(
        table
            .exercise(&handle, &task("sort-photos"), Right::Read, NOW)
            .unwrap_err(),
        Refusal::Revoked
    );
}

#[test]
fn revoking_an_id_the_table_never_issued_is_refused() {
    let mut table = HandleTable::new();
    let mut other = HandleTable::new();
    let elsewhere = other
        .issue(allow(&photos_for(task("sort-photos"))), NOW)
        .expect("issued");

    assert_eq!(table.revoke(&elsewhere.id()).unwrap_err(), Refusal::Unknown);
}

#[test]
fn when_a_task_ends_its_handles_end_with_it_and_it_gets_no_more() {
    let mut table = HandleTable::new();
    let ending = table
        .issue(allow(&photos_for(task("sort-photos"))), NOW)
        .expect("issued");
    let other = table
        .issue(allow(&photos_for(task("rename-files"))), NOW)
        .expect("issued");

    table.end(&task("sort-photos"));

    assert_eq!(
        table
            .exercise(&ending, &task("sort-photos"), Right::Read, NOW)
            .unwrap_err(),
        Refusal::HolderEnded
    );
    assert_eq!(
        table.issue(allow(&photos_for(task("sort-photos"))), NOW).unwrap_err(),
        Refusal::HolderEnded
    );
    table
        .exercise(&other, &task("rename-files"), Right::Read, NOW)
        .expect("another task's handle still works");
}

#[test]
fn when_a_tool_exits_its_handles_end_and_its_next_start_gets_new_ones() {
    let tool = Principal::Tool("morning-pages".to_string());
    let mut table = HandleTable::new();
    let first = table.issue(allow(&photos_for(tool.clone())), NOW).expect("issued");

    table.end(&tool);

    assert_eq!(
        table.exercise(&first, &tool, Right::Read, NOW).unwrap_err(),
        Refusal::HolderEnded
    );
    let next = table
        .issue(allow(&photos_for(tool.clone())), NOW)
        .expect("a Tool starts again");
    table
        .exercise(&next, &tool, Right::Read, NOW)
        .expect("the new start's handle works");
}

#[test]
fn the_agent_is_never_issued_a_handle_it_could_pass_on() {
    let mut delegable = photos_for(Principal::Agent);
    delegable.delegable = true;
    assert!(matches!(
        gate_on_issue(&delegable, NOW),
        Ruling::Refused(Refuse {
            rule: GateRule::ToAgent,
            ..
        })
    ));

    // A handle the Agent cannot pass on, such as a value in a plan, is issued.
    let mut table = HandleTable::new();
    let handle = table.issue(allow(&photos_for(Principal::Agent)), NOW).expect("issued");
    table
        .exercise(&handle, &Principal::Agent, Right::Read, NOW)
        .expect("the Agent reads");
}

#[test]
fn every_refusal_of_the_table_is_a_reason_on_the_gates_list() {
    let until = Time::from_secs(NOW.as_secs() + 60);
    for (refusal, rule, limit) in [
        (Refusal::Unknown, GateRule::Unknown, Limit::None),
        (Refusal::NotHolder, GateRule::Holder, Limit::None),
        (Refusal::Revoked, GateRule::Revoked, Limit::None),
        (Refusal::HolderEnded, GateRule::Ended, Limit::None),
        (Refusal::OriginalEnded, GateRule::Ended, Limit::None),
        (Refusal::Expired(until), GateRule::Expired, Limit::Until(until)),
        (
            Refusal::RightNotHeld(Right::Exec),
            GateRule::Right,
            Limit::Right(Right::Exec),
        ),
        (Refusal::NotDelegable, GateRule::NotDelegable, Limit::None),
        (Refusal::ToAgent, GateRule::ToAgent, Limit::None),
        (Refusal::DelegableToAgent, GateRule::ToAgent, Limit::None),
        (
            Refusal::Widens(crowbar_broker::Widening::Rights),
            GateRule::Right,
            Limit::None,
        ),
        (
            Refusal::Widens(crowbar_broker::Widening::Count(3)),
            GateRule::Limits,
            Limit::Count(3),
        ),
        (Refusal::NotAllowed, GateRule::NotAllowed, Limit::None),
    ] {
        assert_eq!(Refuse::from(refusal), Refuse { rule, limit });
    }
}
