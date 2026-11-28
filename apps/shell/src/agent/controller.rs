// SPDX-License-Identifier: MIT
//! In-process Model-View-Update (MVU) reducer for the Agent's surface.
//!
//! Consolidates `AgentState`, per-thread drafts, submitted-message
//! reconciliation, the "Make this a project" form, slash-menu highlight and
//! fetch guards, `catalog_epoch` invalidation, onboarding focus transitions,
//! and Mark transitions behind a pure `handle_intent` / `handle_event` ->
//! `Vec<AgentEffect>` seam. `AgentSurface::render` performs zero Lever IPC and
//! zero `Model`/entity mutations during paint, and the Agent UX is
//! unit-testable in-process without GPUI.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::state::{AgentState, ModePick, SettingPick};
use super::types::{
    AgentCommand, AgentEvent, DirEntry, DirListing, ListSnapshot, Mark, RequestView,
    draft_after_send, request_view,
};
use crate::slash::{self, Menu, MenuKind, Resolve, Run, Take, Wants};

/// Actions triggered by the person from the Agent drawer's controls and shortcuts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadAction {
    List,
    New,
    Open(String),
    Archive,
    AskMakeProject,
    CancelMakeProject,
    ConfirmMakeProject,
    PickProjectPlace(String),
    ToggleProjectBrowse,
    BrowseProjectDir(String),
    UseProjectFolder(String),
    AskDelete,
    Delete,
    CancelDelete,
    ChooseFreeModels(bool),
    SignIn,
    RefreshModels,
    Act(String),
    /// Show all of what an act gave, or its first lines again.
    ActWhole(String),
    Context(u64),
    /// Open or fold the acts of a turn.
    Acts(u64),
    /// Open or fold a subagent in its turn.
    Subagent(String),
    /// Take back a message that waits behind a turn.
    Unqueue(String),
}

/// Which focusable element inside the Agent drawer should hold keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusTarget {
    Surface,
    Composer,
    ProjectName,
    ProjectFolder,
}

/// All intents from the person or shell handled by [`AgentController`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentIntent {
    DrawerOpened,
    DrawerClosed,
    TakeKeyboard,
    DraftChanged(String),
    ProjectNameChanged(String),
    ProjectFolderChanged(String),
    Enter,
    TakeMenuRow,
    ClickMenuRow(usize),
    HoverMenuRow(usize),
    StepMenu(isize),
    StepProvider(isize),
    SelectProvider(String),
    Dismiss,
    StopTurn,
    AnswerRequest { request: String, answer: String },
    AnswerWaiting(usize),
    Action(ThreadAction),
    ModelPickerSignIn,
    ModelPickerFreeModels(bool),
    SignInLaunchResult(Result<(), String>),
}

/// Declarative side effects returned by [`AgentController`] for the shell
/// runtime and GPUI view to carry out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEffect {
    SendLeverCommand(AgentCommand),
    SavePicks {
        model: Option<String>,
        effort: Option<String>,
    },
    LaunchSignIn,
    AgentKeyboard(bool),
    SyncMarkOverlay,
    SyncAgentView,
    SetComposerText(String),
    SetProjectNameText(String),
    SetProjectFolderText(String),
    SyncComposerMenu {
        list_open: bool,
        rail_open: bool,
    },
    Focus(FocusTarget),
    ScrollToBottom,
    ScrollToBottomIfNearBottom,
    ResetScroll,
    LogMarkChange {
        prev: Mark,
        next: Mark,
    },
    LogRequestAnswer(String),
    NotifyDrawer,
    NotifyMarkOverlay,
    RefreshWindows,
}

/// State of the inline "Make this a project" form inside the drawer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectFormState {
    pub making: bool,
    pub name: String,
    pub folder: String,
    /// Parent place (`~/Projects`, `~/Code`, `~`, etc.) while the folder field
    /// tracks `<place>/<name>`; `None` once the person edits Folder directly or
    /// chooses an exact folder.
    pub place: Option<String>,
    pub browse_open: bool,
    pub browse_loading: bool,
    pub browse_dir: Option<String>,
    pub browse_parent: Option<String>,
    pub browse_home: Option<String>,
    pub browse_entries: Vec<DirEntry>,
    pub error: Option<String>,
}

impl Default for ProjectFormState {
    fn default() -> Self {
        Self {
            making: false,
            name: String::new(),
            folder: String::new(),
            place: Some("~/Projects".into()),
            browse_open: false,
            browse_loading: false,
            browse_dir: None,
            browse_parent: None,
            browse_home: None,
            browse_entries: Vec::new(),
            error: None,
        }
    }
}

impl ProjectFormState {
    pub fn close(&mut self) -> bool {
        let was_open = self.making || self.browse_open || self.browse_loading;
        self.making = false;
        self.browse_open = false;
        self.browse_loading = false;
        self.browse_dir = None;
        self.browse_parent = None;
        self.browse_entries.clear();
        self.error = None;
        was_open
    }
}

/// A suggested parent folder in the "Make this a project" form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceSuggestion {
    pub label: String,
    pub value: String,
}

/// Narrow read-only projection of [`AgentController`] for one `AgentSurface`
/// render pass, omitting saved drafts, in-flight submission tracking, and
/// slash-menu fetch guards.
#[derive(Clone, Debug)]
pub struct AgentRenderSnapshot {
    pub state: AgentState,
    pub draft: String,
    pub menu: Option<Menu>,
    pub list_snapshot: Option<ListSnapshot>,
    pub highlight: usize,
    pub confirm_delete: bool,
    pub project: ProjectFormState,
    pub project_form_open: bool,
    pub expanded_acts: HashSet<String>,
    pub whole_acts: HashSet<String>,
    pub expanded_context: HashSet<u64>,
    pub opened_turns: HashSet<u64>,
    pub expanded_subagents: HashSet<String>,
}

/// Deep in-process MVU controller owning all Agent domain and drawer UI state.
#[derive(Debug)]
pub struct AgentController {
    state: AgentState,
    drawer_up: bool,
    drafts: HashMap<String, String>,
    current_draft: String,
    submitted: Option<(String, String)>,
    confirm_delete: bool,
    project: ProjectFormState,
    expanded_acts: HashSet<String>,
    whole_acts: HashSet<String>,
    expanded_context: HashSet<u64>,
    opened_turns: HashSet<u64>,
    expanded_subagents: HashSet<String>,
    highlight: usize,
    list_for: String,
    dismissed: Option<String>,
    said_of: Option<String>,
    provider: Option<String>,
    effort_after: Option<String>,
    seen_catalog_epoch: u64,
    last_menu_kind: Option<MenuKind>,
    asked_offer: bool,
    asked_efforts: HashSet<String>,
    asked_commands: Option<Option<String>>,
    was_choosing: bool,
}

