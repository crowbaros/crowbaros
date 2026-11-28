// SPDX-License-Identifier: MIT
//! Core Agent state machine updated by Lever surface events.

use serde::Serialize;

use super::types::{
    AgentEvent, CommandInfo, EffortInfo, Mark, ModeInfo, ModelInfo, RequestState, RequestView,
    ResumedThread, StatusInfo, Subagent, SubagentMessage, SubagentState, ThreadSummary, Turn,
    TurnState, calm_turn_error, request_view, unix_now,
};

/// The shell's held state for the Agent drawer and mark.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AgentState {
    pub list_view: bool,
    pub loading: bool,
    pub threads: Vec<ThreadSummary>,
    #[serde(skip)]
    list_received: bool,

    pub configured: bool,
    pub connected: bool,
    pub lever_version: Option<String>,
    pub harness_started: bool,
    pub harnesses: Vec<String>,
    pub free_hosted_models: bool,
    /// Whether the person has chosen a Model source (free hosted Models or a
    /// signed-in/configured provider).
    pub models_chosen: bool,
    pub no_harness: bool,
    pub sending: bool,
    pub model: Option<String>,
    pub missing_model: Option<String>,
    pub thread: Option<String>,
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_turn: Option<u64>,
    pub turns: Vec<Turn>,
    pub error: Option<String>,
    /// The mark across every thread: `waiting` while any request waits,
    /// else `working` while any thread works, else `done` until the drawer
    /// is summoned or put away after a turn ended, else `idle`.
    pub mark: Mark,
    /// The thread's mode, as Lever has it.
    pub mode: Option<String>,
    /// The mode picked for the drawer's new thread, sent with its first message.
    pub pending_mode: Option<String>,
    /// The mode new threads start in, from Lever's `status`.
    pub default_mode: Option<String>,
    /// The modes Lever offers, fetched when the person asks for a list of
    /// them or of the Models.
    pub modes: Vec<ModeInfo>,
    pub modes_loading: bool,
    pub modes_error: Option<String>,
    /// The thread's effort, as Lever has it.
    pub effort: Option<String>,
    /// The Model and effort picked for the drawer's new thread, sent with
    /// its first message.
    pub pending_model: Option<String>,
    pub pending_effort: Option<String>,
    /// The Model the person last picked, and the effort picked for it since,
    /// which a new thread starts with.
    pub last_model: Option<String>,
    pub last_effort: Option<String>,
    /// The Models Lever offers, and the one a thread works with when none
    /// was picked for it, fetched with the modes.
    pub models: Vec<ModelInfo>,
    pub default_model: Option<String>,
    /// Lever has said what the harness offers on this connection.
    pub offer_known: bool,
    /// Lever explicitly reported `supported: false` for Model selection, so an
    /// empty `models` list does not mean "no Model available".
    #[serde(skip)]
    pub offer_unsupported: bool,
    /// The harness can take a message into a turn under way, as Lever said
    /// in its last `models`.
    pub steering: bool,
    /// True once the person clicked Sign in from the drawer and Models have not
    /// refreshed (or failed to launch) yet.
    #[serde(skip)]
    pub sign_in_launched: bool,
    /// Bumped whenever the drawer should retry catalog/effort/command lookups
    /// (on disconnect, reconnect, error, or free-models toggle).
    #[serde(skip)]
    pub catalog_epoch: u64,
    /// The effort levels of each Model Lever has said them for.
    pub efforts: std::collections::BTreeMap<String, Vec<EffortInfo>>,
    /// The harness's commands, and those of the drawer's thread once Lever
    /// has said them.
    pub commands: Vec<CommandInfo>,
    pub thread_commands: Option<Vec<CommandInfo>>,
    /// The threads working and waiting, from Lever's `activity`.
    pub working_threads: Vec<String>,
    pub waiting_threads: Vec<String>,
    /// Lever has said which threads work and wait on this connection.
    #[serde(skip)]
    pub(super) watching: bool,
    /// The mark from the drawer's own thread alone: what the mark is before
    /// Lever's `activity` arrives, and where `done` is held after it does.
    #[serde(skip)]
    pub(super) thread_mark: Mark,
    #[serde(skip)]
    working_before_disconnect: Option<String>,
}

/// What picking a Model or an effort in the drawer does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingPick {
    /// Ask Lever to set it on the thread.
    SetThread { thread: String, value: String },
    /// Held for the drawer's new thread, and sent with its first message.
    Pending,
    /// The thread works, or the drawer cannot reach Lever; nothing changes.
    Refused,
}

/// What picking a mode in the drawer does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModePick {
    /// Ask Lever to set the thread's mode.
    SetThread { thread: String, mode: String },
    /// Held for the drawer's new thread, and sent with its first message.
    Pending,
    /// The thread works, or the drawer cannot reach Lever; nothing changes.
    Refused,
}

impl AgentState {
    pub fn new(configured: bool) -> Self {
        Self {
            configured,
            list_view: true,
            ..Default::default()
        }
    }

