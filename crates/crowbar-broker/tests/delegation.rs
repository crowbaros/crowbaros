//! A holder may pass a handle on only when it was issued delegable, and only
//! narrowed. The copy keeps the label and mark of what it names, takes on the
//! delegator's, ends when the original ends, and is never delegated again. No
//! handle passes to the Agent, which holds none of its tasks' authority
//! (ADR 0011).

use crowbar_broker::{
    Delegation, EffectLimits, Handle, HandleTable, Principal, Refusal, Right, Rights, Time, Widening,
};
use crowbar_labels::{Category, Compartment, Label, MailAccount, MarkSource, Origin};

mod common;
use common::{NOW, personal, photos_for, task};

fn tool() -> Principal {
    Principal::Tool("photo-sorter".to_string())
}

/// A table with one handle on the photos, held by the task `sort-photos`.
fn table_with(delegable: bool) -> (HandleTable, Handle) {
    let mut table = HandleTable::new();
    let mut issue = photos_for(task("sort-photos"));
    issue.delegable = delegable;
    let original = table.issue(issue, NOW).expect("issued");
    (table, original)
}

/// `from`, holding nothing, passes `handle` to `to` with read only and no
/// limits.
fn pass(table: &mut HandleTable, handle: &Handle, from: &Principal, to: Principal) -> Result<Handle, Refusal> {
    table.delegate(
        handle,
        Delegation {
            from,
            held: &Compartment::fresh(),
            to,
            rights: Rights::from([Right::Read]),
            limits: EffectLimits::new(),
        },
        NOW,
    )
}

#[test]
fn a_delegable_handle_passes_narrowed_and_both_work() {
    let mut table = HandleTable::new();
    let mut issue = photos_for(task("sort-photos"));
    issue.delegable = true;
    let original = table.issue(issue, NOW).expect("issued");

    let copy = table
        .delegate(
            &original,
            Delegation {
                from: &task("sort-photos"),
                held: &Compartment::fresh(),
                to: tool(),
                rights: Rights::from([Right::Read]),
                limits: EffectLimits::new(),
            },
            NOW,
        )
        .expect("delegated");

    let used = table
        .exercise(&copy, &tool(), Right::Read, NOW)
        .expect("the Tool reads");
    assert_eq!(used.rights(), &Rights::from([Right::Read]));
    // The copy names its original, so the gate counts its uses against the
    // original's limits too.
    assert_eq!(used.original(), Some(original.id()));
    table
        .exercise(&original, &task("sort-photos"), Right::Write, NOW)
        .expect("the original still works");
}

#[test]
fn a_handle_not_issued_delegable_is_not_passed_on() {
    let (mut table, original) = table_with(false);

    assert_eq!(
        pass(&mut table, &original, &task("sort-photos"), tool()).unwrap_err(),
        Refusal::NotDelegable
    );
}

#[test]
fn a_copy_never_carries_more_rights_or_wider_limits_than_the_original() {
    let until = Time::from_secs(NOW.as_secs() + 60);
    let mut table = HandleTable::new();
    let mut issue = photos_for(task("sort-photos"));
    issue.delegable = true;
    issue.limits = EffectLimits::new().count(10).until(until);
    let original = table.issue(issue, NOW).expect("issued");

    let mut copy = |rights: Rights, limits: EffectLimits| {
        table.delegate(
            &original,
            Delegation {
                from: &task("sort-photos"),
                held: &Compartment::fresh(),
                to: tool(),
                rights,
                limits,
            },
            NOW,
        )
    };
    let read = || Rights::from([Right::Read]);

    // A right the original does not carry.
    assert_eq!(
        copy(
            Rights::from([Right::Read, Right::Create]),
            EffectLimits::new().count(10).until(until)
        )
        .unwrap_err(),
        Refusal::Widens(Widening::Rights)
    );
    // More objects than the original allows.
    assert_eq!(
        copy(read(), EffectLimits::new().count(11).until(until)).unwrap_err(),
        Refusal::Widens(Widening::Count)
    );
    // No end, where the original has one.
    assert_eq!(
        copy(read(), EffectLimits::new().count(10)).unwrap_err(),
        Refusal::Widens(Widening::Until)
    );
    // Narrow, but its time has already run out.
    assert_eq!(
        copy(read(), EffectLimits::new().count(3).until(NOW)).unwrap_err(),
        Refusal::Expired(NOW)
    );
    copy(read(), EffectLimits::new().count(3).until(until)).expect("a narrower copy passes");
}

