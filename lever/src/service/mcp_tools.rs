//! MCP tool socket auth/session tokens and project-folder actions
//! (`list-dirs`, `make-project`, and `mcp-call`).

use super::Service;
use super::turns::TurnDispatch;
use super::wire::{Broadcast, RequestOutcome, default_cwd, error_outcome, store_failed, unix_now};
use crate::log::Log;
use crate::mcp::{self, McpServerSpec};
use crate::project;
use crate::store::ThreadMeta;
use crate::sys;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(super) struct McpTools {
    /// Path to this `crowbar-lever` executable resolved once at startup so
    /// spawning `mcp-bridge` still works if the binary is replaced on disk.
    self_exe: PathBuf,
    /// Private Unix socket served to `crowbar-lever mcp-bridge` subprocesses.
    tool_socket: PathBuf,
    /// Harness-lifetime authentication token given to MCP bridges spawned by
    /// the current harness process.
    auth: String,
    /// Maps each opaque per-session MCP token to its owning Lever thread ID.
    sessions: HashMap<String, String>,
}

impl McpTools {
    pub(super) fn new(tool_socket: PathBuf) -> Self {
        let self_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/usr/local/bin/crowbar-lever"));
        Self {
            self_exe,
            tool_socket,
            auth: String::new(),
            sessions: HashMap::new(),
        }
    }

    pub(super) fn reset_for_harness(&mut self) {
        self.auth = sys::random_hex(16);
        self.sessions.clear();
    }

    pub(super) fn clear(&mut self) {
        self.auth.clear();
        self.sessions.clear();
    }

    pub(super) fn forget_thread(&mut self, thread_id: &str) {
        self.sessions.retain(|_, tid| tid != thread_id);
    }

    /// Builds the harness-agnostic MCP server configuration for `thread_id`,
    /// minting a per-session token bound to `thread_id` while keeping the
    /// harness-lifetime auth secret in the bridge's environment.
    pub(super) fn servers_for_thread(&mut self, thread_id: &str) -> Vec<McpServerSpec> {
        if self.auth.is_empty() {
            return Vec::new();
        }
        let session_token = self
            .sessions
            .iter()
            .find(|(_, tid)| tid.as_str() == thread_id)
            .map(|(tok, _)| tok.clone())
            .unwrap_or_else(|| {
                let tok = sys::random_hex(16);
                self.sessions.insert(tok.clone(), thread_id.to_string());
                tok
            });
        let exe = self.self_exe.clone();
        vec![McpServerSpec::crowbar(
            exe,
            &self.tool_socket,
            &self.auth,
            &session_token,
        )]
    }

    pub(super) fn authenticate(&self, auth: &str) -> bool {
        !self.auth.is_empty() && constant_time_eq(auth.as_bytes(), self.auth.as_bytes())
    }