    /// Whether any subagent of the drawer's thread is still working.
    pub fn subagent_working(&self) -> bool {
        self.turns
            .iter()
            .any(|t| t.subagents.iter().any(|s| s.state.is_working()))
    }

    fn subagent_mut(&mut self, thread: &str, turn: u64, subagent: &str) -> Option<&mut Subagent> {
        if self.thread.as_deref() != Some(thread) {
            return None;
        }
        self.turns
            .iter_mut()
            .find(|t| t.turn == turn)?
            .subagents
            .iter_mut()
            .find(|s| s.id == subagent)
    }

    fn mark_after_turn_ends(&self, drawer_up: bool, finished_done: bool) -> Mark {
        if self.subagent_working() {
            Mark::Working
        } else if finished_done || !drawer_up {
            Mark::Done
        } else {
            Mark::Idle
        }
    }

    pub fn working(&self) -> bool {
        self.thread_mark == Mark::Working
            || self
                .turns
                .last()
                .is_some_and(|t| matches!(t.state, TurnState::Working))
            || self.subagent_working()
    }

    /// Stop is offered while the thread works, or while a request of its
    /// waits: Stop withdraws it.
    pub fn can_stop(&self) -> bool {
        self.connected && (self.working() || self.waiting())
    }

    /// The drawer must ask where Models come from before sending anything:
    /// Lever is connected with a harness, no Model source has been chosen yet,
    /// and there are no existing threads to read.
    pub fn needs_model_choice(&self) -> bool {
        self.connected
            && !self.no_harness
            && !self.models_chosen
            && self.turns.is_empty()
            && self.threads.is_empty()
    }

    /// Whether the message box sends now. While a turn works, Lever takes
    /// the message into it where the harness can, and otherwise has it wait.
    pub fn can_send(&self) -> bool {
        self.connected
            && !self.sending
            && !self.loading
            && !self.no_harness
            && !self.needs_model_choice()
    }

    /// Lever has probed Models, supports Model selection, and offers none.
    pub fn no_models_available(&self) -> bool {
        self.connected
            && !self.no_harness
            && self.offer_known
            && !self.offer_unsupported
            && self.models.is_empty()
    }

    pub fn current_turn(&self) -> Option<u64> {
        self.active_turn.or_else(|| {
            self.turns
                .last()
                .filter(|t| matches!(t.state, TurnState::Working))
                .map(|t| t.turn)
        })
    }

    /// A request of the drawer's thread waits for the person.
    pub fn waiting(&self) -> bool {
        self.turns.iter().any(|t| {
            t.requests.iter().any(|r| {
                matches!(
                    request_view(&t.state, r),
                    RequestView::Waiting | RequestView::Sending(_)
                )
            })
        })
    }

    /// The mode the drawer shows: the thread's, or for the drawer's new
    /// thread, the one picked for it or the starting mode.
    pub fn shown_mode(&self) -> Option<String> {
        if self.thread.is_some() {
            self.mode.clone()
        } else {
            self.pending_mode
                .clone()
                .or_else(|| self.default_mode.clone())
        }
    }

    /// The Model the drawer shows.
    pub fn shown_model(&self) -> Option<String> {
        if self.thread.is_some() {
            return self.model.clone().or_else(|| self.default_model.clone());
        }
        self.new_thread_model()
            .or_else(|| self.default_model.clone())
    }

    /// The Model a new thread's first message asks for, if any.
    pub fn new_thread_model(&self) -> Option<String> {
        self.pending_model.clone().or_else(|| {
            self.last_model.clone().filter(|model| {
                self.models.is_empty() || self.models.iter().any(|m| &m.id == model)
            })
        })
    }

    /// The effort the drawer shows.
    pub fn shown_effort(&self) -> Option<String> {
        if self.thread.is_some() {
            return self.effort.clone();
        }
        self.new_thread_effort()
    }

    /// The effort a new thread's first message asks for, if any.
    pub fn new_thread_effort(&self) -> Option<String> {
        self.pending_effort.clone().or_else(|| {
            let model = self.new_thread_model()?;
            (self.last_model.as_deref() == Some(model.as_str()))
                .then(|| self.last_effort.clone())
                .flatten()
        })
    }

    /// The name of a Model: the one Lever gave, or what its ID has after its
    /// provider.
    pub fn model_label(&self, id: &str) -> String {
        if let Some(model) = self.models.iter().find(|m| m.id == id) {
            return model.label().to_string();
        }
        id.split_once('/')
            .map(|(_, rest)| rest)
            .unwrap_or(id)
            .to_string()
    }

    /// The name of an effort level of a Model.
    pub fn effort_label(&self, model: Option<&str>, id: &str) -> String {
        if let Some(level) = model
            .and_then(|m| self.efforts.get(m))
            .and_then(|levels| levels.iter().find(|l| l.id == id))
        {
            return level.name.clone();
        }
        let mut chars = id.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    }

