//! One-shot ACP v1 session discovery (`initialize` + `session/list`) and
//! discoverer selection for ACP/OpenCode harnesses.

use crate::discovery::{
    Listing, SessionDiscoverer, meta_indicates_subagent, parse_rfc3339_unix, spec_has_data, watch_specs_from_paths,
};
use crate::harness::{ExternalSession, HarnessConfig};
use crate::opencode;
use crate::sys;
use crate::version::LEVER_VERSION;
use crate::watcher::{StoreWatcher, WatchSpec};
use agent_client_protocol_schema::ProtocolVersion;
use agent_client_protocol_schema::v1::{
    AGENT_METHOD_NAMES, ClientCapabilities, Implementation, InitializeRequest, InitializeResponse, JsonRpcMessage,
    ListSessionsRequest, ListSessionsResponse, Request, RequestId, SessionInfo,
};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::{Duration, Instant};

/// Upper bound for one-shot ACP discovery so a wedged harness never stalls
/// future discovery passes.
const DISCOVERY_RPC_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum pages followed via `nextCursor` in one `session/list` pass.
pub(crate) const MAX_LIST_PAGES: usize = 50;
/// Maximum line length accepted from a one-shot discovery child.
const MAX_DISCOVERY_LINE: usize = 16 * 1024 * 1024;

/// Converts an ACP v1 `SessionInfo` into Lever's harness-agnostic
/// [`ExternalSession`], skipping entries without an ID or absolute `cwd`.
pub(crate) fn session_info_to_external(info: &SessionInfo) -> Option<ExternalSession> {
    let sid = info.session_id.0.as_ref().to_string();
    if sid.is_empty() {
        return None;
    }
    let cwd = info.cwd.to_string_lossy().into_owned();
    if !Path::new(&cwd).is_absolute() {
        return None;
    }
    let title = info
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let updated = info.updated_at.as_deref().and_then(parse_rfc3339_unix);
    let is_subagent = meta_indicates_subagent(info.meta.as_ref());
    Some(ExternalSession {
        harness_session_id: sid,
        cwd,
        title,
        created: None,
        updated,
        stamp: info.updated_at.clone(),
        is_subagent,
    })
}

/// Generic ACP v1 one-shot session discoverer: spawns an ACP harness, calls
/// `initialize` + paginated `session/list`, and immediately stops the child so
/// no harness process stays running for discovery.
pub struct AcpSessionDiscoverer {
    config: HarnessConfig,
    watch_specs: Vec<WatchSpec>,
    opencode_env: bool,
}

impl AcpSessionDiscoverer {
    pub fn new(config: HarnessConfig, watch_specs: Vec<WatchSpec>) -> Self {
        let opencode_env = opencode::is_opencode_config(&config)
            || Path::new(&config.program).file_name().and_then(|n| n.to_str()) == Some("crowbar-stub-harness");
        Self {
            config,
            watch_specs,
            opencode_env,
        }
    }

    /// Runs a single one-shot ACP `initialize` + `session/list` against
    /// `self.config` and immediately terminates the child process.
    pub fn list_via_acp(&self) -> Result<Listing, String> {
        let mut watcher = StoreWatcher::new(self.watch_specs.clone()).ok().flatten();
        let (env_set, env_remove) = if self.opencode_env {
            opencode::build_discovery_env(&self.config)
        } else {
            let env_set: Vec<(String, String)> = self.config.env.iter().filter(|(k, _)| k != "PWD").cloned().collect();
            (env_set, vec!["PWD"])
        };
        let cmd = sys::harness_command(&self.config.program, &self.config.args, &env_set, &env_remove);
        let mut child = sys::WorkerChild::spawn(cmd)
            .map_err(|e| format!("could not start {} to list sessions: {e}", self.config.name))?;
        let mut stdin = child
            .child()
            .stdin
            .take()
            .ok_or_else(|| "discovery child had no stdin pipe".to_string())?;
        let mut stdout = child
            .child()
            .stdout
            .take()
            .ok_or_else(|| "discovery child had no stdout pipe".to_string())?;
        sys::set_nonblocking(stdout.as_raw_fd())
            .map_err(|e| format!("could not set discovery stdout nonblocking: {e}"))?;

        let deadline = Instant::now() + DISCOVERY_RPC_TIMEOUT;
        let mut rpc = OneShotRpc {
            stdin: &mut stdin,
            stdout: &mut stdout,
            read_buf: Vec::new(),
            next_id: 1,
            deadline,
        };

        let outcome = (|| -> Result<Vec<ExternalSession>, String> {
            let init = InitializeRequest::new(ProtocolVersion::V1)
                .client_capabilities(ClientCapabilities::new())
                .client_info(Implementation::new("crowbar-lever", LEVER_VERSION));
            let init_val = rpc.call(AGENT_METHOD_NAMES.initialize, &init)?;
            if let Some(err) = init_val.get("error").filter(|v| !v.is_null()) {
                return Err(format!("initialize failed during discovery: {err}"));
            }
            let init_resp: InitializeResponse =
                serde_json::from_value(init_val.get("result").cloned().unwrap_or(Value::Null))
                    .map_err(|e| format!("invalid initialize response during discovery: {e}"))?;
            if init_resp.protocol_version != ProtocolVersion::V1 {
                return Err("harness uses an unsupported protocol version".to_string());
            }
            if init_resp.agent_capabilities.session_capabilities.list.is_none() {
                watcher = None;
                return Ok(Vec::new());
            }
            rpc.list_all()?;
            if let Some(watcher) = watcher.as_mut() {
                let _ = watcher.drain_changed();
            }
            rpc.list_all()
        })();

        drop(rpc);
        drop(stdin);
        drop(stdout);
        child.finish(Duration::from_millis(300));
        outcome.map(|sessions| Listing { sessions, watcher })
    }
}