impl AgentController {
    pub fn new(
        configured: bool,
        last_model: Option<String>,
        last_effort: Option<String>,
    ) -> Self {
        let mut state = AgentState::new(configured);
        state.last_model = last_model;
        state.last_effort = last_effort;
        let seen_catalog_epoch = state.catalog_epoch;
        let was_choosing = state.needs_model_choice();
        Self {
            state,
            drawer_up: false,
            drafts: HashMap::new(),
            current_draft: String::new(),
            submitted: None,
            confirm_delete: false,
            project: ProjectFormState::default(),
            expanded_acts: HashSet::new(),
            whole_acts: HashSet::new(),
            expanded_context: HashSet::new(),
            opened_turns: HashSet::new(),
            expanded_subagents: HashSet::new(),
            highlight: 0,
            list_for: String::new(),
            dismissed: None,
            said_of: None,
            provider: None,
            effort_after: None,
            seen_catalog_epoch,
            last_menu_kind: None,
            asked_offer: false,
            asked_efforts: HashSet::new(),
            asked_commands: None,
            was_choosing,
        }
    }

    pub fn state(&self) -> &AgentState {
        &self.state
    }

    #[cfg(test)]
    pub fn drawer_up(&self) -> bool {
        self.drawer_up
    }

    #[cfg(test)]
    pub fn draft_for(&self, key: &str) -> Option<&str> {
        self.drafts.get(key).map(String::as_str)
    }

    #[cfg(test)]
    pub fn project(&self) -> &ProjectFormState {
        &self.project
    }

    pub fn confirm_delete(&self) -> bool {
        self.confirm_delete
    }

    #[cfg(test)]
    pub fn current_draft(&self) -> &str {
        &self.current_draft
    }

    pub fn project_form_open(&self) -> bool {
        self.project.making && self.state.thread.is_some() && !self.state.list_view
    }

    /// Reconciles controller state when GPUI reports the drawer window closed
    /// externally before a summon gesture.
    pub fn mark_drawer_closed_externally(&mut self) {
        self.drawer_up = false;
        self.reset_ephemeral_drawer_ui();
    }

    /// Builds a narrow read-only snapshot for a single `AgentSurface::render`
    /// frame without cloning saved drafts, submission tracking, or fetch guards.
    pub fn render_snapshot(&self) -> AgentRenderSnapshot {
        let menu = self.menu();
        let list_snapshot = self.list_snapshot(menu.as_ref());
        AgentRenderSnapshot {
            state: self.state.clone(),
            draft: self.current_draft.clone(),
            menu,
            list_snapshot,
            highlight: self.highlight,
            confirm_delete: self.confirm_delete,
            project: self.project.clone(),
            project_form_open: self.project_form_open(),
            expanded_acts: self.expanded_acts.clone(),
            whole_acts: self.whole_acts.clone(),
            expanded_context: self.expanded_context.clone(),
            opened_turns: self.opened_turns.clone(),
            expanded_subagents: self.expanded_subagents.clone(),
        }
    }

    /// Pure read-only calculation of the open slash menu, if any.
    pub fn menu(&self) -> Option<Menu> {
        if self.state.needs_model_choice()
            || self.state.list_view
            || self.confirm_delete
            || self.project_form_open()
        {
            return None;
        }
        if self.dismissed.as_deref() == Some(self.current_draft.as_str()) {
            return None;
        }
        slash::menu(&self.current_draft, &self.state, self.provider.as_deref())
    }

    /// Snapshot of the open slash list for the control socket.
    pub fn list_snapshot(&self, menu: Option<&Menu>) -> Option<ListSnapshot> {
        menu.map(|menu| ListSnapshot {
            kind: format!("{:?}", menu.kind).to_lowercase(),
            items: menu.items.iter().map(|item| item.id.clone()).collect(),
            highlighted: menu
                .items
                .get(self.highlight.min(menu.items.len().saturating_sub(1)))
                .map(|item| item.id.clone()),
            provider: menu.provider.clone(),
            note: menu.note.clone(),
        })
    }

    /// Resets transient per-drawer-session UI state while preserving `state`,
    /// `drafts`, and `submitted`.
    fn reset_ephemeral_drawer_ui(&mut self) {
        self.current_draft.clear();
        self.confirm_delete = false;
        self.project = ProjectFormState::default();
        self.expanded_acts.clear();
        self.whole_acts.clear();
        self.expanded_context.clear();
        self.opened_turns.clear();
        self.expanded_subagents.clear();
        self.highlight = 0;
        self.list_for.clear();
        self.dismissed = None;
        self.said_of = None;
        self.provider = None;
        self.effort_after = None;
        self.seen_catalog_epoch = self.state.catalog_epoch;
        self.last_menu_kind = None;
        self.asked_offer = false;
        self.asked_efforts.clear();
        self.asked_commands = None;
        self.was_choosing = self.state.needs_model_choice();
    }