    /// The harness's commands for the drawer's thread.
    pub fn harness_commands(&self) -> &[CommandInfo] {
        self.thread_commands.as_deref().unwrap_or(&self.commands)
    }

    /// The person picked a Model for the thread.
    pub fn pick_model(&mut self, model: &str) -> SettingPick {
        if self.mode_locked() {
            return SettingPick::Refused;
        }
        if self.last_model.as_deref() != Some(model) {
            self.last_effort = None;
        }
        self.last_model = Some(model.to_string());
        match &self.thread {
            Some(thread) => SettingPick::SetThread {
                thread: thread.clone(),
                value: model.to_string(),
            },
            None => {
                if self.pending_model.as_deref() != Some(model) {
                    self.pending_effort = None;
                }
                self.pending_model = Some(model.to_string());
                SettingPick::Pending
            }
        }
    }

    /// The person picked an effort for the thread, one of its Model's.
    pub fn pick_effort(&mut self, effort: &str) -> SettingPick {
        if self.mode_locked() {
            return SettingPick::Refused;
        }
        if let Some(model) = self.shown_model() {
            if self.last_model.as_deref() != Some(model.as_str()) {
                self.last_model = Some(model);
            }
            self.last_effort = Some(effort.to_string());
        }
        match &self.thread {
            Some(thread) => SettingPick::SetThread {
                thread: thread.clone(),
                value: effort.to_string(),
            },
            None => {
                self.pending_effort = Some(effort.to_string());
                SettingPick::Pending
            }
        }
    }

    /// The thread's mode, Model, and effort cannot change now.
    pub fn mode_locked(&self) -> bool {
        let turn_works = match self.turns.last() {
            Some(turn) => matches!(turn.state, TurnState::Working),
            None => self.thread_mark == Mark::Working,
        };
        !self.connected || self.no_harness || self.sending || self.loading || turn_works
    }

    /// A list needs what the harness offers: ask Lever for its Models and modes.
    pub fn ask_offer(&mut self) -> bool {
        if !self.connected || self.no_harness {
            return false;
        }
        self.modes_loading = true;
        self.modes_error = None;
        true
    }

    /// The person picked a mode for the thread.
    pub fn pick_mode(&mut self, mode: &str) -> ModePick {
        if self.mode_locked() {
            return ModePick::Refused;
        }
        match &self.thread {
            Some(thread) => ModePick::SetThread {
                thread: thread.clone(),
                mode: mode.to_string(),
            },
            None => {
                self.pending_mode = Some(mode.to_string());
                ModePick::Pending
            }
        }
    }

    /// The person answered a request.
    pub fn begin_answer(&mut self, request: &str, answer: &str) -> Option<String> {
        if !self.connected {
            return None;
        }
        let thread = self.thread.clone()?;
        let turn = self
            .turns
            .iter_mut()
            .find(|t| t.requests.iter().any(|r| r.id == request))?;
        let state = turn.state.clone();
        let req = turn.requests.iter_mut().find(|r| r.id == request)?;
        if request_view(&state, req) != RequestView::Waiting
            || !req.answers.iter().any(|a| a.answer == answer)
        {
            return None;
        }
        req.sending = Some(answer.to_string());
        Some(thread)
    }

    fn refresh_mark(&mut self) {
        self.mark = if self.watching {
            if !self.waiting_threads.is_empty() {
                Mark::Waiting
            } else if !self.working_threads.is_empty() {
                Mark::Working
            } else if self.thread_mark == Mark::Done {
                Mark::Done
            } else {
                Mark::Idle
            }
        } else if self.connected && self.list_received {
            if self.threads.iter().any(|t| t.waiting) {
                Mark::Waiting
            } else if self.threads.iter().any(|t| t.working) {
                Mark::Working
            } else {
                self.thread_mark
            }
        } else if self.connected && self.waiting() {
            Mark::Waiting
        } else {
            self.thread_mark
        };
    }

    fn apply_status(&mut self, status: StatusInfo) -> bool {
        let no_harness = status.harnesses.is_empty();
        let lever_version = Some(status.lever_version);
        let changed = self.lever_version != lever_version
            || self.harness_started != status.harness_started
            || self.harnesses != status.harnesses
            || self.free_hosted_models != status.free_hosted_models
            || self.models_chosen != status.models_chosen
            || self.no_harness != no_harness
            || (status.default_mode.is_some() && self.default_mode != status.default_mode);
        let free_changed = self.free_hosted_models != status.free_hosted_models;
        let harness_came_up =
            !self.harness_started && status.harness_started && self.models.is_empty();
        if status.default_mode.is_some() {
            self.default_mode = status.default_mode;
        }
        self.lever_version = lever_version;
        self.harness_started = status.harness_started;
        self.harnesses = status.harnesses;
        self.free_hosted_models = status.free_hosted_models;
        self.models_chosen = status.models_chosen;
        self.no_harness = no_harness;
        if free_changed || harness_came_up {
            self.offer_known = false;
            self.modes_error = None;
            if free_changed {
                self.efforts.clear();
            }
            self.catalog_epoch = self.catalog_epoch.wrapping_add(1);
        }
        if self.no_harness {
            self.error = None;
        }
        changed
    }

