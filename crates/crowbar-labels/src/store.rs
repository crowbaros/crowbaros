//! The labels and marks of stored objects.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::label::Label;
use crate::mark::{LabelMark, Mark};
use crate::origin::Origin;

/// The id of a stored object, as the Data layer names it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(String);

impl ObjectId {
    pub fn new(id: &str) -> Self {
        ObjectId(id.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The label and mark of each stored object, by the object's id.
///
/// Marks persist in storage (ADR 0008). A new object starts at the label and
/// mark of whatever created it ([`LabelStore::create`]). A later write joins
/// with them ([`LabelStore::write`]), so a write can add to them and never take
/// them away. An object with no record is an unknown origin, and a write to it
/// joins with the unknown row. Saved and loaded through serde, the store keeps
/// every label and mark; loading refuses a record that names an object twice.
///
/// This holds the records only. Where they live, and who may write them, is
/// for the Data layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct LabelStore {
    records: BTreeMap<ObjectId, LabelMark>,
}

impl LabelStore {
    /// Records a new object that something at `writer`'s label and mark
    /// wrote. Only the broker knows an object is new, so only it calls this.
    /// An object that already has a record is refused.
    pub fn create(&mut self, object: &ObjectId, writer: &LabelMark) -> Result<(), AlreadyRecorded> {
        match self.records.entry(object.clone()) {
            Entry::Occupied(_) => Err(AlreadyRecorded { object: object.clone() }),
            Entry::Vacant(entry) => {
                entry.insert(writer.clone());
                Ok(())
            }
        }
    }

    /// Records a write to `object` by something at `writer`'s label and mark.
    /// The write joins with what the object holds: its record, or the unknown
    /// row when it has none, because an object with no record may have been on
    /// disk before any record was kept (ADR 0019).
    pub fn write(&mut self, object: &ObjectId, writer: &LabelMark) {
        let held = self.origin_of(object).label_mark();
        self.records.insert(object.clone(), held.join(writer));
    }

    /// Where reading `object` comes from: a stored result with its record, or
    /// an unknown origin when there is no record.
    pub fn origin_of(&self, object: &ObjectId) -> Origin {
        match self.records.get(object) {
            Some(record) => Origin::Stored(record.clone()),
            None => Origin::Unknown,
        }
    }
}

/// [`LabelStore::create`] was given an object that already has a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlreadyRecorded {
    object: ObjectId,
}

impl AlreadyRecorded {
    pub fn object(&self) -> &ObjectId {
        &self.object
    }
}

impl fmt::Display for AlreadyRecorded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the object {:?} already has a record; a write joins with it", self.object.as_str())
    }
}

impl std::error::Error for AlreadyRecorded {}

impl<'de> Deserialize<'de> for LabelStore {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(RecordsVisitor)
    }
}

/// One stored record as it is loaded: both fields are required, and any other
/// field is refused.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRecord {
    label: Label,
    mark: Mark,
}

struct RecordsVisitor;

impl<'de> Visitor<'de> for RecordsVisitor {
    type Value = LabelStore;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a map from object names to their label and mark")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<LabelStore, A::Error> {
        let mut records = BTreeMap::new();
        while let Some((object, record)) = map.next_entry::<ObjectId, StoredRecord>()? {
            if records.contains_key(&object) {
                return Err(serde::de::Error::custom(format!(
                    "the object {object:?} is recorded twice"
                )));
            }
            records.insert(object, LabelMark::new(record.label, record.mark));
        }
        Ok(LabelStore { records })
    }
}