    /// Handles an event arriving from Lever's surface socket.
    pub fn handle_event(&mut self, event: AgentEvent) -> Vec<AgentEffect> {
        let mut effects = Vec::new();
        let is_sent = matches!(
            event,
            AgentEvent::Sent { .. }
                | AgentEvent::SteerTaken { .. }
                | AgentEvent::QueueTaken { .. }
        );
        let is_disconnected = matches!(event, AgentEvent::Disconnected);
        let is_lifecycle = matches!(event, AgentEvent::Lifecycle { .. });
        let is_project_made = matches!(event, AgentEvent::ProjectMade { .. });
        let is_error = matches!(event, AgentEvent::Error { .. });
        let dirs_listing = match &event {
            AgentEvent::Dirs(listing) => Some(listing.clone()),
            _ => None,
        };
        let should_scroll = matches!(
            event,
            AgentEvent::Message { .. }
                | AgentEvent::Steered { .. }
                | AgentEvent::Queued { .. }
                | AgentEvent::Act { .. }
                | AgentEvent::Note { .. }
                | AgentEvent::Request { .. }
                | AgentEvent::Subagent { .. }
                | AgentEvent::SubagentText { .. }
                | AgentEvent::SubagentAct { .. }
                | AgentEvent::Text { .. }
                | AgentEvent::Done { .. }
                | AgentEvent::Stopped { .. }
                | AgentEvent::Failed { .. }
                | AgentEvent::Interrupted { .. }
                | AgentEvent::Resumed(_)
                | AgentEvent::Recovered(_)
                | AgentEvent::Connected {
                    resumed: Some(_),
                    ..
                }
        );
        let turn_ended = matches!(
            event,
            AgentEvent::Done { .. }
                | AgentEvent::Stopped { .. }
                | AgentEvent::Failed { .. }
                | AgentEvent::Interrupted { .. }
        ) || matches!(
            &event,
            AgentEvent::Subagent { subagent, .. } if !subagent.state.is_working()
        );

        let prev_mark = self.state.mark;
        let changed = self.state.apply(event, self.drawer_up);
        let needs_status_refresh =
            changed && (turn_ended || (is_sent && !self.state.harness_started));
        if needs_status_refresh && self.state.configured {
            effects.push(AgentEffect::SendLeverCommand(AgentCommand::RefreshStatus));
        }

        let next_mark = self.state.mark;
        let mark_changed = next_mark != prev_mark;
        if mark_changed {
            effects.push(AgentEffect::LogMarkChange {
                prev: prev_mark,
                next: next_mark,
            });
        }
        effects.push(AgentEffect::SyncMarkOverlay);
        if mark_changed {
            effects.push(AgentEffect::NotifyMarkOverlay);
        }

        let mut notify_drawer = false;
        if self.drawer_up {
            if (self.state.list_view || self.state.thread.is_none()) && self.project.close() {
                notify_drawer = true;
            }
            if is_lifecycle && self.state.list_view {
                effects.push(AgentEffect::Focus(FocusTarget::Surface));
            }
            if (is_disconnected || is_error) && self.on_agent_error() {
                notify_drawer = true;
            }
            if is_project_made && self.project.making {
                self.project.close();
                effects.push(AgentEffect::Focus(FocusTarget::Composer));
                effects.push(AgentEffect::SyncAgentView);
                notify_drawer = true;
            }
            if let Some(listing) = dirs_listing
                && self.on_dirs_listed(listing)
            {
                effects.push(AgentEffect::SyncAgentView);
                notify_drawer = true;
            }
        }

        if is_disconnected {
            self.submitted = None;
        }

        if is_sent {
            if let Some((key, submitted_text)) = self.submitted.take() {
                let new_key = draft_key(&self.state);
                let open_input = self.drawer_up.then_some(self.current_draft.as_str());
                if let Some(next) = reconcile_draft_on_sent(
                    &mut self.drafts,
                    &key,
                    &new_key,
                    &submitted_text,
                    open_input,
                ) {
                    if self.dismissed.as_deref().is_some_and(|closed| closed != next) {
                        self.dismissed = None;
                    }
                    self.current_draft = next.clone();
                    effects.push(AgentEffect::SetComposerText(next));
                }
            }
            if self.drawer_up {
                effects.push(AgentEffect::ScrollToBottomIfNearBottom);
                notify_drawer = true;
            }
        } else if changed && should_scroll && self.drawer_up {
            effects.push(AgentEffect::ScrollToBottomIfNearBottom);
            notify_drawer = true;
        }

        if self.drawer_up {
            self.reconcile_ui(&mut effects);
        }

        if mark_changed {
            effects.push(AgentEffect::RefreshWindows);
        } else if (changed || notify_drawer) && self.drawer_up {
            effects.push(AgentEffect::NotifyDrawer);
        }

        effects
    }

