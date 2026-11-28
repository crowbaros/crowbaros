//! Bounded, volatile history for the shell alone (ADR 0039).
use crate::{
    State,
    event_loop::{TimerId, TimerKey, Token},
    json::quote,
};
use fulcrum_wire::protocol::wayland::wl_data_source;
use rustix::{
    io::Errno,
    pipe::{PipeFlags, pipe_with},
};
use std::{
    collections::VecDeque,
    os::fd::{AsFd, OwnedFd},
    time::{Duration, Instant},
};

pub const ITEM_BYTES: usize = 8 * 1024;
pub const TOTAL_BYTES: usize = 64 * 1024;
pub const ITEMS: usize = 50;

#[derive(Default)]
pub(crate) struct History {
    items: VecDeque<String>,
    bytes: usize,
}
impl History {
    fn push(&mut self, text: String) {
        if text.is_empty() || text.len() > ITEM_BYTES || self.items.front() == Some(&text) {
            return;
        }
        self.bytes += text.len();
        self.items.push_front(text);
        while self.items.len() > ITEMS || self.bytes > TOTAL_BYTES {
            self.bytes -= self.items.pop_back().unwrap().len();
        }
    }
    fn clear(&mut self) {
        self.items.clear();
        self.bytes = 0;
    }
    fn reply(&self) -> String {
        let items = self.items.iter().map(|text| quote(text)).collect::<Vec<_>>().join(",");
        format!("{{\"reply\":\"clipboard_history\",\"items\":[{items}]}}")
    }
}
struct Capture {
    generation: u64,
    fd: OwnedFd,
    timer: TimerId,
    bytes: Vec<u8>,
}
#[derive(Default)]
pub(crate) struct Clipboard {
    pub owner: Option<u64>,
    generation: u64,
    history: History,
    capture: Option<Capture>,
}
impl State {
    pub(crate) fn clipboard_start(&mut self, owner: u64) {
        if self.clipboard.owner != Some(owner) {
            self.clear_clipboard_history();
            self.clipboard.owner = Some(owner);
        }
    }
    pub(crate) fn clipboard_history(&self) -> String {
        self.clipboard.history.reply()
    }
    pub(crate) fn clear_clipboard_history(&mut self) {
        self.cancel_clipboard_capture();
        self.clipboard.history.clear();
    }
    pub(crate) fn cancel_clipboard_capture(&mut self) {
        if let Some(capture) = self.clipboard.capture.take() {
            self.event_loop.remove(capture.fd.as_fd());
            self.event_loop.cancel_timer(capture.timer);
        }
    }
    pub(crate) fn clipboard_timeout(&mut self, generation: u64) {
        if self
            .clipboard
            .capture
            .as_ref()
            .is_some_and(|c| c.generation == generation)
        {
            self.cancel_clipboard_capture();
        }
    }
    pub(crate) fn capture_clipboard(&mut self) {
        self.cancel_clipboard_capture();
        if self.locked() || self.clipboard.owner.is_none() {
            return;
        }
        let Some(source) = self.data.selection.and_then(|id| self.data.sources.get(&id)) else {
            return;
        };
        let Some(mime) = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"]
            .into_iter()
            .find(|mime| source.mime_types.iter().any(|m| m == mime))
        else {
            return;
        };
        let (client_id, object) = (source.client, source.object);
        if self.may_hold_descriptor(client_id).is_err() {
            return;
        }
        let Ok((read, write)) = pipe_with(PipeFlags::CLOEXEC) else {
            return;
        };
        // Only our end is nonblocking: clients may write with ordinary I/O.
        if rustix::fs::fcntl_setfl(&read, rustix::fs::OFlags::NONBLOCK).is_err() {
            return;
        }
        if self.event_loop.add(read.as_fd(), Token::Clipboard, false).is_err() {
            return;
        }
        self.clipboard.generation += 1;
        let generation = self.clipboard.generation;
        let timer = self
            .event_loop
            .set_timer(Instant::now() + Duration::from_secs(2), TimerKey::Clipboard(generation));
        self.clipboard.capture = Some(Capture {
            generation,
            fd: read,
            timer,
            bytes: Vec::new(),
        });
        if let Some(client) = self.clients.get_mut(&client_id) {
            client.send(
                object,
                wl_data_source::Event::Send {
                    mime_type: mime.into(),
                    fd: write,
                },
            );
        }
    }
    pub(crate) fn clipboard_ready(&mut self) {
        let Some(capture) = self.clipboard.capture.as_mut() else {
            return;
        };
        let mut buffer = [0u8; 4096];
        loop {
            match rustix::io::read(&capture.fd, &mut buffer) {
                Ok(0) => {
                    let bytes = std::mem::take(&mut capture.bytes);
                    self.cancel_clipboard_capture();
                    if !self.locked()
                        && let Ok(text) = String::from_utf8(bytes)
                    {
                        self.clipboard.history.push(text);
                        self.control_event("clipboard_changed");
                    }
                    return;
                }
                Ok(n) if capture.bytes.len() + n <= ITEM_BYTES => capture.bytes.extend_from_slice(&buffer[..n]),
                Err(Errno::AGAIN) => return,
                Err(Errno::INTR) => continue,
                _ => {
                    self.cancel_clipboard_capture();
                    return;
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_deduplicates_only_consecutive_items_and_keeps_newest_first() {
        let mut history = History::default();
        for s in ["one", "one", "two", "one", ""] {
            history.push(s.into());
        }
        assert_eq!(history.items, ["one", "two", "one"]);
    }
    #[test]
    fn history_caps_count_and_bytes_and_does_not_truncate_large_text() {
        let mut history = History::default();
        for i in 0..100 {
            history.push(i.to_string());
        }
        assert_eq!(history.items.len(), ITEMS);
        assert_eq!(history.items.back().unwrap(), "50");
        history.push("x".repeat(ITEM_BYTES + 1));
        assert_eq!(history.items.front().unwrap(), "99");
        for i in 0..20 {
            history.push(format!("{i:02}{}", "x".repeat(ITEM_BYTES - 2)));
        }
        assert_eq!(history.bytes, TOTAL_BYTES);
        assert_eq!(history.items.len(), 8);
        history.clear();
        assert_eq!(history.bytes, 0);
        assert!(history.items.is_empty());
    }
}
