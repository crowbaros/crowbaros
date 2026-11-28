//! Non-blocking stdio JSON-RPC 2.0 transport and child-process lifecycle for ACP.

use crate::sys;
use agent_client_protocol_schema::v1::{Error as AcpError, JsonRpcMessage, Notification, Request, RequestId, Response};
use serde_json::Value;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::process::{Child, ChildStdin, ChildStdout};
use std::time::{Duration, Instant};

/// Most bytes Lever reads from the harness in one wake. A harness that never
/// stops writing then holds back neither Lever's peers nor a stop signal, and
/// a peer that watches a thread is handed its events a part at a time, with
/// the chance to read them in between.
const READ_BUDGET: usize = 256 * 1024;
/// The longest line Lever takes from the harness. ACP is a message to a
/// line, so a harness that writes more than this with no line end is not
/// speaking it, and Lever would otherwise keep all of it in memory.
const MAX_LINE: usize = 16 * 1024 * 1024;
const LONG_LINE: &str = "wrote a line longer than 16 MiB";

/// Non-blocking stdio JSON-RPC 2.0 transport owning the harness child process.
pub(super) struct AcpTransport {
    child: Child,
    /// When the harness started, so what it left behind is told from what
    /// an earlier one did.
    started: u64,
    stdin: Option<ChildStdin>,
    stdout: ChildStdout,
    write_buf: Vec<u8>,
    read_buf: Vec<u8>,
    /// How much of `read_buf` is known to hold no line end, so a long line
    /// is looked through once and not again at each read.
    scanned: usize,
    next_id: i64,
    /// Why Lever is ending this harness, when the harness did not end itself.
    fault: Option<&'static str>,
}

impl AcpTransport {
    pub(super) fn spawn(
        name: &str,
        program: &str,
        args: &[String],
        env_set: &[(String, String)],
        env_remove: &[&str],
    ) -> Result<Self, String> {
        let mut child = sys::spawn_harness(program, args, env_set, env_remove)
            .map_err(|e| format!("could not start harness {name}: {e}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "harness child had no stdin pipe".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "harness child had no stdout pipe".to_string())?;
        if let Err(e) = sys::set_nonblocking(stdin.as_raw_fd()).and_then(|()| sys::set_nonblocking(stdout.as_raw_fd()))
        {
            // The child is running and nothing will ever speak to it.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not set the pipes of harness {name} nonblocking: {e}"));
        }
        let started = sys::start_time(child.id()).unwrap_or(0);
        Ok(Self {
            child,
            started,
            stdin: Some(stdin),
            stdout,
            write_buf: Vec::new(),
            read_buf: Vec::new(),
            scanned: 0,
            next_id: 1,
            fault: None,
        })
    }

    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    pub(super) fn stdout_fd(&self) -> RawFd {
        self.stdout.as_raw_fd()
    }

    pub(super) fn stdin_fd(&self) -> Option<RawFd> {
        self.stdin.as_ref().map(AsRawFd::as_raw_fd)
    }

    pub(super) fn wants_write(&self) -> bool {
        !self.write_buf.is_empty() && self.stdin.is_some()
    }

    pub(super) fn fault(&self) -> Option<&'static str> {
        self.fault
    }

    pub(super) fn send_request<P: serde::Serialize>(&mut self, method: &str, params: &P) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        let msg = JsonRpcMessage::wrap(Request {
            id: RequestId::Number(id),
            method: method.into(),
            params: Some(params),
        });
        self.queue_json(&serde_json::to_value(msg).expect("serialize JSON-RPC request"));
        id
    }

    pub(super) fn send_notification<P: serde::Serialize>(&mut self, method: &str, params: &P) {
        let msg = JsonRpcMessage::wrap(Notification {
            method: method.into(),
            params: Some(params),
        });
        self.queue_json(&serde_json::to_value(msg).expect("serialize JSON-RPC notification"));
    }

    pub(super) fn send_result_response<R: serde::Serialize>(&mut self, id: RequestId, result: R) {
        let msg = JsonRpcMessage::wrap(Response::<R, AcpError>::new(id, Ok(result)));
        self.queue_json(&serde_json::to_value(msg).expect("serialize JSON-RPC result response"));
    }

    pub(super) fn send_error_response(&mut self, id: RequestId, err: AcpError) {
        let msg = JsonRpcMessage::wrap(Response::<Value, AcpError>::new(id, Err(err)));
        self.queue_json(&serde_json::to_value(msg).expect("serialize JSON-RPC error response"));
    }

    fn queue_json(&mut self, value: &Value) {
        let line = value.to_string();
        self.write_buf.extend_from_slice(line.as_bytes());
        self.write_buf.push(b'\n');
    }

    /// Flushes queued JSON-RPC bytes to the child's stdin. Returns `false` if
    /// writing failed permanently.
    pub(super) fn on_writable(&mut self) -> bool {
        let Some(stdin) = self.stdin.as_mut() else {
            return false;
        };
        while !self.write_buf.is_empty() {
            match stdin.write(&self.write_buf) {
                Ok(0) => return false,
                Ok(n) => {
                    self.write_buf.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
        true
    }

    /// Reads JSON-RPC messages from stdout up to `READ_BUDGET`, invoking
    /// `on_message` for each parsed JSON value. Returns `true` when the child
    /// exited or hit a fatal protocol fault.
    pub(super) fn read_messages(&mut self, mut on_message: impl FnMut(Value)) -> bool {
        let mut buf = [0u8; 16384];
        let mut taken = 0;
        while taken < READ_BUDGET {
            match self.stdout.read(&mut buf) {
                Ok(0) => {
                    self.drain_lines(&mut on_message);
                    return true;
                }
                Ok(n) => {
                    taken += n;
                    self.read_buf.extend_from_slice(&buf[..n]);
                    self.drain_lines(&mut on_message);
                    if self.read_buf.len() > MAX_LINE {
                        self.fault = Some(LONG_LINE);
                    }
                    if self.fault.is_some() {
                        return true;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return true,
            }
        }
        false
    }

    fn drain_lines(&mut self, on_message: &mut impl FnMut(Value)) {
        while let Some(found) = self.read_buf[self.scanned..].iter().position(|&b| b == b'\n') {
            let pos = self.scanned + found;
            if pos > MAX_LINE {
                self.fault = Some(LONG_LINE);
                return;
            }
            let line = self.read_buf[..pos].to_vec();
            self.read_buf.drain(..=pos);
            self.scanned = 0;
            let Ok(text) = std::str::from_utf8(&line) else {
                continue;
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            on_message(value);
        }
        self.scanned = self.read_buf.len();
    }

    /// Ends the harness and every process under it at once.
    pub(super) fn kill_tree(mut self) {
        self.stdin.take();
        let pid = self.child.id();
        sys::kill_all_of_harness(pid, self.started);
        let _ = self.child.kill();
        let _ = self.child.wait();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            sys::reap_orphans(None);
            if sys::live_of_harness(pid, self.started).is_empty() || Instant::now() >= deadline {
                break;
            }
            sys::kill_all_of_harness(pid, self.started);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Gracefully stops and reaps the harness child process.
    pub(super) fn stop(mut self) {
        self.stdin.take();
        if self.wait_exit(Duration::from_millis(200)) {
            return;
        }
        sys::signal_pid(self.child.id(), libc::SIGTERM);
        if self.wait_exit(Duration::from_millis(500)) {
            return;
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn wait_exit(&mut self, patience: Duration) -> bool {
        let deadline = Instant::now() + patience;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                Ok(None) => return false,
                Err(_) => return true,
            }
        }
    }
}