    pub(super) fn thread_for_session_token(&self, token: &str) -> Option<&str> {
        self.sessions.get(token).map(String::as_str)
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

impl Service {
    pub(super) fn mcp_servers_for_thread(&mut self, thread_id: &str) -> Vec<McpServerSpec> {
        self.mcp.servers_for_thread(thread_id)
    }

    /// Handles `op: "list-dirs"` for the shell's inline project folder picker
    /// (since `crowbaros-shell` runs under its own account and cannot read a
    /// `0700` home directory on Base).
    pub(super) fn handle_list_dirs(&self, value: &Value) -> RequestOutcome {
        let Some(obj) = value.as_object() else {
            return error_outcome("invalid-request", "Expected a JSON object.");
        };
        let path = obj.get("path").and_then(Value::as_str);
        if obj.contains_key("path") && !obj["path"].is_null() && path.is_none() {
            return error_outcome("invalid-request", "path must be a string or null.");
        }
        let home_str = default_cwd();
        let home = PathBuf::from(&home_str);
        match project::list_subdirectories(path, &home) {
            Ok(listing) => {
                let Some(path_str) = listing.path.to_str() else {
                    return error_outcome("invalid-request", "Folder resolves to a path that is not valid UTF-8.");
                };
                let parent = match listing.parent.as_deref() {
                    Some(p) => p.to_str().map(str::to_string),
                    None => None,
                };
                let entries: Vec<Value> = listing
                    .entries
                    .into_iter()
                    .filter_map(|(name, p)| p.to_str().map(|ps| json!({ "name": name, "path": ps })))
                    .collect();
                RequestOutcome {
                    reply: Some(json!({
                        "reply": "dirs",
                        "path": path_str,
                        "home": home_str,
                        "parent": parent,
                        "entries": entries,
                    })),
                    subscribe: None,
                    broadcasts: Vec::new(),
                }
            }
            Err(err) => error_outcome(err.code(), err.message()),
        }
    }

    /// Handles `op: "make-project"` from the drawer or `crowbar-agent`.
    pub(super) fn handle_make_project(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let Some(obj) = value.as_object() else {
            return error_outcome("invalid-request", "Expected a JSON object.");
        };
        let Some(thread_id) = obj.get("thread").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return error_outcome("invalid-request", "make-project requires a thread ID.");
        };
        let name = obj.get("name").and_then(Value::as_str);
        let place = obj.get("place").and_then(Value::as_str);
        let path = obj.get("path").and_then(Value::as_str);
        if obj.contains_key("name") && name.is_none() {
            return error_outcome("invalid-request", "name must be a string.");
        }
        if obj.contains_key("place") && !obj["place"].is_null() && place.is_none() {
            return error_outcome("invalid-request", "place must be a string or null.");
        }
        if obj.contains_key("path") && !obj["path"].is_null() && path.is_none() {
            return error_outcome("invalid-request", "path must be a string or null.");
        }
        if path.is_some() && (place.is_some() || name.is_some()) {
            return error_outcome("invalid-request", "Provide either path or name/place, not both.");
        }
        let home = PathBuf::from(default_cwd());
        let target = match (path, name) {
            (Some(exact), None) => project::resolve_exact_path(exact, &home),
            (None, Some(name)) => project::resolve_project_path(name, place, &home),
            (None, None) | (Some(_), Some(_)) => Err(project::ProjectError::Invalid(
                "make-project requires a project name or folder path.".to_string(),
            )),
        };
        let target = match target {
            Ok(p) => p,
            Err(err) => return error_outcome(err.code(), err.message()),
        };

        self.save_endings();
        if self.journal.has_unsaved_for_thread(thread_id) {
            return error_outcome(
                "storage-error",
                "Lever is still saving how this thread's last turn ended.",
            );
        }
        let meta = match self.store.thread_meta(thread_id) {
            Ok(Some(meta)) => meta,
            Ok(None) => return error_outcome("no-thread", &format!("There is no thread {thread_id}.")),
            Err(err) => {
                store_failed(log, &err);
                return error_outcome("storage-error", &err);
            }
        };
        if meta.needs_history {
            return error_outcome(
                "working",
                "Open this thread first so Lever imports its history before making it a project.",
            );
        }
        if meta.working_turn.is_some()
            || self.thread_busy(thread_id)
            || self.turns.has_live_session_work(thread_id)
            || self.discovery.has_pending_send(thread_id)
        {
            return error_outcome(
                "working",
                "Wait for the current turn, subagents, and requests to finish before making this thread a project.",
            );
        }

        match self.apply_make_project(thread_id, &meta, &target, false, log) {
            Ok((cwd, created, broadcasts)) => RequestOutcome {
                reply: Some(json!({
                    "reply": "project-made",
                    "thread": thread_id,
                    "cwd": cwd,
                    "created": created,
                })),
                subscribe: None,
                broadcasts,
            },
            Err((code, msg)) => error_outcome(code, &msg),
        }
    }

    /// Handles `op: "mcp-call"` from `crowbar-lever mcp-bridge` over Lever's
    /// private tool Unix socket.
    pub(super) fn handle_mcp_call(&mut self, value: &Value, log: &mut Log) -> RequestOutcome {
        let mcp_error = |text: &str| RequestOutcome {
            reply: Some(json!({ "reply": "mcp-result", "ok": false, "text": text })),
            subscribe: None,
            broadcasts: Vec::new(),
        };
        let Some(obj) = value.as_object() else {
            return mcp_error("Invalid MCP tool request.");
        };
        let auth = obj.get("auth").and_then(Value::as_str).unwrap_or("");
        if !self.mcp.authenticate(auth) {
            return mcp_error("MCP bridge authentication failed.");
        }
        let session = obj.get("session").and_then(Value::as_str).unwrap_or("");
        let meta_session = obj
            .get("meta_session")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let Some(tool) = obj.get("tool").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
            return mcp_error("Missing tool name.");
        };
        let arguments = obj.get("arguments").unwrap_or(&Value::Null);

        let thread_id = if let Some(sid) = meta_session {
            let Some(harness) = self.harness.as_ref() else {
                return mcp_error("No active harness session for this tool call.");
            };
            if harness.is_subagent_session(sid) {
                return mcp_error("Only the main thread can be made into a project, not a subagent.");
            }
            match harness.thread_for_session(sid) {
                Some(tid) => tid.to_string(),
                None => return mcp_error("Could not identify the calling thread for this tool call."),
            }
        } else {
            match self.mcp.thread_for_session_token(session) {
                Some(tid) => tid.to_string(),
                None => return mcp_error("Could not identify the calling thread for this tool call."),
            }
        };

        let Some(active) = self.turns.working_turns.get(&thread_id) else {
            return mcp_error("Could not identify an active turn for this tool call.");
        };
        if active.stop_asked.is_some() {
            return mcp_error("This turn was stopped; cannot make the thread into a project.");
        }
        if !matches!(
            active.dispatch,
            TurnDispatch::PromptInFlight { .. } | TurnDispatch::HarnessStarted { .. }
        ) {
            return mcp_error("Could not identify an active turn for this tool call.");
        }

        if self.turns.deleting(&thread_id) {
            return mcp_error("The calling thread is being deleted.");
        }

        match tool {
            mcp::TOOL_MAKE_PROJECT => {
                let Some(args_obj) = arguments.as_object() else {
                    return mcp_error("make_project arguments must be a JSON object.");
                };
                let Some(name) = args_obj.get("name").and_then(Value::as_str) else {
                    return mcp_error("make_project requires a string 'name' argument.");
                };
                let place = match args_obj.get("place") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(s.as_str()),
                    Some(_) => return mcp_error("make_project 'place' must be a string."),
                };
                let home = PathBuf::from(default_cwd());
                let target = match project::resolve_project_path(name, place, &home) {
                    Ok(p) => p,
                    Err(err) => return mcp_error(err.message()),
                };
                let meta = match self.store.thread_meta(&thread_id) {
                    Ok(Some(meta)) => meta,
                    Ok(None) => return mcp_error("The calling thread no longer exists."),
                    Err(err) => {
                        store_failed(log, &err);
                        return mcp_error(&format!("Could not read thread record: {err}"));
                    }
                };
                if meta.needs_history {
                    return mcp_error("The calling thread's history has not been imported yet.");
                }
                match self.apply_make_project(&thread_id, &meta, &target, true, log) {
                    Ok((cwd, created, broadcasts)) => {
                        let status = if created {
                            format!("Created project folder {cwd} and tied this thread to it.")
                        } else {
                            format!("Tied this thread to existing folder {cwd} without changing its contents.")
                        };
                        let text = format!(
                            "{status} Subsequent turns will run in {cwd} automatically with the thread's conversation history preserved. If you run any command in this current turn, use {cwd} as the working directory."
                        );
                        RequestOutcome {
                            reply: Some(json!({
                                "reply": "mcp-result",
                                "ok": true,
                                "text": text,
                            })),
                            subscribe: None,
                            broadcasts,
                        }
                    }
                    Err((_, msg)) => mcp_error(&msg),
                }
            }
            other => mcp_error(&format!("Unknown tool: {other}")),
        }
    }

    /// Creates the target project directory (if missing) and updates the thread
    /// record and harness session lifecycle.
    fn apply_make_project(
        &mut self,
        thread_id: &str,
        meta: &ThreadMeta,
        target: &Path,
        from_active_turn: bool,
        log: &mut Log,
    ) -> Result<(String, bool, Vec<Broadcast>), (&'static str, String)> {
        let Some(target_str) = target.to_str().map(str::to_string) else {
            return Err((
                "invalid-request",
                "Project folder resolves to a path that is not valid UTF-8.".to_string(),
            ));
        };
        let created = project::ensure_project_dir(target).map_err(|e| (e.code(), e.message().to_string()))?;
        if target_str == meta.cwd {
            return Ok((target_str, created, Vec::new()));
        }

        let has_prior = meta.latest_turn.is_some();
        let old_session = meta.harness_session_id.clone().filter(|s| !s.is_empty()).or_else(|| {
            self.harness
                .as_ref()
                .and_then(|h| h.session_for_thread(thread_id))
                .map(str::to_string)
        });

        self.store
            .move_thread_to_project(thread_id, &target_str, has_prior, old_session.as_deref(), unix_now())
            .map_err(|err| {
                store_failed(log, &err);
                ("storage-error", err)
            })?;

        // Any active turn on the old session must no longer clear `needs_handoff`
        // when it emits output or finishes: the flag in SQLite now belongs to
        // this newer move, not the old session's handoff.
        if let Some(active) = self.turns.working_turns.get_mut(thread_id) {
            active.handoff_pending = false;
        }

        let retired_turn = self
            .turns
            .working_turns
            .get(thread_id)
            .map(|active| active.turn)
            .or(meta.latest_turn);
        if from_active_turn && self.turns.has_live_session_work(thread_id) {
            self.turns
                .pending_session_resets
                .insert(thread_id.to_string(), retired_turn);
        } else {
            self.turns.pending_session_resets.remove(thread_id);
            if let Some(harness) = self.harness.as_mut() {
                harness.detach_session(thread_id, retired_turn);
            }
            self.mcp.forget_thread(thread_id);
        }

        let broadcasts = vec![Broadcast {
            thread_id: thread_id.to_string(),
            message: json!({
                "event": "thread-updated",
                "thread": thread_id,
                "cwd": target_str,
            }),
        }];
        Ok((target_str, created, broadcasts))
    }

    /// Detaches a moved thread's pre-move harness session once no live work
    /// remains on it. Returns `true` when no pre-move session remains attached
    /// (either none was pending or it was just detached).
    pub(super) fn flush_pending_session_reset(&mut self, thread_id: &str) -> bool {
        if !self.turns.pending_session_resets.contains_key(thread_id) {
            return true;
        }
        if self.turns.has_live_session_work(thread_id) {
            return false;
        }
        let retired_turn = self.turns.pending_session_resets.remove(thread_id).flatten();
        if let Some(harness) = self.harness.as_mut() {
            harness.detach_session(thread_id, retired_turn);
        }
        self.mcp.forget_thread(thread_id);
        true
    }

    /// Clears `needs_handoff` in SQLite once the harness acknowledges or
    /// answers a turn that carried `<thread-history>`.
    pub(super) fn mark_handoff_delivered(&mut self, thread_id: &str, log: &mut Log) {
        if let Some(active) = self.turns.working_turns.get_mut(thread_id)
            && active.handoff_pending
            && matches!(active.dispatch, TurnDispatch::PromptInFlight { .. })
        {
            active.handoff_pending = false;
            if let Err(err) = self.store.clear_needs_handoff(thread_id) {
                store_failed(log, &err);
            }
        }
    }
}