    /// Handles an intent from the person or shell.
    pub fn handle_intent(&mut self, intent: AgentIntent) -> Vec<AgentEffect> {
        let mut effects = Vec::new();
        match intent {
            AgentIntent::DrawerOpened => {
                self.drawer_up = true;
                self.reset_ephemeral_drawer_ui();
                let prev_mark = self.state.mark;
                if self.state.on_drawer_opened() {
                    effects.push(AgentEffect::LogMarkChange {
                        prev: prev_mark,
                        next: self.state.mark,
                    });
                }
                let draft = self
                    .drafts
                    .get(&draft_key(&self.state))
                    .cloned()
                    .unwrap_or_default();
                self.current_draft = draft.clone();
                if !draft.is_empty() {
                    effects.push(AgentEffect::SetComposerText(draft));
                }
                if !self.state.turns.is_empty() {
                    effects.push(AgentEffect::ScrollToBottom);
                }
                let target = if self.state.list_view || self.state.needs_model_choice() {
                    FocusTarget::Surface
                } else {
                    FocusTarget::Composer
                };
                effects.push(AgentEffect::Focus(target));
                if self.state.configured {
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::RefreshStatus));
                }
                self.reconcile_ui(&mut effects);
                effects.push(AgentEffect::SyncMarkOverlay);
                effects.push(AgentEffect::AgentKeyboard(true));
                effects.push(AgentEffect::RefreshWindows);
            }
            AgentIntent::DrawerClosed => {
                self.drawer_up = false;
                let prev_mark = self.state.mark;
                if self.state.on_drawer_closed() {
                    effects.push(AgentEffect::LogMarkChange {
                        prev: prev_mark,
                        next: self.state.mark,
                    });
                }
                self.reset_ephemeral_drawer_ui();
                effects.push(AgentEffect::SyncMarkOverlay);
                effects.push(AgentEffect::AgentKeyboard(false));
                effects.push(AgentEffect::RefreshWindows);
            }
            AgentIntent::TakeKeyboard => {
                let target = if self.state.list_view || self.state.needs_model_choice() {
                    FocusTarget::Surface
                } else {
                    FocusTarget::Composer
                };
                effects.push(AgentEffect::Focus(target));
                if self.state.configured {
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::RefreshStatus));
                }
                effects.push(AgentEffect::AgentKeyboard(true));
            }
            AgentIntent::DraftChanged(text) => {
                let text_changed = text != self.current_draft;
                self.current_draft = text.clone();
                if self.dismissed.as_deref().is_some_and(|closed| closed != text) {
                    self.dismissed = None;
                }
                if !self.state.list_view {
                    if text_changed && self.said_of.as_deref() != Some(text.as_str()) {
                        self.said_of = None;
                        self.state.error = None;
                    }
                    let key = draft_key(&self.state);
                    if text.trim().is_empty() || slash::is_drawer_command(&text) {
                        self.drafts.remove(&key);
                    } else {
                        self.drafts.insert(key, text);
                    }
                }
                self.reconcile_ui(&mut effects);
                effects.push(AgentEffect::NotifyDrawer);
            }
            AgentIntent::ProjectNameChanged(name) => {
                let changed = name != self.project.name;
                self.project.name = name.clone();
                if changed {
                    self.project.error = None;
                    self.state.error = None;
                }
                if let Some(place) = self.project.place.clone() {
                    let offered = join_project_place(&place, &name);
                    if self.project.folder != offered {
                        self.project.folder = offered.clone();
                        effects.push(AgentEffect::SetProjectFolderText(offered));
                    }
                }
                effects.push(AgentEffect::NotifyDrawer);
            }
            AgentIntent::ProjectFolderChanged(folder) => {
                let changed = folder != self.project.folder;
                self.project.folder = folder.clone();
                if changed {
                    self.project.error = None;
                    self.state.error = None;
                }
                if let Some(place) = self.project.place.as_deref() {
                    let offered = join_project_place(place, &self.project.name);
                    if folder != offered {
                        self.project.place = None;
                    }
                }
                effects.push(AgentEffect::NotifyDrawer);
            }
            AgentIntent::Enter => {
                self.enter(&mut effects);
            }
            AgentIntent::TakeMenuRow => {
                if let Some(menu) = self.menu() {
                    self.take(&menu, &mut effects);
                }
            }
            AgentIntent::ClickMenuRow(at) => {
                self.highlight = at;
                if let Some(menu) = self.menu() {
                    self.take(&menu, &mut effects);
                }
                if !effects.iter().any(|e| matches!(e, AgentEffect::Focus(_))) {
                    effects.push(AgentEffect::Focus(FocusTarget::Composer));
                }
            }
            AgentIntent::HoverMenuRow(at) => {
                if self.highlight != at {
                    self.highlight = at;
                    effects.push(AgentEffect::NotifyDrawer);
                }
            }
            AgentIntent::StepMenu(by) => {
                if let Some(menu) = self.menu() {
                    let rows = menu.items.len() as isize;
                    if rows > 0 {
                        self.highlight = (self.highlight as isize + by).rem_euclid(rows) as usize;
                        effects.push(AgentEffect::NotifyDrawer);
                    }
                }
            }
            AgentIntent::StepProvider(by) => {
                if let Some(menu) = self.menu() {
                    let at = menu
                        .rail
                        .iter()
                        .position(|p| Some(&p.id) == menu.provider.as_ref())
                        .unwrap_or(0) as isize;
                    let rows = menu.rail.len() as isize;
                    if rows > 0 {
                        self.provider =
                            Some(menu.rail[(at + by).rem_euclid(rows) as usize].id.clone());
                        self.reconcile_ui(&mut effects);
                        effects.push(AgentEffect::NotifyDrawer);
                    }
                }
            }
            AgentIntent::SelectProvider(provider) => {
                self.provider = Some(provider);
                self.reconcile_ui(&mut effects);
                effects.push(AgentEffect::Focus(FocusTarget::Composer));
                effects.push(AgentEffect::NotifyDrawer);
            }
            AgentIntent::Dismiss => {
                if self.confirm_delete {
                    self.apply_action(ThreadAction::CancelDelete, &mut effects);
                } else if self.project_form_open() {
                    self.apply_action(ThreadAction::CancelMakeProject, &mut effects);
                } else if self.menu().is_some() {
                    self.dismissed = Some(self.current_draft.clone());
                    self.reconcile_ui(&mut effects);
                    effects.push(AgentEffect::NotifyDrawer);
                }
            }
            AgentIntent::StopTurn => {
                self.stop_turn(&mut effects);
            }
            AgentIntent::AnswerRequest { request, answer } => {
                self.answer_request(request, answer, &mut effects);
            }
            AgentIntent::AnswerWaiting(place) => {
                self.answer_waiting(place, &mut effects);
            }
            AgentIntent::Action(action) => {
                self.apply_action(action, &mut effects);
            }
            AgentIntent::ModelPickerSignIn => {
                self.set_composer_text("", &mut effects);
                if !self.apply_action(ThreadAction::SignIn, &mut effects) {
                    self.reconcile_ui(&mut effects);
                    effects.push(AgentEffect::SyncAgentView);
                    effects.push(AgentEffect::NotifyDrawer);
                }
            }
            AgentIntent::ModelPickerFreeModels(enabled) => {
                self.set_composer_text("", &mut effects);
                if !self.apply_action(ThreadAction::ChooseFreeModels(enabled), &mut effects) {
                    self.reconcile_ui(&mut effects);
                    effects.push(AgentEffect::SyncAgentView);
                    effects.push(AgentEffect::NotifyDrawer);
                }
            }
            AgentIntent::SignInLaunchResult(result) => {
                if let Err(error) = result {
                    self.state.sign_in_launched = false;
                    self.state.error = Some(error);
                    effects.push(AgentEffect::SyncAgentView);
                    if self.drawer_up {
                        effects.push(AgentEffect::NotifyDrawer);
                    }
                }
            }
        }
        effects
    }

    fn set_composer_text(&mut self, text: impl Into<String>, effects: &mut Vec<AgentEffect>) {
        let text = text.into();
        self.current_draft = text.clone();
        if self.dismissed.as_deref().is_some_and(|closed| closed != text) {
            self.dismissed = None;
        }
        if !self.state.list_view {
            let key = draft_key(&self.state);
            if text.trim().is_empty() || slash::is_drawer_command(&text) {
                self.drafts.remove(&key);
            } else {
                self.drafts.insert(key, text.clone());
            }
        }
        if let Some(prev) = effects
            .iter_mut()
            .rev()
            .find(|e| matches!(e, AgentEffect::SetComposerText(_)))
        {
            *prev = AgentEffect::SetComposerText(text);
        } else {
            effects.push(AgentEffect::SetComposerText(text));
        }
    }

    fn on_dirs_listed(&mut self, listing: DirListing) -> bool {
        self.project.browse_loading = false;
        self.project.browse_home = Some(listing.home);
        if self.project.making && self.project.browse_open {
            self.project.browse_dir = Some(listing.path);
            self.project.browse_parent = listing.parent;
            self.project.browse_entries = listing.entries;
            self.project.error = None;
            true
        } else {
            false
        }
    }

    fn on_agent_error(&mut self) -> bool {
        if self.project.browse_loading {
            self.project.browse_loading = false;
            if self.project.browse_dir.is_none() {
                self.project.browse_open = false;
            }
            true
        } else {
            false
        }
    }

    /// Reconciles onboarding focus, post-`/model` `/effort ` autofill,
    /// `catalog_epoch` fetch-guard invalidation, menu close resets, highlight
    /// initialization, and on-demand Lever catalog fetches (`models`, `efforts`,
    /// `commands`). Previously lived inside `AgentSurface::render`.
    fn reconcile_ui(&mut self, effects: &mut Vec<AgentEffect>) {
        let needs_choice = self.state.needs_model_choice();
        if self.was_choosing && !needs_choice {
            self.was_choosing = false;
            if !self.state.list_view {
                effects.push(AgentEffect::Focus(FocusTarget::Composer));
            }
        } else if !self.was_choosing && needs_choice {
            self.was_choosing = true;
            effects.push(AgentEffect::Focus(FocusTarget::Surface));
        }

        // The Model just picked is at hand, and Lever has said its effort
        // levels: they are offered next, in the message box that is empty.
        if let Some(model) = self.effort_after.clone() {
            if self.state.error.is_some() {
                self.effort_after = None;
            } else if self.state.shown_model().as_deref() == Some(model.as_str())
                && let Some(levels) = self.state.efforts.get(&model)
            {
                let has_levels = !levels.is_empty();
                self.effort_after = None;
                if has_levels && self.current_draft.is_empty() && !self.state.mode_locked() {
                    self.set_composer_text("/effort ", effects);
                }
            }
        }

        // Reset cached fetch guards whenever Lever connects, disconnects, or
        // changes free hosted Models / harness availability.
        if self.seen_catalog_epoch != self.state.catalog_epoch {
            self.seen_catalog_epoch = self.state.catalog_epoch;
            self.asked_offer = false;
            self.asked_efforts.clear();
            self.asked_commands = None;
        }

        let menu = self.menu();
        let menu_kind = menu.as_ref().map(|m| m.kind);
        if self.last_menu_kind != menu_kind {
            if self.last_menu_kind == Some(MenuKind::Models) {
                self.provider = None;
            }
            if should_reset_offer_on_menu_close(self.last_menu_kind, &self.state) {
                self.asked_offer = false;
            }
            if self.last_menu_kind == Some(MenuKind::Efforts) {
                self.asked_efforts
                    .retain(|m| self.state.efforts.contains_key(m));
            }
            if self.last_menu_kind == Some(MenuKind::Commands)
                && self.state.harness_commands().is_empty()
            {
                self.asked_commands = None;
            }
            self.last_menu_kind = menu_kind;
        }

        // The list starts on the choice the thread is at each time it is a
        // list of something else, or once its choices arrive from Lever.
        let list_for = menu
            .as_ref()
            .map(|menu| list_for_key(menu, &self.current_draft))
            .unwrap_or_default();
        if list_for != self.list_for {
            self.highlight = menu.as_ref().map(Menu::start).unwrap_or(0);
            self.list_for = list_for;
        }

        // A harness that is running says what it offers without being
        // started for it, so the thread's Model shows by its name.
        if self.state.configured
            && !self.asked_offer
            && self.state.connected
            && self.state.harness_started
            && !self.state.offer_known
        {
            self.asked_offer = true;
            if self.state.ask_offer() {
                effects.push(AgentEffect::SendLeverCommand(AgentCommand::FetchModes {
                    refresh: false,
                }));
            }
        }

        // What the open list needs of Lever is asked for once.
        if let Some(menu) = &menu
            && self.state.configured
        {
            match &menu.wants {
                Some(Wants::Offer) if !self.asked_offer => {
                    self.asked_offer = true;
                    if self.state.ask_offer() {
                        effects.push(AgentEffect::SendLeverCommand(AgentCommand::FetchModes {
                            refresh: false,
                        }));
                    }
                }
                Some(Wants::Efforts(model)) if self.asked_efforts.insert(model.clone()) => {
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::FetchEfforts {
                        model: model.clone(),
                    }));
                }
                _ => {}
            }
            if menu.kind == MenuKind::Commands
                && self.state.connected
                && !self.state.no_harness
                && self.asked_commands.as_ref() != Some(&self.state.thread)
            {
                self.asked_commands = Some(self.state.thread.clone());
                effects.push(AgentEffect::SendLeverCommand(AgentCommand::FetchCommands {
                    thread: self.state.thread.clone(),
                }));
            }
        }

        let list_open = menu.is_some();
        let rail_open = menu.as_ref().is_some_and(|m| !m.rail.is_empty());
        effects.push(AgentEffect::SyncComposerMenu {
            list_open,
            rail_open,
        });
    }

    /// Applies a `ThreadAction`. Returns `true` when it ran reconciliation and
    /// drawer notification, or `false` if it returned early without doing so.
    fn apply_action(&mut self, action: ThreadAction, effects: &mut Vec<AgentEffect>) -> bool {
        let allowed_while_busy = matches!(
            action,
            ThreadAction::Act(_)
                | ThreadAction::ActWhole(_)
                | ThreadAction::Context(_)
                | ThreadAction::Acts(_)
                | ThreadAction::Subagent(_)
                | ThreadAction::SignIn
                | ThreadAction::ChooseFreeModels(_)
                | ThreadAction::RefreshModels
                | ThreadAction::Unqueue(_)
                | ThreadAction::CancelMakeProject
        );
        if (self.state.sending || self.state.loading) && !allowed_while_busy {
            return false;
        }
        match action {
            ThreadAction::List => {
                self.state.list_view = true;
                self.confirm_delete = false;
                self.project.close();
                effects.push(AgentEffect::ResetScroll);
                effects.push(AgentEffect::Focus(FocusTarget::Surface));
            }
            ThreadAction::New | ThreadAction::Open(_) => {
                if !self.state.connected {
                    return false;
                }
                self.confirm_delete = false;
                self.project.close();
                let thread = match action {
                    ThreadAction::Open(t) => Some(t),
                    _ => None,
                };
                if self.state.select_thread(thread.clone()) && self.state.configured {
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::Select {
                        thread,
                    }));
                }
                let draft = self
                    .drafts
                    .get(&draft_key(&self.state))
                    .cloned()
                    .unwrap_or_default();
                self.set_composer_text(draft, effects);
                self.expanded_acts.clear();
                self.whole_acts.clear();
                self.expanded_context.clear();
                self.opened_turns.clear();
                self.expanded_subagents.clear();
                self.asked_commands = None;
                self.provider = None;
                self.effort_after = None;
                effects.push(AgentEffect::ResetScroll);
                effects.push(AgentEffect::ScrollToBottom);
                effects.push(AgentEffect::Focus(FocusTarget::Composer));
            }
            ThreadAction::AskMakeProject => {
                let can_open = self.state.thread.is_some()
                    && self.state.connected
                    && !self.state.mode_locked();
                if !can_open {
                    return false;
                }
                self.confirm_delete = false;
                self.project.close();
                self.project.making = true;
                self.project.place = Some("~/Projects".into());
                self.project.name.clear();
                self.project.folder = "~/Projects".into();
                self.state.error = None;
                effects.push(AgentEffect::SetProjectNameText(String::new()));
                effects.push(AgentEffect::SetProjectFolderText("~/Projects".into()));
                effects.push(AgentEffect::Focus(FocusTarget::ProjectName));
            }
            ThreadAction::CancelMakeProject => {
                self.project.close();
                self.state.error = None;
                effects.push(AgentEffect::Focus(FocusTarget::Composer));
            }
            ThreadAction::PickProjectPlace(place) => {
                let next = join_project_place(&place, &self.project.name);
                self.project.place = Some(place);
                self.project.browse_open = false;
                self.project.browse_loading = false;
                self.project.error = None;
                self.state.error = None;
                self.project.folder = next.clone();
                effects.push(AgentEffect::SetProjectFolderText(next));
                if self.project.name.trim().is_empty() {
                    effects.push(AgentEffect::Focus(FocusTarget::ProjectName));
                } else {
                    effects.push(AgentEffect::Focus(FocusTarget::ProjectFolder));
                }
            }
            ThreadAction::ToggleProjectBrowse => {
                if self.project.browse_open {
                    self.project.browse_open = false;
                    self.project.browse_loading = false;
                } else if self.state.configured {
                    self.project.error = None;
                    self.project.browse_open = true;
                    self.project.browse_loading = true;
                    let start = browse_start_path(&self.project.folder);
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::ListDirs {
                        path: start,
                    }));
                } else {
                    self.project.error = Some("The Agent is not reachable right now.".into());
                }
            }
            ThreadAction::BrowseProjectDir(dir) => {
                if self.state.configured {
                    self.project.error = None;
                    self.project.browse_open = true;
                    self.project.browse_loading = true;
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::ListDirs {
                        path: Some(dir),
                    }));
                }
            }
            ThreadAction::UseProjectFolder(folder) => {
                self.project.place = None;
                self.project.browse_open = false;
                self.project.browse_loading = false;
                self.project.error = None;
                self.state.error = None;
                self.project.folder = folder.clone();
                effects.push(AgentEffect::SetProjectFolderText(folder));
                effects.push(AgentEffect::Focus(FocusTarget::ProjectFolder));
            }
            ThreadAction::ConfirmMakeProject => {
                let validated = validate_project_form(
                    &self.project.name,
                    &self.project.folder,
                    self.project.place.as_deref(),
                );
                let (req_name, req_place, req_path) = match validated {
                    Ok(triple) => triple,
                    Err(why) => {
                        self.project.error = Some(why);
                        effects.push(AgentEffect::SyncAgentView);
                        effects.push(AgentEffect::NotifyDrawer);
                        return true;
                    }
                };
                self.project.error = None;
                if self.state.connected
                    && !self.state.mode_locked()
                    && self.state.configured
                    && let Some(thread) = self.state.thread.clone()
                {
                    self.state.loading = true;
                    self.state.error = None;
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::MakeProject {
                        thread,
                        name: req_name,
                        place: req_place,
                        path: req_path,
                    }));
                }
            }
            ThreadAction::AskDelete => {
                self.project.close();
                self.confirm_delete = true;
                effects.push(AgentEffect::Focus(FocusTarget::Surface));
            }
            ThreadAction::CancelDelete => {
                self.confirm_delete = false;
                effects.push(AgentEffect::Focus(FocusTarget::Composer));
            }
            ThreadAction::Archive | ThreadAction::Delete => {
                let delete = matches!(action, ThreadAction::Delete);
                if !(delete && self.state.can_stop())
                    && self.state.configured
                    && let Some(thread) = self.state.thread.clone()
                {
                    self.drafts.remove(&thread);
                    self.state.loading = true;
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::Lifecycle {
                        thread,
                        delete,
                    }));
                }
                self.confirm_delete = false;
            }
            ThreadAction::Unqueue(id) => {
                if self.state.configured
                    && let Some(thread) = self.state.thread.clone()
                {
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::Unqueue {
                        thread,
                        queued: id,
                    }));
                }
            }
            ThreadAction::Act(id) => {
                if !self.expanded_acts.remove(&id) {
                    self.expanded_acts.insert(id);
                }
            }
            ThreadAction::ActWhole(id) => {
                if !self.whole_acts.remove(&id) {
                    self.whole_acts.insert(id);
                }
            }
            ThreadAction::Context(turn) => {
                if !self.expanded_context.remove(&turn) {
                    self.expanded_context.insert(turn);
                }
            }
            ThreadAction::Acts(turn) => {
                if !self.opened_turns.remove(&turn) {
                    self.opened_turns.insert(turn);
                }
            }
            ThreadAction::Subagent(id) => {
                if !self.expanded_subagents.remove(&id) {
                    self.expanded_subagents.insert(id);
                }
            }
            ThreadAction::ChooseFreeModels(enabled) => {
                if self.state.connected && !self.state.no_harness && self.state.configured {
                    self.state.error = None;
                    if enabled {
                        self.state.sign_in_launched = false;
                    }
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::SetFreeModels {
                        enabled,
                    }));
                }
            }
            ThreadAction::SignIn => {
                if self.state.connected && !self.state.no_harness {
                    self.state.error = None;
                    self.state.sign_in_launched = true;
                    effects.push(AgentEffect::AgentKeyboard(false));
                    effects.push(AgentEffect::LaunchSignIn);
                }
            }
            ThreadAction::RefreshModels => {
                if self.state.connected && !self.state.no_harness && self.state.configured {
                    self.state.error = None;
                    self.state.modes_loading = true;
                    self.state.modes_error = None;
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::FetchModes {
                        refresh: true,
                    }));
                }
            }
        }
        self.reconcile_ui(effects);
        effects.push(AgentEffect::SyncAgentView);
        effects.push(AgentEffect::NotifyDrawer);
        true
    }

    fn refuse(&mut self, why: String, effects: &mut Vec<AgentEffect>) {
        self.said_of = Some(self.current_draft.clone());
        self.state.error = Some(why);
        effects.push(AgentEffect::SyncAgentView);
        effects.push(AgentEffect::NotifyDrawer);
    }

    fn take(&mut self, menu: &Menu, effects: &mut Vec<AgentEffect>) {
        let Some(item) = menu
            .items
            .get(self.highlight.min(menu.items.len().saturating_sub(1)))
        else {
            return;
        };
        match item.take.clone() {
            Take::Fill(text) => {
                self.state.error = None;
                self.set_composer_text(text, effects);
                self.reconcile_ui(effects);
                effects.push(AgentEffect::NotifyDrawer);
            }
            Take::Run(run) => self.run(run, effects),
            Take::Refuse(why) => self.refuse(why, effects),
        }
    }

    fn run(&mut self, run: Run, effects: &mut Vec<AgentEffect>) {
        match run {
            Run::PickModel(model) => {
                let pick = self.state.pick_model(&model);
                if pick == SettingPick::Refused {
                    return self.refuse(
                        "The Agent is working on this thread. /stop ends the turn.".into(),
                        effects,
                    );
                }
                effects.push(AgentEffect::SavePicks {
                    model: self.state.last_model.clone(),
                    effort: self.state.last_effort.clone(),
                });
                let known = self.state.efforts.contains_key(&model);
                self.set_composer_text("", effects);
                self.state.error = None;
                self.provider = None;
                if self.state.configured {
                    if let SettingPick::SetThread { thread, value } = pick {
                        effects.push(AgentEffect::SendLeverCommand(AgentCommand::SetModel {
                            thread,
                            model: value,
                        }));
                    }
                    if !known && self.asked_efforts.insert(model.clone()) {
                        effects.push(AgentEffect::SendLeverCommand(AgentCommand::FetchEfforts {
                            model: model.clone(),
                        }));
                    }
                }
                self.effort_after = Some(model);
            }
            Run::PickEffort(effort) => {
                let pick = self.state.pick_effort(&effort);
                if pick == SettingPick::Refused {
                    return self.refuse(
                        "The Agent is working on this thread. /stop ends the turn.".into(),
                        effects,
                    );
                }
                effects.push(AgentEffect::SavePicks {
                    model: self.state.last_model.clone(),
                    effort: self.state.last_effort.clone(),
                });
                self.set_composer_text("", effects);
                self.state.error = None;
                if self.state.configured
                    && let SettingPick::SetThread { thread, value } = pick
                {
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::SetEffort {
                        thread,
                        effort: Some(value),
                    }));
                }
            }
            Run::PickMode(mode) => {
                let pick = self.state.pick_mode(&mode);
                if pick == ModePick::Refused {
                    return self.refuse(
                        "The Agent is working on this thread. /stop ends the turn.".into(),
                        effects,
                    );
                }
                self.set_composer_text("", effects);
                self.state.error = None;
                if self.state.configured
                    && let ModePick::SetThread { thread, mode } = pick
                {
                    effects.push(AgentEffect::SendLeverCommand(AgentCommand::SetMode {
                        thread,
                        mode,
                    }));
                }
            }
            Run::PickDefaultMode(mode) => {
                self.set_composer_text("", effects);
                self.state.error = None;
                if self.state.configured {
                    effects.push(AgentEffect::SendLeverCommand(
                        AgentCommand::SetDefaultMode { mode },
                    ));
                }
            }
            Run::SetFreeModels(enabled) => {
                self.set_composer_text("", effects);
                if self.apply_action(ThreadAction::ChooseFreeModels(enabled), effects) {
                    return;
                }
            }
            Run::SignIn => {
                self.set_composer_text("", effects);
                if self.apply_action(ThreadAction::SignIn, effects) {
                    return;
                }
            }
            Run::New => {
                self.set_composer_text("", effects);
                self.state.error = None;
                if self.apply_action(ThreadAction::New, effects) {
                    return;
                }
            }
            Run::Threads => {
                self.set_composer_text("", effects);
                self.state.error = None;
                if self.apply_action(ThreadAction::List, effects) {
                    return;
                }
            }
            Run::MakeProject => {
                self.set_composer_text("", effects);
                self.state.error = None;
                if self.apply_action(ThreadAction::AskMakeProject, effects) {
                    return;
                }
            }
            Run::Stop => {
                self.set_composer_text("", effects);
                self.state.error = None;
                self.stop_turn(effects);
            }
            Run::Archive => {
                self.set_composer_text("", effects);
                self.state.error = None;
                if self.apply_action(ThreadAction::Archive, effects) {
                    return;
                }
            }
            Run::Delete => {
                self.set_composer_text("", effects);
                self.state.error = None;
                if self.apply_action(ThreadAction::AskDelete, effects) {
                    return;
                }
            }
        }
        self.reconcile_ui(effects);
        effects.push(AgentEffect::SyncAgentView);
        effects.push(AgentEffect::NotifyDrawer);
    }

    fn enter(&mut self, effects: &mut Vec<AgentEffect>) {
        if self.state.needs_model_choice()
            || self.state.list_view
            || self.confirm_delete
            || self.project_form_open()
        {
            return;
        }
        if let Some(menu) = self.menu()
            && !menu.items.is_empty()
        {
            return self.take(&menu, effects);
        }
        let resolved = slash::resolve(self.current_draft.trim_end(), &self.state);
        match resolved {
            Resolve::Send => self.send(effects),
            Resolve::Run(run) => self.run(run, effects),
            Resolve::Fill(text) => {
                self.set_composer_text(text, effects);
                self.reconcile_ui(effects);
                effects.push(AgentEffect::NotifyDrawer);
            }
            Resolve::Refuse(why) => self.refuse(why, effects),
        }
    }

    fn send(&mut self, effects: &mut Vec<AgentEffect>) {
        let query = self.current_draft.clone();
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.state.needs_model_choice() {
            return;
        }
        if self.state.no_models_available() {
            self.state.missing_model = Some(
                "No Model is available. Turn on OpenCode's free hosted Models or sign in to a provider."
                    .into(),
            );
            effects.push(AgentEffect::SyncAgentView);
            effects.push(AgentEffect::NotifyDrawer);
            return;
        }
        if !self.state.can_send() || !self.state.configured {
            return;
        }
        self.state.sending = true;
        self.state.error = None;
        let thread = self.state.thread.clone();
        let (mode, model, effort) = if thread.is_none() {
            (
                self.state.pending_mode.clone(),
                self.state.new_thread_model(),
                self.state.new_thread_effort(),
            )
        } else {
            (None, None, None)
        };
        let text = trimmed.to_string();
        let key = thread.clone().unwrap_or_default();
        self.submitted = Some((key, query));
        self.reconcile_ui(effects);
        effects.push(AgentEffect::SyncAgentView);
        effects.push(AgentEffect::SendLeverCommand(AgentCommand::Send {
            thread,
            text,
            mode,
            model,
            effort,
        }));
        effects.push(AgentEffect::NotifyDrawer);
    }

    fn answer_request(&mut self, request: String, answer: String, effects: &mut Vec<AgentEffect>) {
        if let Some(thread) = self.state.begin_answer(&request, &answer)
            && self.state.configured
        {
            effects.push(AgentEffect::LogRequestAnswer(answer.clone()));
            effects.push(AgentEffect::SendLeverCommand(AgentCommand::Answer {
                thread,
                request,
                answer,
            }));
        }
        effects.push(AgentEffect::SyncAgentView);
        effects.push(AgentEffect::NotifyDrawer);
    }

    fn answer_waiting(&mut self, place: usize, effects: &mut Vec<AgentEffect>) {
        if self.state.list_view || self.confirm_delete || self.project_form_open() {
            return;
        }
        let found = self.state.turns.iter().find_map(|turn| {
            turn.requests
                .iter()
                .find(|req| request_view(&turn.state, req) == RequestView::Waiting)
                .and_then(|req| {
                    req.answers
                        .get(place)
                        .map(|a| (req.id.clone(), a.answer.clone()))
                })
        });
        if let Some((request, answer)) = found {
            self.answer_request(request, answer, effects);
        }
    }

    fn stop_turn(&self, effects: &mut Vec<AgentEffect>) {
        if self.state.configured
            && let Some(thread) = self.state.thread.clone()
        {
            effects.push(AgentEffect::SendLeverCommand(AgentCommand::Stop {
                thread,
                turn: self.state.current_turn(),
            }));
        }
    }
}

