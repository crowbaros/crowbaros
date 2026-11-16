//! Resources, and the rights a handle carries on one.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use crowbar_labels::{ObjectId, Site};
use serde::{Deserialize, Serialize};

/// The one thing a handle names. "Network" is never a resource: a handle
/// names one host and port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resource {
    /// A Data object, or a set of them, such as "this person's photos".
    Data(BTreeSet<ObjectId>),
    /// A path in a sandbox's view.
    Path(PathBuf),
    /// A device, by the name Base gives it.
    Device(String),
    /// One host and port the broker connects to.
    Endpoint { host: Site, port: u16 },
    /// A portal session, such as a file chosen in the powerbox.
    Portal(String),
    /// An operation of one service, carried out by its adapter.
    Service(String),
    /// A value the plan interpreter holds, named without being revealed.
    Value(String),
}

/// The kind of resource a handle names. The table states it without naming
/// the resource itself, which may carry a name a stranger chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    Data,
    Path,
    Device,
    Endpoint,
    Portal,
    Service,
    Value,
}

impl Resource {
    pub fn kind(&self) -> ResourceKind {
        match self {
            Resource::Data(_) => ResourceKind::Data,
            Resource::Path(_) => ResourceKind::Path,
            Resource::Device(_) => ResourceKind::Device,
            Resource::Endpoint { .. } => ResourceKind::Endpoint,
            Resource::Portal(_) => ResourceKind::Portal,
            Resource::Service(_) => ResourceKind::Service,
            Resource::Value(_) => ResourceKind::Value,
        }
    }
}

/// One operation a handle allows on its resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Right {
    Read,
    Write,
    Create,
    Exec,
    Connect,
}

impl fmt::Display for Right {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Right::Read => "read",
            Right::Write => "write",
            Right::Create => "create",
            Right::Exec => "execute",
            Right::Connect => "connect",
        })
    }
}

/// The rights a handle carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Rights(BTreeSet<Right>);

impl Rights {
    pub fn contains(&self, right: Right) -> bool {
        self.0.contains(&right)
    }

    /// Whether every right here is in `other`.
    pub fn is_subset(&self, other: &Rights) -> bool {
        self.0.is_subset(&other.0)
    }

    pub fn iter(&self) -> impl Iterator<Item = Right> + '_ {
        self.0.iter().copied()
    }
}

impl<const N: usize> From<[Right; N]> for Rights {
    fn from(rights: [Right; N]) -> Self {
        Rights(rights.into())
    }
}
