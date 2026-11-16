//! The id of a handle, as the ledger and the Agent see it.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// The id of a handle, as the ledger and the Agent see it: 128 random bits,
/// written as 32 hexadecimal digits. It names a handle for inspection and
/// revocation, and nothing takes it to exercise one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct HandleId(u128);

impl HandleId {
    /// The id with these bits. The broker draws them from the kernel's
    /// random source when it issues a handle. An id confers nothing, so
    /// making one grants nothing.
    pub const fn from_bits(bits: u128) -> Self {
        HandleId(bits)
    }
}

impl fmt::Display for HandleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:032x}", self.0)
    }
}

/// The text is not an id: it is not 32 hexadecimal digits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotAnId;

impl fmt::Display for NotAnId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a handle id is 32 hexadecimal digits")
    }
}

impl std::error::Error for NotAnId {}

impl FromStr for HandleId {
    type Err = NotAnId;

    fn from_str(text: &str) -> Result<Self, NotAnId> {
        if text.len() != 32 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(NotAnId);
        }
        u128::from_str_radix(text, 16).map(HandleId).map_err(|_| NotAnId)
    }
}

impl From<HandleId> for String {
    fn from(id: HandleId) -> Self {
        id.to_string()
    }
}

impl TryFrom<String> for HandleId {
    type Error = NotAnId;

    fn try_from(text: String) -> Result<Self, NotAnId> {
        text.parse()
    }
}
