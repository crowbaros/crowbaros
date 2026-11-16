//! Helpers the test files share.

// Each test file is its own crate and uses only some of these.
#![allow(dead_code)]

use crowbar_authority::Authority;
use crowbar_broker::{
    Delegation, EffectLimits, Handle, HandleTable, Issue, Principal, Refusal, Resource, Right, Rights, TaskId, Time,
};
use crowbar_capability::Refuse;
use crowbar_gate::{Act, Allowed, Caller, Gate, InMemory, Operation, Ruling};
use crowbar_labels::{Compartment, LabelMark, LabelStore, ObjectId, Origin};

/// The time every test starts at.
pub const NOW: Time = Time::from_secs(1_000);

pub fn task(name: &str) -> Principal {
    Principal::Task(TaskId::new(name))
}

pub fn photo(name: &str) -> ObjectId {
    ObjectId::new(&format!("photos/{name}"))
}

/// The label and mark of something the person wrote: personal, unmarked.
pub fn personal() -> LabelMark {
    Origin::Typed.label_mark()
}

/// A handle to read and write two photos, held by `holder`, with no limits
/// beyond the resource and the rights.
pub fn photos_for(holder: Principal) -> Issue {
    Issue {
        holder,
        resource: Resource::Data([photo("cat.jpg"), photo("dog.jpg")].into()),
        rights: Rights::from([Right::Read, Right::Write]),
        limits: EffectLimits::new(),
        label_mark: personal(),
        delegable: false,
        reason: "Sorts your photos by date.".to_string(),
    }
}

/// The photos are the person's own: personal, unmarked.
fn labels() -> LabelStore {
    let mut labels = LabelStore::default();
    for name in ["cat.jpg", "dog.jpg"] {
        labels.create(&photo(name), &personal()).expect("new");
    }
    labels
}

/// A gate with nothing counted, under authority that grants nothing.
fn gate() -> Gate<InMemory> {
    Gate::start(InMemory::default(), &[])
}

/// What the gate rules on issuing `issue` at `now`.
pub fn gate_on_issue(issue: &Issue, now: Time) -> Ruling {
    let compartment = Compartment::fresh();
    let operation = Operation {
        caller: Caller {
            principal: &issue.holder,
            standing_task: None,
            compartment: &compartment,
        },
        key: None,
        act: Act::Issue {
            issue,
            labels: &labels(),
        },
    };
    gate().decide(Some(&Authority::default()), &operation, now)
}

/// What the gate gives to allow issuing `issue` at [`NOW`].
pub fn allow(issue: &Issue) -> Allowed {
    match gate_on_issue(issue, NOW) {
        Ruling::Allowed(allowed) => allowed,
        other => panic!("the gate did not allow the issue: {other:?}"),
    }
}

/// An `Allowed` for something else: an issue of photos to another task.
pub fn spare() -> Allowed {
    allow(&photos_for(task("spare")))
}

/// Passes `handle` on as the broker does: the gate decides on what the
/// table states about the handle, and the table passes it on with the
/// gate's `Allowed`.
///
/// Where the gate refuses, the table is given an `Allowed` for something
/// else, to show that it refuses on its own, and under the same rule.
pub fn delegate(
    table: &mut HandleTable,
    handle: &Handle,
    delegation: Delegation<'_>,
    now: Time,
) -> Result<Handle, Refusal> {
    match gate_on_delegation(table, handle, &delegation, now) {
        Ruling::Allowed(allowed) => table.delegate(handle, delegation, allowed, now),
        Ruling::Refused(gate) => {
            let refusal = table
                .delegate(handle, delegation, spare(), now)
                .expect_err("the table refuses what the gate refused");
            assert_eq!(
                Refuse::from(refusal.clone()).rule,
                gate.rule,
                "the table and the gate disagree"
            );
            Err(refusal)
        }
        other => panic!("the gate asked: {other:?}"),
    }
}

/// What the gate rules on `delegation` of `handle`, from what the table
/// states about the handle.
pub fn gate_on_delegation(table: &HandleTable, handle: &Handle, delegation: &Delegation<'_>, now: Time) -> Ruling {
    let presented = table.presented(handle);
    let operation = Operation {
        caller: Caller {
            principal: delegation.from,
            standing_task: None,
            compartment: delegation.compartment,
        },
        key: None,
        act: Act::Delegate {
            handle: presented.as_ref(),
            to: &delegation.to,
            rights: &delegation.rights,
            limits: &delegation.limits,
        },
    };
    gate().decide(Some(&Authority::default()), &operation, now)
}