/// The key a thread's unsent words are kept under: its ID, or nothing for
/// the thread that has not begun.
pub fn draft_key(state: &AgentState) -> String {
    state.thread.clone().unwrap_or_default()
}

/// Reconciles `drafts` when Lever acknowledges `Sent` for
/// `(submitted_key, submitted_text)`.
pub fn reconcile_draft_on_sent(
    drafts: &mut HashMap<String, String>,
    submitted_key: &str,
    new_thread_key: &str,
    submitted_text: &str,
    open_input: Option<&str>,
) -> Option<String> {
    let store = |drafts: &mut HashMap<String, String>, key: &str, text: &str| {
        if text.trim().is_empty() || slash::is_drawer_command(text) {
            drafts.remove(key);
        } else {
            drafts.insert(key.to_string(), text.to_string());
        }
    };
    match open_input {
        Some(current) => {
            if let Some(next) = draft_after_send(current, submitted_text) {
                drafts.remove(submitted_key);
                store(drafts, new_thread_key, &next);
                Some(next)
            } else {
                if submitted_key != new_thread_key {
                    drafts.remove(submitted_key);
                }
                store(drafts, new_thread_key, current);
                None
            }
        }
        None => {
            let stored = drafts.get(submitted_key).cloned();
            if let Some(stored) = stored {
                if let Some(next) = draft_after_send(&stored, submitted_text) {
                    drafts.remove(submitted_key);
                    store(drafts, new_thread_key, &next);
                } else if submitted_key != new_thread_key {
                    drafts.remove(submitted_key);
                    store(drafts, new_thread_key, &stored);
                }
            }
            None
        }
    }
}

