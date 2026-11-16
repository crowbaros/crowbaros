//! Helpers the test files share.

// Each test file is its own crate and uses only some of these.
#![allow(dead_code)]

use crowbar_broker::{EffectLimits, Issue, Principal, Resource, Right, Rights, TaskId, Time};
use crowbar_labels::{LabelMark, ObjectId, Origin};

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
