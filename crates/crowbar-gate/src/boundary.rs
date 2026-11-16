//! The undo boundary: which adapter operations stay inside it.

/// The adapter operations that stay inside the undo boundary: within the
/// task's limits, undoable for a stated window, and inside the fleet.
///
/// Every operation not listed here crosses. An adapter adds a row only for
/// an operation it can undo, and testing that each row is true belongs to
/// the checks and rehearsal work.
///
/// | Operation | Why it stays inside |
/// | --- | --- |
/// | `data.read` | A read inside the task's limits does not ask. The reader takes on the label and mark of what it read. |
/// | `data.create` | A new object is staged, and the Data history can remove it for its window. |
/// | `data.write` | A write is staged, and the Data history keeps the earlier version for its window. |
pub const INSIDE: &[&str] = &["data.read", "data.create", "data.write"];

/// Whether `operation` crosses the undo boundary. An operation the table
/// does not list crosses.
pub fn crosses(operation: &str) -> bool {
    !INSIDE.contains(&operation)
}

/// Whether `operation` is an adapter operation's name: `service.operation`,
/// each part lowercase ASCII letters, digits, `-`, or `_`, and not empty.
/// Anything else is an operation the gate does not know.
pub(crate) fn is_named(operation: &str) -> bool {
    let part = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_')
    };
    match operation.split_once('.') {
        Some((service, name)) => part(service) && name.split('.').all(part),
        None => false,
    }
}
