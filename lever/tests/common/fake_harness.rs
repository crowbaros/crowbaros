//! In-memory `FakeHarness`, `FakeHarnessFactory`, and `FakeSessionDiscoverer`
//! implementing Lever's `Harness`, `HarnessFactory`, and `SessionDiscoverer`
//! traits from outside the `crowbar_lever` crate.

use crowbar_lever::discovery::{Listing, SessionDiscoverer};
use crowbar_lever::harness::{
    EffortSelector, ExternalSession, Harness, HarnessCapabilities, HarnessConfig, HarnessEvent, HarnessFactory,
    HarnessLaunchOptions,
};
use crowbar_lever::mcp::McpServerSpec;
use crowbar_lever::watcher::WatchSpec;
use std::collections::{HashMap, HashSet, VecDeque};
use std::os::fd::RawFd;
use std::sync::{Arc, Mutex};

/// Every outbound call `Service` makes into `FakeHarness`, recorded for
/// deterministic assertions in in-process tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedCall {
    Spawn {
        free_hosted_models: bool,
    },
    EnsureSession {
        thread_id: String,
        turn: u64,
        cwd: String,
        existing_session_id: Option<String>,
    },
    SendPrompt {
        thread_id: String,
        turn: u64,
        session_id: String,
        context: Option<String>,
        text: String,
    },
    Steer {
        thread_id: String,
        steer: u64,
        session_id: String,
        context: Option<String>,
        text: String,
    },
    CancelSession {
        session_id: String,
    },
    AnswerPermission {
        handle: u64,
        answer: Option<String>,
    },
    ProbeModels {
        cwd: String,
    },
    ProbeEfforts {
        cwd: String,
        model: String,
    },
    SetSessionModel {
        thread_id: String,
        turn: Option<u64>,
        session_id: String,
        model: String,
    },
    SetSessionMode {
        thread_id: String,
        turn: Option<u64>,
        session_id: String,
        mode: String,
    },
    SetSessionEffort {
        thread_id: String,
        turn: u64,
        session_id: String,
        effort: String,
    },
    LoadSessionHistory {
        thread_id: String,
        session_id: String,
        cwd: String,
    },
    DeleteSession {
        thread_id: String,
        session_id: String,
    },
    DetachSession {
        thread_id: String,
        retired_turn: Option<u64>,
    },
    ForgetSession {
        thread_id: String,
    },
    ListSessions,
    AbandonListSessions,
    Stopped,
    KilledTree,
}

#[derive(Default)]
struct Inner {
    capabilities: HarnessCapabilities,
    spawn_error: Option<String>,
    decline_steer: bool,
    initialized: bool,
    probing_models: bool,
    listing_sessions: bool,
    fault: Option<&'static str>,
    events: VecDeque<HarnessEvent>,
    calls: Vec<RecordedCall>,
    thread_to_session: HashMap<String, String>,
    session_to_thread: HashMap<String, String>,
    subagent_sessions: HashSet<String>,
    launchers: HashMap<String, String>,
    retired_turns: HashMap<String, u64>,
    session_models: HashMap<String, String>,
    session_modes: HashMap<String, String>,
    session_efforts: HashMap<String, EffortSelector>,
    /// Pending outbound `set_session_mode` / `set_session_effort` requests,
    /// committed only when the matching `ModeSet` / `EffortSet` event is
    /// delivered (matching `AcpHarness`'s async RPC completion semantics).
    pending_mode_sets: HashMap<String, (String, String)>,
    pending_effort_sets: HashMap<String, (String, String)>,
    active_permissions: HashSet<u64>,
    probe_sessions: Vec<String>,
    spawns: usize,
}