struct OneShotRpc<'a> {
    stdin: &'a mut std::process::ChildStdin,
    stdout: &'a mut std::process::ChildStdout,
    read_buf: Vec<u8>,
    next_id: i64,
    deadline: Instant,
}

impl OneShotRpc<'_> {
    fn call<P: serde::Serialize>(&mut self, method: &str, params: &P) -> Result<Value, String> {
        let id = send_acp_request(self.stdin, &mut self.next_id, method, params)?;
        recv_acp_response(self.stdout, &mut self.read_buf, id, self.deadline)
    }

    fn list_all(&mut self) -> Result<Vec<ExternalSession>, String> {
        let mut all = Vec::new();
        let mut seen_ids = HashSet::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let mut req = ListSessionsRequest::new();
            if let Some(ref c) = cursor {
                req = req.cursor(c.clone());
            }
            let val = self.call(AGENT_METHOD_NAMES.session_list, &req)?;
            if let Some(err) = val.get("error").filter(|v| !v.is_null()) {
                return Err(format!("session/list failed: {err}"));
            }
            let resp: ListSessionsResponse = serde_json::from_value(val.get("result").cloned().unwrap_or(Value::Null))
                .map_err(|e| format!("invalid session/list response: {e}"))?;
            if resp.sessions.is_empty() {
                break;
            }
            let mut added = 0;
            for info in resp.sessions {
                let Some(ext) = session_info_to_external(&info) else {
                    continue;
                };
                if !seen_ids.insert(ext.harness_session_id.clone()) {
                    continue;
                }
                all.push(ext);
                added += 1;
            }
            if added == 0 {
                break;
            }
            match resp.next_cursor {
                Some(next) if !next.is_empty() && Some(&next) != cursor.as_ref() => cursor = Some(next),
                _ => break,
            }
        }
        Ok(all)
    }
}

impl SessionDiscoverer for AcpSessionDiscoverer {
    fn watch_specs(&self) -> Vec<WatchSpec> {
        self.watch_specs.clone()
    }

    fn has_store_data(&self) -> bool {
        self.watch_specs.iter().any(spec_has_data)
    }

    fn list_sessions(&self) -> Result<Listing, String> {
        self.list_via_acp()
    }
}

/// Selects the right [`SessionDiscoverer`] for a configured ACP/OpenCode harness.
pub fn discoverer_for_harness(config: &HarnessConfig) -> Option<Box<dyn SessionDiscoverer>> {
    if opencode::is_opencode_config(config) {
        let specs = opencode::watch_specs_for(config);
        let lister = Box::new(AcpSessionDiscoverer::new(config.clone(), specs.clone()));
        return Some(Box::new(opencode::OpenCodeSessionDiscoverer::new(
            config.clone(),
            specs,
            lister,
        )));
    }
    let specs = watch_specs_from_paths(&config.watch_paths);
    if specs.is_empty() {
        return None;
    }
    Some(Box::new(AcpSessionDiscoverer::new(config.clone(), specs)))
}

fn send_acp_request<P: serde::Serialize>(
    stdin: &mut impl Write,
    next_id: &mut i64,
    method: &str,
    params: &P,
) -> Result<i64, String> {
    let id = *next_id;
    *next_id += 1;
    let msg = JsonRpcMessage::wrap(Request {
        id: RequestId::Number(id),
        method: method.into(),
        params: Some(params),
    });
    let line = serde_json::to_string(&msg).map_err(|e| format!("could not encode discovery RPC: {e}"))?;
    stdin
        .write_all(line.as_bytes())
        .and_then(|()| stdin.write_all(b"\n"))
        .and_then(|()| stdin.flush())
        .map_err(|e| format!("could not write discovery RPC: {e}"))?;
    Ok(id)
}

fn recv_acp_response(
    stdout: &mut std::process::ChildStdout,
    buf: &mut Vec<u8>,
    expected_id: i64,
    deadline: Instant,
) -> Result<Value, String> {
    let mut chunk = [0u8; 8192];
    loop {
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line = buf[..pos].to_vec();
            buf.drain(..=pos);
            let Ok(text) = std::str::from_utf8(&line) else {
                continue;
            };
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            if val.get("id").and_then(Value::as_i64) == Some(expected_id) && val.get("method").is_none() {
                return Ok(val);
            }
        }
        if buf.len() > MAX_DISCOVERY_LINE {
            return Err("discovery harness wrote a line longer than 16 MiB".to_string());
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err("timed out waiting for harness to list sessions".to_string());
        };
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
        let mut pfd = [libc::pollfd {
            fd: stdout.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        sys::poll(&mut pfd, timeout_ms).map_err(|e| format!("discovery poll failed: {e}"))?;
        match stdout.read(&mut chunk) {
            Ok(0) => return Err("discovery harness exited before answering".to_string()),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("could not read discovery harness: {e}")),
        }
    }
}
