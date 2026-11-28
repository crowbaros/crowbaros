//! History request state is separate from rendering and compositor I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Read,
    Clear,
}

#[derive(Default)]
pub struct History {
    pub items: Vec<String>,
    pub pending: Option<Operation>,
    pub failed: Option<Operation>,
    refresh_again: bool,
}
impl History {
    pub fn begin(&mut self, operation: Operation) -> bool {
        if self.pending.is_some() {
            self.refresh_again |= operation == Operation::Read;
            return false;
        }
        self.pending = Some(operation);
        self.failed = None;
        true
    }
    pub fn finish(&mut self, result: Result<Vec<String>, ()>) -> bool {
        let operation = self.pending.take().expect("only an active request can finish");
        match result {
            Ok(items) => self.items = items,
            Err(()) => self.failed = Some(operation),
        }
        std::mem::take(&mut self.refresh_again)
    }
    pub fn can_copy(&self) -> bool {
        self.pending.is_none() && self.failed.is_none()
    }
    pub fn status(&self) -> Option<&'static str> {
        match (self.pending, self.failed) {
            (Some(Operation::Read), _) => Some(if self.items.is_empty() {
                "Reading history…"
            } else {
                "Refreshing history…"
            }),
            (Some(Operation::Clear), _) => Some("Clearing history…"),
            (_, Some(Operation::Read)) => Some(if self.items.is_empty() {
                "Could not read history. Close and reopen to try again."
            } else {
                "Could not refresh history. Close and reopen to try again."
            }),
            (_, Some(Operation::Clear)) => Some("Clear was not confirmed. Try Clear again."),
            _ => None,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn populated() -> History {
        History {
            items: vec!["earlier copy".into()],
            ..Default::default()
        }
    }
    #[test]
    fn failed_refresh_keeps_rows_but_reports_failure_and_blocks_stale_copy() {
        let mut h = populated();
        assert!(h.begin(Operation::Read));
        assert!(!h.can_copy());
        assert!(!h.finish(Err(())));
        assert_eq!(h.items, ["earlier copy"]);
        assert_eq!(h.failed, Some(Operation::Read));
        assert!(h.status().is_some());
        assert!(!h.can_copy());
    }
    #[test]
    fn clear_cannot_look_successful_before_confirmation_and_can_be_retried() {
        let mut h = populated();
        assert!(h.begin(Operation::Clear));
        assert_eq!(h.items, ["earlier copy"]);
        h.finish(Err(()));
        assert_eq!(h.items, ["earlier copy"]);
        assert_eq!(h.failed, Some(Operation::Clear));
        assert!(!h.can_copy());
        assert!(h.begin(Operation::Clear));
        h.finish(Ok(vec![]));
        assert!(h.items.is_empty());
        assert!(h.failed.is_none());
        assert!(h.can_copy());
    }
    #[test]
    fn changes_during_a_request_coalesce_into_one_refresh() {
        let mut h = populated();
        assert!(h.begin(Operation::Read));
        for _ in 0..20 {
            assert!(!h.begin(Operation::Read));
        }
        assert!(h.finish(Ok(vec!["new copy".into()])));
        assert!(h.begin(Operation::Read));
        assert!(!h.finish(Ok(vec!["latest copy".into()])));
        assert_eq!(h.items, ["latest copy"]);
    }
}