impl Inner {
    fn apply_event(&mut self, event: &HarnessEvent) {
        match event {
            HarnessEvent::Initialized => self.initialized = true,
            HarnessEvent::SessionReady {
                thread_id,
                session_id,
                current_model,
                ..
            } => {
                if let Some(old) = self.thread_to_session.insert(thread_id.clone(), session_id.clone())
                    && old != *session_id
                {
                    self.session_to_thread.remove(&old);
                    self.session_models.remove(&old);
                    self.session_modes.remove(&old);
                    self.session_efforts.remove(&old);
                }
                self.session_to_thread.insert(session_id.clone(), thread_id.clone());
                if let Some(m) = current_model {
                    self.session_models.insert(session_id.clone(), m.clone());
                }
            }
            HarnessEvent::ModelSet { thread_id, model, .. } => {
                if let Some(sid) = self.thread_to_session.get(thread_id).cloned() {
                    self.session_models.insert(sid, model.clone());
                }
            }
            HarnessEvent::ModeSet { thread_id, .. } => {
                if let Some((sid, mode)) = self.pending_mode_sets.remove(thread_id) {
                    self.session_modes.insert(sid, mode);
                }
            }
            HarnessEvent::ModeSetFailed { thread_id, .. } => {
                self.pending_mode_sets.remove(thread_id);
            }
            HarnessEvent::EffortSet { thread_id, .. } => {
                if let Some((sid, effort)) = self.pending_effort_sets.remove(thread_id)
                    && let Some(sel) = self.session_efforts.get_mut(&sid)
                {
                    sel.current = effort;
                }
            }
            HarnessEvent::EffortSetFailed { thread_id, .. } => {
                self.pending_effort_sets.remove(thread_id);
            }
            HarnessEvent::CatalogUpdated {
                thread_id,
                current_model,
                current_mode,
                ..
            } => {
                if let Some(sid) = self.thread_to_session.get(thread_id).cloned() {
                    if let Some(m) = current_model {
                        self.session_models.insert(sid.clone(), m.clone());
                    }
                    if let Some(mode) = current_mode {
                        self.session_modes.insert(sid, mode.clone());
                    }
                }
            }
            HarnessEvent::HistoryLoaded { thread_id, history, .. } => {
                if let Some(sid) = self.thread_to_session.get(thread_id).cloned()
                    && let Some(ref m) = history.model
                {
                    self.session_models.insert(sid, m.clone());
                }
            }
            HarnessEvent::ModelsProbed(_) => self.probing_models = false,
            HarnessEvent::SessionsListed(_) => self.listing_sessions = false,
            HarnessEvent::PermissionRequested { handle, .. } => {
                self.active_permissions.insert(*handle);
            }
            HarnessEvent::PermissionWithdrawn { handle } => {
                self.active_permissions.remove(handle);
            }
            _ => {}
        }
    }
}

/// Shared handle held by both the test and the [`FakeHarness`] instances
/// spawned by [`FakeHarnessFactory`].
#[derive(Clone)]
pub struct FakeHarnessHandle {
    inner: Arc<Mutex<Inner>>,
}

impl FakeHarnessHandle {
    pub fn new(capabilities: HarnessCapabilities) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                capabilities,
                ..Inner::default()
            })),
        }
    }

    pub fn factory(&self) -> FakeHarnessFactory {
        FakeHarnessFactory { handle: self.clone() }
    }

    pub fn set_capabilities(&self, capabilities: HarnessCapabilities) {
        self.inner.lock().unwrap().capabilities = capabilities;
    }

    pub fn set_spawn_error(&self, err: Option<String>) {
        self.inner.lock().unwrap().spawn_error = err;
    }

    pub fn set_decline_steer(&self, decline: bool) {
        self.inner.lock().unwrap().decline_steer = decline;
    }

    pub fn set_session_model(&self, session_id: &str, model: &str) {
        self.inner
            .lock()
            .unwrap()
            .session_models
            .insert(session_id.to_string(), model.to_string());
    }

    pub fn set_session_mode(&self, session_id: &str, mode: &str) {
        self.inner
            .lock()
            .unwrap()
            .session_modes
            .insert(session_id.to_string(), mode.to_string());
    }

    pub fn set_session_effort(&self, session_id: &str, selector: EffortSelector) {
        self.inner
            .lock()
            .unwrap()
            .session_efforts
            .insert(session_id.to_string(), selector);
    }

    pub fn session_current_mode(&self, session_id: &str) -> Option<String> {
        self.inner.lock().unwrap().session_modes.get(session_id).cloned()
    }

    pub fn session_current_model(&self, session_id: &str) -> Option<String> {
        self.inner.lock().unwrap().session_models.get(session_id).cloned()
    }

    pub fn register_subagent(&self, child_session_id: &str, launcher: Option<&str>) {
        let mut g = self.inner.lock().unwrap();
        g.subagent_sessions.insert(child_session_id.to_string());
        if let Some(l) = launcher {
            g.launchers.insert(child_session_id.to_string(), l.to_string());
        }
    }

    pub fn push_event(&self, event: HarnessEvent) {
        self.inner.lock().unwrap().events.push_back(event);
    }

    pub fn push_events(&self, events: impl IntoIterator<Item = HarnessEvent>) {
        let mut g = self.inner.lock().unwrap();
        g.events.extend(events);
    }

    pub fn take_calls(&self) -> Vec<RecordedCall> {
        std::mem::take(&mut self.inner.lock().unwrap().calls)
    }

    pub fn calls(&self) -> Vec<RecordedCall> {
        self.inner.lock().unwrap().calls.clone()
    }

    pub fn spawns(&self) -> usize {
        self.inner.lock().unwrap().spawns
    }
}