/// Whether closing `closed` should reset `asked_offer`.
pub fn should_reset_offer_on_menu_close(closed: Option<MenuKind>, state: &AgentState) -> bool {
    if closed == Some(MenuKind::Models) {
        return true;
    }
    matches!(
        closed,
        Some(MenuKind::Modes | MenuKind::DefaultModes | MenuKind::Efforts)
    ) && (state.modes_error.is_some() || state.models.is_empty() || state.modes.is_empty())
}

/// Identity key for the open slash menu used to reset highlight to `menu.start()`.
pub fn list_for_key(menu: &Menu, draft: &str) -> String {
    let named = match slash::typed(draft) {
        slash::Typed::Name(query) => query.to_string(),
        slash::Typed::Command { arg, .. } => arg.to_string(),
        slash::Typed::Words => String::new(),
    };
    format!(
        "{:?}|{named}|{:?}|{}",
        menu.kind,
        menu.provider,
        menu.items.is_empty()
    )
}

/// Joins a base place (`~/Projects`, `~/Code`, `/home/crowbar`, etc.) with a project name when given.
pub fn join_project_place(place: &str, name: &str) -> String {
    let base = place.trim_start().trim_end_matches('/');
    let base = if base.trim().is_empty() { "/" } else { base };
    let trimmed = name.trim();
    if trimmed.is_empty() {
        base.to_string()
    } else if base == "/" {
        format!("/{trimmed}")
    } else {
        format!("{base}/{trimmed}")
    }
}

