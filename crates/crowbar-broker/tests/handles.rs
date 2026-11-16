//! A handle is a right for one principal to use one resource. The broker's
//! table is the source of truth: a handle works only for its holder, only for
//! the rights it names, and only while it is live.

use crowbar_broker::{EffectLimits, HandleId, HandleTable, Principal, Refusal, ResourceKind, Right, Time};

mod common;
use common::{NOW, personal, photos_for, task};

#[test]
fn the_holder_uses_its_handle_and_another_principal_cannot() {
    let mut table = HandleTable::new();
    let handle = table.issue(photos_for(task("sort-photos")), NOW).expect("issued");

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
    table.issue(photos_for(task("sort-photos")), NOW).expect("issued");

    // Another table issues the same request, as a forger who knows the
    // request would. Its handle confers nothing here.
    let mut other = HandleTable::new();
    let forged = other.issue(photos_for(task("sort-photos")), NOW).expect("issued");

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
    let handle = table.issue(photos_for(task("sort-photos")), NOW).expect("issued");

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
    let elsewhere = other.issue(photos_for(task("sort-photos")), NOW).expect("issued");
    assert!(table.inspect(&elsewhere.id()).is_none());
    assert!("not-an-id".parse::<HandleId>().is_err());
}

#[test]
fn the_holder_uses_only_the_rights_the_handle_carries() {
    let mut table = HandleTable::new();
    let handle = table.issue(photos_for(task("sort-photos")), NOW).expect("issued");

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
    let handle = table.issue(issue, NOW).expect("issued");

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
    let mut table = HandleTable::new();
    let mut issue = photos_for(task("sort-photos"));
    issue.limits = EffectLimits::new().until(NOW);

    assert_eq!(table.issue(issue, NOW).unwrap_err(), Refusal::Expired(NOW));
}

#[test]
fn a_revoked_handle_stops_working_at_once() {
    let mut table = HandleTable::new();
    let handle = table.issue(photos_for(task("sort-photos")), NOW).expect("issued");

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
    let elsewhere = other.issue(photos_for(task("sort-photos")), NOW).expect("issued");

    assert_eq!(table.revoke(&elsewhere.id()).unwrap_err(), Refusal::Unknown);
}

#[test]
fn when_a_task_ends_its_handles_end_with_it_and_it_gets_no_more() {
    let mut table = HandleTable::new();
    let ending = table.issue(photos_for(task("sort-photos")), NOW).expect("issued");
    let other = table.issue(photos_for(task("rename-files")), NOW).expect("issued");

    table.end(&task("sort-photos"));

    assert_eq!(
        table
            .exercise(&ending, &task("sort-photos"), Right::Read, NOW)
            .unwrap_err(),
        Refusal::HolderEnded
    );
    assert_eq!(
        table.issue(photos_for(task("sort-photos")), NOW).unwrap_err(),
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
    let first = table.issue(photos_for(tool.clone()), NOW).expect("issued");

    table.end(&tool);

    assert_eq!(
        table.exercise(&first, &tool, Right::Read, NOW).unwrap_err(),
        Refusal::HolderEnded
    );
    let next = table.issue(photos_for(tool.clone()), NOW).expect("a Tool starts again");
    table
        .exercise(&next, &tool, Right::Read, NOW)
        .expect("the new start's handle works");
}

#[test]
fn the_agent_is_never_issued_a_handle_it_could_pass_on() {
    let mut table = HandleTable::new();
    let mut delegable = photos_for(Principal::Agent);
    delegable.delegable = true;

    assert_eq!(table.issue(delegable, NOW).unwrap_err(), Refusal::DelegableToAgent);

    // A handle the Agent cannot pass on, such as a value in a plan, is issued.
    let handle = table.issue(photos_for(Principal::Agent), NOW).expect("issued");
    table
        .exercise(&handle, &Principal::Agent, Right::Read, NOW)
        .expect("the Agent reads");
}