    fn apply_resumed(&mut self, resumed: ResumedThread, drawer_up: bool) {
        self.sending = false;
        self.loading = false;
        self.title = if resumed.title.is_empty() {
            None
        } else {
            Some(resumed.title)
        };
        if self.thread.as_deref() != Some(resumed.thread.as_str()) {
            self.thread_commands = None;
        }
        self.thread = Some(resumed.thread);
        self.cwd = (!resumed.cwd.is_empty()).then_some(resumed.cwd);
        self.model = resumed.model;
        self.effort = resumed.effort;
        self.mode = resumed.mode;
        self.pending_mode = None;
        self.pending_model = None;
        self.pending_effort = None;
        self.active_turn = resumed
            .turns
            .last()
            .filter(|t| matches!(t.state, TurnState::Working))
            .map(|t| t.turn);
        let was_working = self.thread_mark == Mark::Working
            || self
                .working_before_disconnect
                .take()
                .is_some_and(|t| self.thread.as_deref() == Some(t.as_str()));
        let any_subagent_working = resumed
            .turns
            .iter()
            .any(|t| t.subagents.iter().any(|s| s.state.is_working()));
        let next_mark = if resumed.working || any_subagent_working {
            Mark::Working
        } else {
            match resumed.turns.last().map(|t| &t.state) {
                Some(TurnState::Working) => Mark::Working,
                Some(TurnState::Done) if was_working => Mark::Done,
                Some(TurnState::Stopped | TurnState::Failed(_) | TurnState::Interrupted)
                    if was_working && !drawer_up =>
                {
                    Mark::Done
                }
                _ if was_working => Mark::Idle,
                _ => self.thread_mark,
            }
        };
        self.turns = resumed.turns;
        self.thread_mark = next_mark;
    }

    /// Forgets the drawer's thread, as for a thread Lever no longer has.
    fn clear_thread(&mut self) {
        self.thread = None;
        self.title = None;
        self.cwd = None;
        self.model = None;
        self.effort = None;
        self.mode = None;
        self.active_turn = None;
        self.turns.clear();
        self.thread_commands = None;
        self.thread_mark = Mark::Idle;
    }

    pub fn select_thread(&mut self, thread: Option<String>) -> bool {
        if self.sending || self.loading {
            return false;
        }
        self.clear_thread();
        self.thread = thread;
        self.pending_mode = None;
        self.pending_model = None;
        self.pending_effort = None;
        self.working_before_disconnect = None;
        self.refresh_mark();
        self.error = None;
        self.missing_model = None;
        self.loading = self.thread.is_some();
        self.list_view = false;
        true
    }

    /// Folder order follows newest activity, as does each folder's thread order.
    pub fn groups(&self) -> Vec<(String, Vec<ThreadSummary>)> {
        let mut groups: Vec<(String, Vec<ThreadSummary>)> = Vec::new();
        for thread in self.threads.iter().filter(|t| !t.archived) {
            if let Some((_, threads)) = groups.iter_mut().find(|(cwd, _)| cwd == &thread.cwd) {
                threads.push(thread.clone());
            } else {
                groups.push((thread.cwd.clone(), vec![thread.clone()]));
            }
        }
        groups
    }

    /// Applies an event from Lever. Returns true if visible state changed.
    pub fn apply(&mut self, event: AgentEvent, drawer_up: bool) -> bool {
        let before = self.mark;
        let changed = self.apply_event(event, drawer_up);
        self.refresh_mark();
        changed || self.mark != before
    }