/// Normalizes a folder input for `list-dirs` browsing while preserving trailing spaces.
pub fn browse_start_path(folder: &str) -> Option<String> {
    (!folder.trim().is_empty()).then(|| folder.trim_start().to_string())
}

/// Validated `(name, place, path)` arguments for Lever's `make-project` command.
pub type ProjectFormResult = Result<(Option<String>, Option<String>, Option<String>), String>;

/// Validates the inline "Make this a project" form and returns `(name, place, path)` for Lever.
pub fn validate_project_form(
    name: &str,
    folder: &str,
    place: Option<&str>,
) -> ProjectFormResult {
    let name = name.trim();
    if let Some(place) = place {
        if name.is_empty() {
            return Err("Enter a project name.".into());
        }
        if name == "."
            || name == ".."
            || name.contains('/')
            || name.contains('\\')
            || name.chars().any(char::is_control)
        {
            return Err("Project name must be a single folder name.".into());
        }
        let place_arg = (place != "~/Projects").then(|| place.to_string());
        return Ok((Some(name.to_string()), place_arg, None));
    }
    let folder = folder.trim_start();
    if folder.trim().is_empty() {
        return Err("Enter or choose a folder.".into());
    }
    if folder.chars().any(char::is_control) {
        return Err("Folder path contains an invalid character.".into());
    }
    if !folder.starts_with('/') && folder.trim() != "~" && !folder.starts_with("~/") {
        return Err("Enter an absolute folder path or start with ~/.".into());
    }
    Ok((None, None, Some(folder.to_string())))
}

