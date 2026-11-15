//! The labels and marks of stored objects.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;

use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::mark::LabelMark;
use crate::origin::Origin;

/// The label and mark of each stored object, by the object's name.
///
/// Marks persist in storage (ADR 0008): an object keeps the label and mark of
/// whatever wrote it, and a later write joins with them, so a write can add to
/// them and never take them away. An object with no record is an unknown
/// origin. Saved and loaded through serde, it keeps every label and mark;
/// loading refuses a record that names an object twice.
///
/// This holds the records only. Where they live, and who may write them, is
/// for the Data layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct LabelStore {
    records: BTreeMap<String, LabelMark>,
}

impl LabelStore {
    /// Records a write to `object` by something at `writer`'s label and mark.
    pub fn write(&mut self, object: &str, writer: &LabelMark) {
        match self.records.entry(object.to_string()) {
            Entry::Vacant(entry) => {
                entry.insert(writer.clone());
            }
            Entry::Occupied(mut entry) => {
                let joined = entry.get().join(writer);
                entry.insert(joined);
            }
        }
    }

    /// Where reading `object` comes from: a stored result with its record, or
    /// an unknown origin when there is no record.
    pub fn origin_of(&self, object: &str) -> Origin {
        match self.records.get(object) {
            Some(record) => Origin::Stored(record.clone()),
            None => Origin::Unknown,
        }
    }
}

impl<'de> Deserialize<'de> for LabelStore {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(RecordsVisitor)
    }
}

struct RecordsVisitor;

impl<'de> Visitor<'de> for RecordsVisitor {
    type Value = LabelStore;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a map from object names to their label and mark")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<LabelStore, A::Error> {
        let mut records = BTreeMap::new();
        while let Some((object, record)) = map.next_entry::<String, LabelMark>()? {
            if records.contains_key(&object) {
                return Err(serde::de::Error::custom(format!(
                    "the object {object:?} is recorded twice"
                )));
            }
            records.insert(object, record);
        }
        Ok(LabelStore { records })
    }
}
