//! Principals: anything that can hold authority.

use std::fmt;

/// The id of one task, as the broker named it when the task started.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(String);

impl TaskId {
    pub fn new(id: &str) -> Self {
        TaskId(id.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A principal that can hold a handle.
///
/// The broker knows which principal is calling from the connection the Warden
/// gave it (ADR 0016), never from what the caller says.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Principal {
    /// The person's own principal. It coordinates tasks and holds none of
    /// their authority (ADR 0011).
    Agent,
    /// One piece of agent work. A task never starts again: when it ends, it
    /// is gone.
    Task(TaskId),
    /// A Tool, by its identity in the Crowbar State.
    Tool(String),
    /// Installed software, such as a native app or the browser.
    Installed(String),
    /// A per-project workbench (ADR 0017).
    Workbench(String),
}

impl fmt::Display for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Principal::Agent => f.write_str("the Agent"),
            Principal::Task(id) => write!(f, "task {}", id.as_str()),
            Principal::Tool(name) => write!(f, "Tool {name}"),
            Principal::Installed(name) => write!(f, "installed software {name}"),
            Principal::Workbench(name) => write!(f, "workbench {name}"),
        }
    }
}