/// Formats an absolute path using `~` only when Lever has supplied the person's
/// actual home directory.
pub fn display_folder_path(path: &str, known_home: Option<&str>) -> String {
    if let Some(home) = known_home
        .map(|h| h.trim_end_matches('/'))
        .filter(|h| !h.is_empty())
    {
        if path == home {
            return "~".into();
        }
        if let Some(rest) = path.strip_prefix(home).and_then(|r| r.strip_prefix('/')) {
            return format!("~/{rest}");
        }
    }
    path.to_string()
}

/// Quick parent places offered in the "Make this a project" form.
pub fn place_suggestions(state: &AgentState, known_home: Option<&str>) -> Vec<PlaceSuggestion> {
    let mut places = vec![
        PlaceSuggestion {
            label: "~/Projects".into(),
            value: "~/Projects".into(),
        },
        PlaceSuggestion {
            label: "~/Code".into(),
            value: "~/Code".into(),
        },
        PlaceSuggestion {
            label: "~".into(),
            value: "~".into(),
        },
    ];
    for thread in &state.threads {
        if thread.archived || thread.cwd.is_empty() {
            continue;
        }
        let Some(parent) = Path::new(&thread.cwd).parent().and_then(|p| p.to_str()) else {
            continue;
        };
        if parent == "/" || parent.is_empty() {
            continue;
        }
        let label = display_folder_path(parent, known_home);
        if !places.iter().any(|p| p.label == label || p.value == parent) {
            places.push(PlaceSuggestion {
                label,
                value: parent.to_string(),
            });
        }
        if places.len() >= 6 {
            break;
        }
    }
    places
}
