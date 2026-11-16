//! Idempotency keys.

use serde::{Deserialize, Serialize};

/// A key a task attaches to an operation so that it is decided and carried
/// out once. Each task has its own set: the same key from two tasks is two
/// keys.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn new(key: &str) -> Self {
        IdempotencyKey(key.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