#[test]
fn no_handle_passes_to_the_agent() {
    let (mut table, original) = table_with(true);

    assert_eq!(
        pass(&mut table, &original, &task("sort-photos"), Principal::Agent).unwrap_err(),
        Refusal::ToAgent
    );
}

#[test]
fn a_copy_carries_the_label_and_mark_of_the_compartment_that_passed_it() {
    let (mut table, original) = table_with(true);
    // The task has read a mail, so what it emits is private and marked.
    let me = MailAccount::new("me@mail.example");
    let mail = Origin::Mail { account: me.clone() };
    let mut held = Compartment::fresh();
    held.read(&mail.label_mark()).expect("a task reads its mail");

    let copy = table
        .delegate(
            &original,
            Delegation {
                from: &task("sort-photos"),
                held: &held,
                to: tool(),
                rights: Rights::from([Right::Read]),
                limits: EffectLimits::new(),
            },
            NOW,
        )
        .expect("delegated");

    let shown = table.inspect(&copy.id()).expect("the table knows the copy");
    // Personal photos joined with private mail is private, in messages.
    assert_eq!(shown.label_mark().label(), &Label::private(Category::Messages));
    assert_eq!(
        shown.label_mark().mark().sources().collect::<Vec<_>>(),
        vec![&MarkSource::Mail(me)]
    );
    // The original is as it was issued.
    let original_shown = table.inspect(&original.id()).expect("the table knows the original");
    assert_eq!(original_shown.label_mark(), &personal());
}

#[test]
fn a_copy_ends_when_the_original_is_revoked() {
    let (mut table, original) = table_with(true);
    let copy = pass(&mut table, &original, &task("sort-photos"), tool()).expect("delegated");

    table.revoke(&original.id()).expect("the table knows the original");

    assert_eq!(
        table.exercise(&copy, &tool(), Right::Read, NOW).unwrap_err(),
        Refusal::OriginalEnded
    );
}

#[test]
fn a_copy_ends_when_the_task_that_passed_it_ends() {
    let (mut table, original) = table_with(true);
    let copy = pass(&mut table, &original, &task("sort-photos"), tool()).expect("delegated");

    table.end(&task("sort-photos"));

    assert_eq!(
        table.exercise(&copy, &tool(), Right::Read, NOW).unwrap_err(),
        Refusal::OriginalEnded
    );
}

#[test]
fn revoking_a_copy_leaves_the_original_working() {
    let (mut table, original) = table_with(true);
    let copy = pass(&mut table, &original, &task("sort-photos"), tool()).expect("delegated");

    table.revoke(&copy.id()).expect("the table knows the copy");

    table
        .exercise(&original, &task("sort-photos"), Right::Read, NOW)
        .expect("the original still works");
}

#[test]
fn a_copy_is_never_passed_on_again() {
    let (mut table, original) = table_with(true);
    let copy = pass(&mut table, &original, &task("sort-photos"), tool()).expect("delegated");

    assert_eq!(
        pass(&mut table, &copy, &tool(), task("rename-files")).unwrap_err(),
        Refusal::NotDelegable
    );
}

#[test]
fn only_a_live_holder_passes_a_handle_on_and_only_to_a_task_that_has_not_ended() {
    let (mut table, original) = table_with(true);

    assert_eq!(
        pass(&mut table, &original, &task("other-task"), tool()).unwrap_err(),
        Refusal::NotHolder
    );

    table.end(&task("finished"));
    assert_eq!(
        pass(&mut table, &original, &task("sort-photos"), task("finished")).unwrap_err(),
        Refusal::HolderEnded
    );

    table.revoke(&original.id()).expect("the table knows the original");
    assert_eq!(
        pass(&mut table, &original, &task("sort-photos"), tool()).unwrap_err(),
        Refusal::Revoked
    );
}