    fn apply_event(&mut self, event: AgentEvent, drawer_up: bool) -> bool {
        match event {
            AgentEvent::Threads { threads, finished } => {
                let ended = self.threads.iter().any(|old| {
                    old.working
                        && threads
                            .iter()
                            .any(|t| t.thread == old.thread && !t.working)
                });
                let busy = threads.iter().any(|t| t.working || t.waiting);
                if let Some(selected) = threads
                    .iter()
                    .find(|t| Some(t.thread.as_str()) == self.thread.as_deref())
                {
                    self.title = (!selected.title.is_empty()).then(|| selected.title.clone());
                    if !selected.cwd.is_empty() {
                        self.cwd = Some(selected.cwd.clone());
                    }
                    if selected.model.is_some() {
                        self.model = selected.model.clone();
                    }
                }
                self.threads = threads;
                self.list_received = true;
                if !busy && (finished || ended) && !drawer_up {
                    self.thread_mark = Mark::Done;
                }
                true
            }
            AgentEvent::Lifecycle {
                thread,
                deleted,
                warning,
            } => {
                if self.thread.as_deref() == Some(thread.as_str()) {
                    self.loading = false;
                }
                if warning.is_some() {
                    self.error = warning;
                }
                if deleted {
                    self.threads.retain(|t| t.thread != thread);
                } else if let Some(t) = self.threads.iter_mut().find(|t| t.thread == thread) {
                    t.archived = true;
                }
                if self.thread.as_deref() == Some(&thread) {
                    self.clear_thread();
                    self.pending_mode = None;
                    self.list_view = true;
                }
                true
            }
            AgentEvent::Connected {
                status,
                resumed,
                preserved_thread,
                resume_error,
            } => {
                let mut changed = !self.connected
                    || self.error != resume_error
                    || self.missing_model.is_some();
                self.connected = true;
                self.sending = false;
                self.error = resume_error;
                self.missing_model = None;
                self.catalog_epoch = self.catalog_epoch.wrapping_add(1);
                changed |= self.apply_status(status);
                if let Some(resumed) = resumed {
                    if self.thread.is_none() {
                        self.list_view = false;
                    }
                    self.apply_resumed(resumed, drawer_up);
                    changed = true;
                } else if let Some(tid) = preserved_thread {
                    if self.working_before_disconnect.as_deref() != Some(tid.as_str()) {
                        self.working_before_disconnect = None;
                    }
                    if self.thread.as_deref() != Some(tid.as_str()) {
                        self.clear_thread();
                        self.thread = Some(tid);
                        changed = true;
                    } else {
                        if self.active_turn.take().is_some() || self.thread_mark != Mark::Idle {
                            self.thread_mark = Mark::Idle;
                            changed = true;
                        }
                        for turn in &mut self.turns {
                            if matches!(turn.state, TurnState::Working) {
                                turn.state = TurnState::Interrupted;
                                changed = true;
                            }
                            let mut gone = Vec::new();
                            for sub in &mut turn.subagents {
                                if sub.state.is_working() {
                                    sub.state = SubagentState::Interrupted;
                                    gone.push(sub.id.clone());
                                    changed = true;
                                }
                            }
                            for req in &mut turn.requests {
                                if req.state == RequestState::Waiting
                                    && req.subagent.as_ref().is_some_and(|s| gone.contains(s))
                                {
                                    req.state = RequestState::Withdrawn;
                                }
                            }
                        }
                    }
                } else if self.thread.is_some() {
                    self.clear_thread();
                    self.working_before_disconnect = None;
                    changed = true;
                } else {
                    self.working_before_disconnect = None;
                }
                changed
            }
            AgentEvent::Disconnected => {
                self.loading = false;
                let changed = self.connected
                    || self.sending
                    || self.lever_version.is_some()
                    || self.harness_started
                    || !self.harnesses.is_empty()
                    || self.no_harness
                    || self.missing_model.is_some()
                    || self.modes_loading
                    || self.thread_mark != Mark::Idle;
                if self.working()
                    && let Some(thread) = self.thread.clone()
                {
                    self.working_before_disconnect = Some(thread);
                }
                self.watching = false;
                self.working_threads.clear();
                self.waiting_threads.clear();
                self.modes_loading = false;
                self.modes_error = None;
                self.offer_known = false;
                self.offer_unsupported = false;
                self.sign_in_launched = false;
                self.efforts.clear();
                self.catalog_epoch = self.catalog_epoch.wrapping_add(1);
                self.connected = false;
                self.sending = false;
                self.active_turn = None;
                self.lever_version = None;
                self.harness_started = false;
                self.harnesses.clear();
                self.free_hosted_models = false;
                self.models_chosen = false;
                self.no_harness = false;
                self.missing_model = None;
                self.thread_mark = Mark::Idle;
                changed
            }
            AgentEvent::Status(status) => self.apply_status(status),
            AgentEvent::Recovered(ref resumed)
                if self.thread.as_deref() != Some(resumed.thread.as_str()) =>
            {
                false
            }
            AgentEvent::Resumed(resumed) | AgentEvent::Recovered(resumed) => {
                self.error = None;
                self.apply_resumed(resumed, drawer_up);
                true
            }
            AgentEvent::Sent { thread, turn } => {
                self.sending = false;
                self.working_before_disconnect = None;
                if self.thread.as_deref() != Some(thread.as_str()) {
                    self.turns.clear();
                    self.title = None;
                    self.model = self.new_thread_model();
                    self.effort = self.new_thread_effort();
                    self.pending_model = None;
                    self.pending_effort = None;
                    self.thread_commands = None;
                    self.mode = self
                        .pending_mode
                        .take()
                        .or_else(|| self.default_mode.clone());
                }
                self.thread = Some(thread);
                self.active_turn = Some(turn);
                self.error = None;
                self.missing_model = None;
                self.thread_mark = Mark::Working;
                true
            }
            AgentEvent::Message {
                thread,
                turn,
                text,
                context,
                from_harness,
            } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                if !from_harness {
                    self.sending = false;
                }
                self.active_turn = Some(turn);
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.message = text;
                    existing.context = context;
                    if from_harness {
                        existing.from_harness = true;
                        existing.state = TurnState::Working;
                        existing.ended = None;
                    }
                } else {
                    self.turns.push(Turn {
                        turn,
                        message: text,
                        context,
                        acts: Vec::new(),
                        notes: Vec::new(),
                        requests: Vec::new(),
                        subagents: Vec::new(),
                        mode: self.mode.clone(),
                        answer: String::new(),
                        state: TurnState::Working,
                        started: Some(unix_now()),
                        ended: None,
                        from_harness,
                        steered: Vec::new(),
                        queued: Vec::new(),
                    });
                }
                self.error = None;
                self.thread_mark = Mark::Working;
                true
            }
            AgentEvent::SteerTaken { thread, .. } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                self.sending = false;
                self.error = None;
                true
            }
            AgentEvent::QueueTaken { thread, .. } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                self.sending = false;
                self.error = None;
                true
            }
            AgentEvent::Queued {
                thread,
                turn,
                queued,
            } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) else {
                    return false;
                };
                let gone = matches!(queued.state.as_str(), "sent" | "removed");
                match existing.queued.iter().position(|q| q.id == queued.id) {
                    Some(at) if gone => {
                        existing.queued.remove(at);
                    }
                    Some(at) => existing.queued[at] = queued,
                    None if gone => {}
                    None => existing.queued.push(queued),
                }
                true
            }
            AgentEvent::Steered {
                thread,
                turn,
                mut steer,
            } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    steer.at = existing.answer.len();
                    existing.steered.push(steer);
                    return true;
                }
                false
            }
            AgentEvent::Text { thread, turn, text } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.answer.push_str(&text);
                    return true;
                }
                false
            }
            AgentEvent::Act { thread, turn, act } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                if let Some(existing_turn) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    if let Some(existing_act) =
                        existing_turn.acts.iter_mut().find(|a| a.id == act.id)
                    {
                        *existing_act = act;
                    } else {
                        existing_turn.acts.push(act);
                    }
                    return true;
                }
                false
            }
            AgentEvent::Note { thread, turn, note } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                if let Some(existing_turn) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    if let Some(existing_note) =
                        existing_turn.notes.iter_mut().find(|n| n.id == note.id)
                    {
                        existing_note.text = note.text;
                    } else {
                        existing_turn.notes.push(note);
                    }
                    return true;
                }
                false
            }
            AgentEvent::Request {
                thread,
                turn,
                request,
            } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                let Some(existing_turn) = self.turns.iter_mut().find(|t| t.turn == turn) else {
                    return false;
                };
                if let Some(existing) = existing_turn
                    .requests
                    .iter_mut()
                    .find(|r| r.id == request.id)
                {
                    *existing = request;
                } else {
                    existing_turn.requests.push(request);
                }
                true
            }
            AgentEvent::Subagent {
                thread,
                turn,
                subagent,
            } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                let Some(existing_turn) = self.turns.iter_mut().find(|t| t.turn == turn) else {
                    return false;
                };
                let is_working = subagent.state.is_working();
                let is_done = matches!(subagent.state, SubagentState::Done);
                if let Some(existing) = existing_turn
                    .subagents
                    .iter_mut()
                    .find(|s| s.id == subagent.id)
                {
                    if !subagent.title.trim().is_empty() {
                        existing.title = subagent.title;
                    }
                    existing.state = subagent.state;
                    if subagent.started.is_some() {
                        existing.started = subagent.started;
                    }
                    if subagent.ended.is_some() {
                        existing.ended = subagent.ended;
                    } else if is_working {
                        existing.ended = None;
                    }
                } else {
                    existing_turn.subagents.push(subagent);
                }
                if is_working {
                    self.thread_mark = Mark::Working;
                } else if !self
                    .turns
                    .last()
                    .is_some_and(|t| matches!(t.state, TurnState::Working))
                    && !self.subagent_working()
                {
                    self.thread_mark = if is_done || !drawer_up {
                        Mark::Done
                    } else {
                        Mark::Idle
                    };
                }
                true
            }
            AgentEvent::SubagentText {
                thread,
                turn,
                subagent,
                role,
                text,
            } => {
                let Some(existing_sub) = self.subagent_mut(&thread, turn, &subagent) else {
                    return false;
                };
                if let Some(last) = existing_sub.messages.last_mut()
                    && last.role == role
                {
                    if role == "prompt" && last.text.trim() == text.trim() {
                        return false;
                    }
                    last.text.push_str(&text);
                } else {
                    existing_sub.messages.push(SubagentMessage { role, text });
                }
                true
            }
            AgentEvent::SubagentAct {
                thread,
                turn,
                subagent,
                act,
            } => {
                let Some(existing_sub) = self.subagent_mut(&thread, turn, &subagent) else {
                    return false;
                };
                if let Some(existing_act) = existing_sub.acts.iter_mut().find(|a| a.id == act.id) {
                    *existing_act = act;
                } else {
                    existing_sub.acts.push(act);
                }
                true
            }
            AgentEvent::ThreadUpdated {
                thread,
                title,
                cwd,
                mode,
                model,
                effort,
            } => {
                let mut changed = false;
                if let Some(cwd_val) = &cwd {
                    if let Some(summary) = self.threads.iter_mut().find(|t| t.thread == thread)
                        && summary.cwd != *cwd_val
                    {
                        summary.cwd = cwd_val.clone();
                        changed = true;
                    }
                }
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return changed;
                }
                if let Some(cwd) = cwd
                    && self.cwd.as_deref() != Some(cwd.as_str())
                {
                    self.cwd = Some(cwd);
                    changed = true;
                }
                if let Some(model) = model
                    && self.model.as_deref() != Some(model.as_str())
                {
                    self.model = Some(model);
                    changed = true;
                }
                if let Some(effort) = effort
                    && self.effort != effort
                {
                    self.effort = effort;
                    changed = true;
                }
                if let Some(title) = title {
                    let next = if title.is_empty() { None } else { Some(title) };
                    if self.title != next {
                        self.title = next;
                        changed = true;
                    }
                }
                if let Some(mode) = mode
                    && self.mode.as_deref() != Some(mode.as_str())
                {
                    self.mode = Some(mode);
                    changed = true;
                }
                changed
            }
            AgentEvent::ProjectMade { thread, cwd, .. } => {
                self.loading = false;
                self.error = None;
                if let Some(summary) = self.threads.iter_mut().find(|t| t.thread == thread) {
                    summary.cwd = cwd.clone();
                }
                if self.thread.as_deref() == Some(thread.as_str()) {
                    self.cwd = Some(cwd);
                }
                true
            }
            AgentEvent::Dirs(_) => false,
            AgentEvent::ModeSet { thread, mode } => {
                if self.thread.as_deref() != Some(thread.as_str())
                    || self.mode.as_deref() == Some(mode.as_str())
                {
                    return false;
                }
                self.mode = Some(mode);
                true
            }
            AgentEvent::DefaultMode(mode) => {
                if self.default_mode.as_deref() == Some(mode.as_str()) {
                    return false;
                }
                self.default_mode = Some(mode);
                true
            }
            AgentEvent::Offer(offer) => {
                let was_choosing = self.needs_model_choice();
                self.modes_loading = false;
                self.modes_error = None;
                let prev_ids: Vec<String> = self.models.iter().map(|m| m.id.clone()).collect();
                self.modes = offer.modes;
                self.models = offer.models;
                self.default_model = offer.default_model;
                self.free_hosted_models = offer.free_hosted_models;
                self.steering = offer.steering;
                self.offer_known = true;
                self.offer_unsupported = !offer.supported;
                self.models_chosen =
                    !offer.supported || !self.models.is_empty() || self.free_hosted_models;
                let new_ids: Vec<String> = self.models.iter().map(|m| m.id.clone()).collect();
                if new_ids != prev_ids {
                    self.efforts
                        .retain(|id, _| self.models.iter().any(|m| &m.id == id));
                }
                if !self.models.is_empty() {
                    self.missing_model = None;
                    self.error = None;
                    self.sign_in_launched = false;
                }
                if was_choosing
                    && !self.needs_model_choice()
                    && self.threads.is_empty()
                    && self.thread.is_none()
                {
                    self.list_view = false;
                }
                true
            }
            AgentEvent::FreeModels {
                enabled,
                models_chosen,
            } => {
                let was_choosing = self.needs_model_choice();
                let next_chosen = models_chosen.unwrap_or(enabled || self.models_chosen);
                let changed =
                    self.free_hosted_models != enabled || self.models_chosen != next_chosen;
                self.free_hosted_models = enabled;
                self.models_chosen = next_chosen;
                if enabled {
                    self.missing_model = None;
                    self.error = None;
                    self.sign_in_launched = false;
                }
                if changed {
                    self.offer_known = false;
                    self.offer_unsupported = false;
                    self.models.clear();
                    self.default_model = None;
                    self.efforts.clear();
                    self.modes_loading = false;
                    self.modes_error = None;
                    self.catalog_epoch = self.catalog_epoch.wrapping_add(1);
                }
                if was_choosing
                    && !self.needs_model_choice()
                    && self.threads.is_empty()
                    && self.thread.is_none()
                {
                    self.list_view = false;
                }
                changed || was_choosing != self.needs_model_choice()
            }
            AgentEvent::Efforts { model, efforts } => {
                self.efforts.insert(model, efforts);
                true
            }
            AgentEvent::Commands {
                thread,
                commands,
                known,
            } => {
                if !known {
                    return false;
                }
                match thread {
                    Some(thread) if self.thread.as_deref() == Some(thread.as_str()) => {
                        let changed = self.thread_commands.as_ref() != Some(&commands);
                        self.thread_commands = Some(commands);
                        changed
                    }
                    Some(_) => false,
                    None => {
                        let changed = self.commands != commands;
                        self.commands = commands;
                        changed
                    }
                }
            }
            AgentEvent::ModelSet { thread, model } => {
                if self.thread.as_deref() != Some(thread.as_str())
                    || self.model.as_deref() == Some(model.as_str())
                {
                    return false;
                }
                self.model = Some(model);
                true
            }
            AgentEvent::EffortSet { thread, effort } => {
                if self.thread.as_deref() != Some(thread.as_str()) || self.effort == effort {
                    return false;
                }
                self.effort = effort;
                true
            }
            AgentEvent::Activity { working, waiting } => {
                let was_busy = self.watching
                    && (!self.working_threads.is_empty() || !self.waiting_threads.is_empty());
                let busy = !working.is_empty() || !waiting.is_empty();
                if was_busy && !busy && !drawer_up {
                    self.thread_mark = Mark::Done;
                } else if busy && self.thread_mark != Mark::Working {
                    self.thread_mark = Mark::Idle;
                }
                let changed = !self.watching
                    || self.working_threads != working
                    || self.waiting_threads != waiting;
                self.watching = true;
                self.working_threads = working;
                self.waiting_threads = waiting;
                changed
            }
            AgentEvent::Done { thread, turn } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.state = TurnState::Done;
                    existing.ended.get_or_insert_with(unix_now);
                }
                if self.active_turn == Some(turn) {
                    self.active_turn = None;
                }
                self.thread_mark = self.mark_after_turn_ends(drawer_up, true);
                true
            }
            AgentEvent::Stopped { thread, turn } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.state = TurnState::Stopped;
                    existing.ended.get_or_insert_with(unix_now);
                }
                if self.active_turn == Some(turn) {
                    self.active_turn = None;
                }
                self.thread_mark = self.mark_after_turn_ends(drawer_up, false);
                true
            }
            AgentEvent::Failed {
                thread,
                turn,
                message,
            } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                self.sending = false;
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.state = TurnState::Failed(calm_turn_error(&message));
                    existing.ended.get_or_insert_with(unix_now);
                }
                if self.active_turn == Some(turn) {
                    self.active_turn = None;
                }
                self.thread_mark = self.mark_after_turn_ends(drawer_up, false);
                true
            }
            AgentEvent::Interrupted { thread, turn } => {
                if self.thread.as_deref() != Some(thread.as_str()) {
                    return false;
                }
                self.sending = false;
                if let Some(existing) = self.turns.iter_mut().find(|t| t.turn == turn) {
                    existing.state = TurnState::Interrupted;
                    existing.ended.get_or_insert_with(unix_now);
                }
                if self.active_turn == Some(turn) {
                    self.active_turn = None;
                }
                self.thread_mark = self.mark_after_turn_ends(drawer_up, false);
                true
            }
            AgentEvent::Error { code, message } => {
                self.loading = false;
                self.sending = false;
                for turn in &mut self.turns {
                    for request in &mut turn.requests {
                        request.sending = None;
                    }
                }
                if self.modes_loading
                    && !matches!(code.as_str(), "no-model" | "no-thread" | "disconnected")
                {
                    self.modes_loading = false;
                    self.modes_error =
                        Some("Models and modes are not available right now.".into());
                    return true;
                }
                self.modes_loading = false;
                if code == "no-model" {
                    self.missing_model = Some(message);
                    self.error = None;
                } else if code == "no-thread" {
                    self.clear_thread();
                    self.working_before_disconnect = None;
                    self.error = None;
                } else if code == "no-queued" {
                    self.error = Some("That message no longer waits.".into());
                } else if code == "no-request" {
                    self.error = Some("That request no longer waits for an answer.".into());
                } else if code == "invalid-mode" {
                    self.error = Some("The Agent does not offer that mode.".into());
                } else if code == "invalid-model" {
                    self.error = Some("The Agent does not offer that Model.".into());
                } else if code == "invalid-effort" {
                    self.error = Some("This thread's Model does not offer that effort.".into());
                } else if code == "working" {
                    self.error =
                        Some("The Agent is working on this thread. /stop ends the turn.".into());
                } else {
                    self.error = Some(calm_turn_error(&message));
                }
                true
            }
        }
    }

    /// Summoning the drawer acknowledges a finished turn's `Done` mark.
    pub fn on_drawer_opened(&mut self) -> bool {
        self.acknowledge_done()
    }

    /// Putting the drawer away clears `Done` (already seen in the drawer),
    /// while leaving `Working` and `Waiting` so the mark tracks what goes on
    /// behind it.
    pub fn on_drawer_closed(&mut self) -> bool {
        self.acknowledge_done()
    }

    fn acknowledge_done(&mut self) -> bool {
        let before = self.mark;
        if self.thread_mark == Mark::Done {
            self.thread_mark = Mark::Idle;
        }
        self.refresh_mark();
        self.mark != before
    }
}