pub struct FakeHarnessFactory {
    handle: FakeHarnessHandle,
}

impl HarnessFactory for FakeHarnessFactory {
    fn spawn(&mut self, _config: &HarnessConfig, options: HarnessLaunchOptions) -> Result<Box<dyn Harness>, String> {
        let mut g = self.handle.inner.lock().unwrap();
        if let Some(err) = g.spawn_error.clone() {
            return Err(err);
        }
        g.spawns += 1;
        g.initialized = false;
        g.probing_models = false;
        g.listing_sessions = false;
        g.fault = None;
        g.events.clear();
        g.thread_to_session.clear();
        g.session_to_thread.clear();
        g.subagent_sessions.clear();
        g.launchers.clear();
        g.retired_turns.clear();
        g.session_models.clear();
        g.session_modes.clear();
        g.session_efforts.clear();
        g.pending_mode_sets.clear();
        g.pending_effort_sets.clear();
        g.active_permissions.clear();
        g.probe_sessions.clear();
        g.calls.push(RecordedCall::Spawn {
            free_hosted_models: options.free_hosted_models,
        });
        Ok(Box::new(FakeHarness {
            handle: self.handle.clone(),
            thread_to_session: HashMap::new(),
            session_to_thread: HashMap::new(),
            launchers: HashMap::new(),
            session_models: HashMap::new(),
            session_modes: HashMap::new(),
            session_efforts: HashMap::new(),
        }))
    }
}

pub struct FakeHarness {
    handle: FakeHarnessHandle,
    // Mirrors synced on `on_readable()` and mutations so `&self` getters
    // return real borrowed slices with zero `unsafe`.
    thread_to_session: HashMap<String, String>,
    session_to_thread: HashMap<String, String>,
    launchers: HashMap<String, String>,
    session_models: HashMap<String, String>,
    session_modes: HashMap<String, String>,
    session_efforts: HashMap<String, EffortSelector>,
}

impl FakeHarness {
    fn sync(&mut self) {
        let g = self.handle.inner.lock().unwrap();
        self.thread_to_session = g.thread_to_session.clone();
        self.session_to_thread = g.session_to_thread.clone();
        self.launchers = g.launchers.clone();
        self.session_models = g.session_models.clone();
        self.session_modes = g.session_modes.clone();
        self.session_efforts = g.session_efforts.clone();
    }
}

impl Harness for FakeHarness {
    fn pid(&self) -> Option<u32> {
        None
    }

    fn stdout_fd(&self) -> Option<RawFd> {
        None
    }

    fn stdin_fd(&self) -> Option<RawFd> {
        None
    }

    fn wants_write(&self) -> bool {
        false
    }

    fn on_writable(&mut self) -> bool {
        true
    }

    fn on_readable(&mut self) -> Vec<HarnessEvent> {
        let events: Vec<HarnessEvent> = {
            let mut g = self.handle.inner.lock().unwrap();
            let drained: Vec<HarnessEvent> = g.events.drain(..).collect();
            for ev in &drained {
                g.apply_event(ev);
            }
            drained
        };
        self.sync();
        events
    }

    fn fault(&self) -> Option<&'static str> {
        self.handle.inner.lock().unwrap().fault
    }

    fn kill_tree(self: Box<Self>) {
        self.handle.inner.lock().unwrap().calls.push(RecordedCall::KilledTree);
    }

    fn stop(self: Box<Self>) {
        self.handle.inner.lock().unwrap().calls.push(RecordedCall::Stopped);
    }

    fn is_initialized(&self) -> bool {
        self.handle.inner.lock().unwrap().initialized
    }

    fn capabilities(&self) -> HarnessCapabilities {
        let g = self.handle.inner.lock().unwrap();
        let caps = g.capabilities;
        HarnessCapabilities {
            supports_modes: caps.supports_modes,
            supports_steering: g.initialized && caps.supports_steering,
            supports_list_sessions: g.initialized && caps.supports_list_sessions,
            supports_load_session: g.initialized && caps.supports_load_session,
            supports_delete_session: g.initialized && caps.supports_delete_session,
            holds_prompt_during_subagent_request: caps.holds_prompt_during_subagent_request,
        }
    }

    fn session_for_thread(&self, thread_id: &str) -> Option<&str> {
        self.thread_to_session.get(thread_id).map(String::as_str)
    }

    fn thread_for_session(&self, session_id: &str) -> Option<&str> {
        self.session_to_thread.get(session_id).map(String::as_str)
    }

    fn is_subagent_session(&self, session_id: &str) -> bool {
        self.handle.inner.lock().unwrap().subagent_sessions.contains(session_id)
    }

    fn own_session_ids(&self) -> Vec<String> {
        let g = self.handle.inner.lock().unwrap();
        g.session_to_thread
            .keys()
            .chain(g.probe_sessions.iter())
            .cloned()
            .collect()
    }

    fn launcher_of(&self, child_session_id: &str) -> Option<&str> {
        self.launchers.get(child_session_id).map(String::as_str)
    }

    fn retired_turn_of(&self, child_session_id: &str) -> Option<u64> {
        self.handle
            .inner
            .lock()
            .unwrap()
            .retired_turns
            .get(child_session_id)
            .copied()
    }

    fn session_current_model(&self, session_id: &str) -> Option<&str> {
        self.session_models.get(session_id).map(String::as_str)
    }

    fn session_current_mode(&self, session_id: &str) -> Option<&str> {
        self.session_modes.get(session_id).map(String::as_str)
    }

    fn session_effort(&self, session_id: &str) -> Option<&EffortSelector> {
        self.session_efforts.get(session_id)
    }

    fn ensure_session(
        &mut self,
        thread_id: &str,
        turn: u64,
        cwd: &str,
        existing_session_id: Option<&str>,
        _mcp_servers: &[McpServerSpec],
    ) {
        self.handle
            .inner
            .lock()
            .unwrap()
            .calls
            .push(RecordedCall::EnsureSession {
                thread_id: thread_id.to_string(),
                turn,
                cwd: cwd.to_string(),
                existing_session_id: existing_session_id.map(str::to_string),
            });
    }

    fn load_session_history(
        &mut self,
        thread_id: &str,
        session_id: &str,
        cwd: &str,
        _mcp_servers: &[McpServerSpec],
    ) -> bool {
        let mut g = self.handle.inner.lock().unwrap();
        if !g.initialized || !g.capabilities.supports_load_session {
            return false;
        }
        g.calls.push(RecordedCall::LoadSessionHistory {
            thread_id: thread_id.to_string(),
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
        });
        true
    }

    fn delete_session(&mut self, thread_id: &str, session_id: &str) -> bool {
        let mut g = self.handle.inner.lock().unwrap();
        if !g.initialized || !g.capabilities.supports_delete_session {
            return false;
        }
        g.calls.push(RecordedCall::DeleteSession {
            thread_id: thread_id.to_string(),
            session_id: session_id.to_string(),
        });
        true
    }

    fn detach_session(&mut self, thread_id: &str, retired_turn: Option<u64>) {
        {
            let mut g = self.handle.inner.lock().unwrap();
            if let Some(sid) = g.thread_to_session.remove(thread_id) {
                g.session_to_thread.remove(&sid);
                g.session_models.remove(&sid);
                g.session_modes.remove(&sid);
                g.session_efforts.remove(&sid);
            }
            g.pending_mode_sets.remove(thread_id);
            g.pending_effort_sets.remove(thread_id);
            g.calls.push(RecordedCall::DetachSession {
                thread_id: thread_id.to_string(),
                retired_turn,
            });
        }
        self.sync();
    }

    fn forget_session(&mut self, thread_id: &str) {
        {
            let mut g = self.handle.inner.lock().unwrap();
            if let Some(sid) = g.thread_to_session.remove(thread_id) {
                g.session_to_thread.remove(&sid);
                g.session_models.remove(&sid);
                g.session_modes.remove(&sid);
                g.session_efforts.remove(&sid);
            }
            g.pending_mode_sets.remove(thread_id);
            g.pending_effort_sets.remove(thread_id);
            g.calls.push(RecordedCall::ForgetSession {
                thread_id: thread_id.to_string(),
            });
        }
        self.sync();
    }

    fn send_prompt(&mut self, thread_id: &str, turn: u64, session_id: &str, context: Option<&str>, text: &str) {
        self.handle.inner.lock().unwrap().calls.push(RecordedCall::SendPrompt {
            thread_id: thread_id.to_string(),
            turn,
            session_id: session_id.to_string(),
            context: context.map(str::to_string),
            text: text.to_string(),
        });
    }

    fn steer(&mut self, thread_id: &str, steer: u64, session_id: &str, context: Option<&str>, text: &str) -> bool {
        let mut g = self.handle.inner.lock().unwrap();
        if !g.initialized || !g.capabilities.supports_steering || g.decline_steer {
            return false;
        }
        g.calls.push(RecordedCall::Steer {
            thread_id: thread_id.to_string(),
            steer,
            session_id: session_id.to_string(),
            context: context.map(str::to_string),
            text: text.to_string(),
        });
        true
    }

    fn cancel_session(&mut self, session_id: &str) {
        self.handle
            .inner
            .lock()
            .unwrap()
            .calls
            .push(RecordedCall::CancelSession {
                session_id: session_id.to_string(),
            });
    }

    fn answer_permission(&mut self, handle: u64, answer: Option<&str>) -> bool {
        let mut g = self.handle.inner.lock().unwrap();
        let existed = g.active_permissions.remove(&handle);
        if existed {
            g.calls.push(RecordedCall::AnswerPermission {
                handle,
                answer: answer.map(str::to_string),
            });
        }
        existed
    }

    fn probing_models(&self) -> bool {
        self.handle.inner.lock().unwrap().probing_models
    }

    fn probe_models(&mut self, cwd: &str) {
        let mut g = self.handle.inner.lock().unwrap();
        g.probing_models = true;
        g.calls.push(RecordedCall::ProbeModels { cwd: cwd.to_string() });
    }

    fn probe_efforts(&mut self, cwd: &str, model: &str) {
        self.handle
            .inner
            .lock()
            .unwrap()
            .calls
            .push(RecordedCall::ProbeEfforts {
                cwd: cwd.to_string(),
                model: model.to_string(),
            });
    }

    fn take_new_probe_sessions(&mut self) -> Vec<String> {
        std::mem::take(&mut self.handle.inner.lock().unwrap().probe_sessions)
    }

    fn set_session_model(&mut self, thread_id: &str, turn: Option<u64>, session_id: &str, model: &str) {
        self.handle
            .inner
            .lock()
            .unwrap()
            .calls
            .push(RecordedCall::SetSessionModel {
                thread_id: thread_id.to_string(),
                turn,
                session_id: session_id.to_string(),
                model: model.to_string(),
            });
    }

    fn set_session_mode(&mut self, thread_id: &str, turn: Option<u64>, session_id: &str, mode: &str) {
        let mut g = self.handle.inner.lock().unwrap();
        g.pending_mode_sets
            .insert(thread_id.to_string(), (session_id.to_string(), mode.to_string()));
        g.calls.push(RecordedCall::SetSessionMode {
            thread_id: thread_id.to_string(),
            turn,
            session_id: session_id.to_string(),
            mode: mode.to_string(),
        });
    }

    fn set_session_effort(&mut self, thread_id: &str, turn: u64, session_id: &str, effort: &str) {
        let mut g = self.handle.inner.lock().unwrap();
        g.pending_effort_sets
            .insert(thread_id.to_string(), (session_id.to_string(), effort.to_string()));
        g.calls.push(RecordedCall::SetSessionEffort {
            thread_id: thread_id.to_string(),
            turn,
            session_id: session_id.to_string(),
            effort: effort.to_string(),
        });
    }

    fn is_listing_sessions(&self) -> bool {
        self.handle.inner.lock().unwrap().listing_sessions
    }

    fn abandon_list_sessions(&mut self) {
        let mut g = self.handle.inner.lock().unwrap();
        g.listing_sessions = false;
        g.calls.push(RecordedCall::AbandonListSessions);
    }

    fn list_sessions(&mut self) -> bool {
        let mut g = self.handle.inner.lock().unwrap();
        if !g.initialized || !g.capabilities.supports_list_sessions {
            return false;
        }
        g.listing_sessions = true;
        g.calls.push(RecordedCall::ListSessions);
        true
    }

    fn remember_tool_ids(&mut self, _thread_id: &str, _ids: Vec<String>) {}

    fn remember_subagent_ids(&mut self, _thread_id: &str, _ids: Vec<String>) {}
}

/// Simple in-memory [`SessionDiscoverer`] for in-process discovery tests.
pub struct FakeSessionDiscoverer {
    sessions: Arc<Mutex<Vec<ExternalSession>>>,
}

impl FakeSessionDiscoverer {
    pub fn new(sessions: Vec<ExternalSession>) -> (Self, Arc<Mutex<Vec<ExternalSession>>>) {
        let shared = Arc::new(Mutex::new(sessions));
        (
            Self {
                sessions: Arc::clone(&shared),
            },
            shared,
        )
    }
}

impl SessionDiscoverer for FakeSessionDiscoverer {
    fn watch_specs(&self) -> Vec<WatchSpec> {
        Vec::new()
    }

    fn has_store_data(&self) -> bool {
        !self.sessions.lock().unwrap().is_empty()
    }

    fn list_sessions(&self) -> Result<Listing, String> {
        Ok(Listing {
            sessions: self.sessions.lock().unwrap().clone(),
            watcher: None,
        })
    }
}
