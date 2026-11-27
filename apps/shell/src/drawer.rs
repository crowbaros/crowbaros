//! The Agent's surface (ADR 0035): a drawer on the right of the display the
//! pointer was on at the summon, the display's full height, above every
//! window and taking no room from any. The summon gesture or its close
//! control puts it away; Escape does not. Putting the drawer away stops
//! nothing: a working turn finishes behind it and the mark shows `done`.
//!
//! The person drives a thread from the message box. A `/` there lists the
//! drawer's commands and the harness's ([`slash`]), and a command that
//! needs a choice, as `/model` does, lists the choices in the same place.
//! An answer shows as the Markdown it is ([`markdown`]), and a turn's acts
//! fold into one line once it has ended.

use super::*;
use crate::composer::{self, Composer};
use crate::markdown;
use crate::slash::{self, Menu, MenuKind, Resolve, Run, Take, Wants};
use std::collections::HashSet;

/// The most rows of a list shown at once. A longer list moves with the keys.
const LIST_ROWS: usize = 10;

#[derive(Clone)]
pub(crate) enum ThreadAction {
    List,
    New,
    Open(String),
    Archive,
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
}

pub(crate) struct AgentSurface {
    pub(crate) focus: FocusHandle,
    confirm_delete: bool,
    expanded_acts: HashSet<String>,
    /// The acts shown whole, where what they gave is long.
    whole_acts: HashSet<String>,
    expanded_context: HashSet<u64>,
    /// The turns whose acts the person opened, once they had folded.
    opened_turns: HashSet<u64>,
    /// Subagents the person opened in the thread to read their messages and acts.
    expanded_subagents: HashSet<String>,
    pub(crate) model: Entity<Model>,
    pub(crate) input: Entity<Composer>,
    pub(crate) scroll: ScrollHandle,
    pub(crate) scroll_to_bottom: bool,
    _input_sub: Subscription,
    /// The display the drawer is on, and where its left edge is on it.
    output: String,
    left: f32,
    frame: Rc<RefCell<ControlFrame>>,
    /// The row of the open list the keys are on, and the list it is a row of.
    highlight: usize,
    list_for: String,
    /// The last message-box text seen by the observer, so focus/caret/layout
    /// notifications do not clear unrelated errors.
    last_text: String,
    /// What the message box held when Escape closed its list.
    dismissed: Option<String>,
    /// What the message box held when the drawer last said why something
    /// was not done.
    said_of: Option<String>,
    /// The provider picked in the rail beside the list of Models.
    provider: Option<String>,
    /// The Model just picked, whose effort levels are offered next.
    effort_after: Option<String>,
    /// The catalog epoch last seen from `AgentState`, so disconnect/error/
    /// free-model changes reset cached fetch guards.
    seen_catalog_epoch: u64,
    /// The kind of list open on the previous render, to reset transient state
    /// when a picker closes.
    last_menu_kind: Option<MenuKind>,
    /// What the drawer has asked Lever for since it came up.
    asked_offer: bool,
    asked_efforts: HashSet<String>,
    asked_commands: Option<Option<String>>,
    /// Tracks whether the previous frame showed the first-summon choice so
    /// focus moves to the composer as soon as a Model source is chosen.
    was_choosing: bool,
    /// Wakes the drawer each second while a turn works, for how long it has.
    _tick: Task<()>,
}

/// Plain Enter in the focused message box counts once per physical press;
/// held key repeats and modified Enters (like Shift+Enter) are not taken.
fn should_take_composer_enter(is_held: bool, modifiers: &Modifiers, input_focused: bool) -> bool {
    input_focused
        && !is_held
        && !modifiers.shift
        && !modifiers.control
        && !modifiers.alt
        && !modifiers.platform
}

/// Alt+digit answers a waiting permission request only on an initial key press
/// while viewing the thread (never on key repeat, in the thread list, or over
/// the delete confirmation).
fn should_answer_with_alt_digit(is_held: bool, list_view: bool, confirm_delete: bool) -> bool {
    !is_held && !list_view && !confirm_delete
}

/// Reconciles `agent_drafts` when Lever acknowledges `Sent` for
/// `(submitted_key, submitted_text)`. Works whether the drawer is open
/// (`open_input == Some(current)`) or was put away before `Sent` arrived
/// (`open_input == None`). Returns `Some(next)` when an open composer should
/// replace its text with `next`, preserving any post-send edits or typed-ahead
/// suffix.
pub(crate) fn reconcile_draft_on_sent(
    drafts: &mut std::collections::HashMap<String, String>,
    submitted_key: &str,
    new_thread_key: &str,
    submitted_text: &str,
    open_input: Option<&str>,
) -> Option<String> {
    let store = |drafts: &mut std::collections::HashMap<String, String>, key: &str, text: &str| {
        if text.trim().is_empty() || slash::is_drawer_command(text) {
            drafts.remove(key);
        } else {
            drafts.insert(key.to_string(), text.to_string());
        }
    };
    match open_input {
        Some(current) => {
            if let Some(next) = agent::draft_after_send(current, submitted_text) {
                drafts.remove(submitted_key);
                store(drafts, new_thread_key, &next);
                Some(next)
            } else {
                // Person replaced the message box with unrelated edits before
                // `Sent` arrived: keep those edits under the active thread key.
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
                if let Some(next) = agent::draft_after_send(&stored, submitted_text) {
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

/// Whether closing `closed` should reset `asked_offer` so reopening retries
/// a failed or empty catalog lookup, or refreshes `/model` against Lever's
/// cached catalog after an out-of-band sign-in.
fn should_reset_offer_on_menu_close(closed: Option<MenuKind>, state: &agent::AgentState) -> bool {
    if closed == Some(MenuKind::Models) {
        return true;
    }
    matches!(
        closed,
        Some(MenuKind::Modes | MenuKind::DefaultModes | MenuKind::Efforts)
    ) && (state.modes_error.is_some() || state.models.is_empty() || state.modes.is_empty())
}

/// Identity key for the open slash menu used to reset highlight to `menu.start()`
/// when the list switches kind, query, provider, or finishes loading its rows.
fn list_for_key(menu: &Menu, draft: &str) -> String {
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

/// The key a thread's unsent words are kept under: its ID, or nothing for
/// the thread that has not begun.
fn draft_key(state: &agent::AgentState) -> String {
    state.thread.clone().unwrap_or_default()
}

impl AgentSurface {
    pub(crate) fn new(
        model: Entity<Model>,
        output: String,
        left: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| Composer::new("Ask the Agent. / for commands", 16384, cx));
        let focus = cx.focus_handle();
        let state = &model.read(cx).agent_state;
        let scroll_to_bottom = !state.turns.is_empty();
        let seen_catalog_epoch = state.catalog_epoch;
        // What the person typed in this thread and did not send.
        let draft = model.read(cx).agent_drafts.get(&draft_key(state)).cloned();
        let last_text = draft.clone().unwrap_or_default();
        let was_choosing = state.needs_model_choice();
        if state.list_view || was_choosing {
            window.focus(&focus);
        } else {
            window.focus(&input.focus_handle(cx));
        }
        if let Some(draft) = draft {
            input.update(cx, |input, cx| input.set_text(draft, cx));
        }
        let input_sub = cx.observe(&input, |this, input, cx| {
            let text = input.read(cx).text();
            let text_changed = text != this.last_text;
            this.last_text = text.clone();
            if this.dismissed.as_deref().is_some_and(|closed| closed != text) {
                this.dismissed = None;
            }
            this.model.update(cx, |m, _| {
                if m.agent_state.list_view {
                    return;
                }
                // What the drawer said of the last thing typed is said: it
                // goes once the person edits the text, never on focus/layout.
                if text_changed && this.said_of.as_deref() != Some(text.as_str()) {
                    this.said_of = None;
                    m.agent_state.error = None;
                }
                let key = draft_key(&m.agent_state);
                // Keep plain words, Unix paths, and harness commands as drafts;
                // only empty text and drawer-owned commands are not kept.
                if text.trim().is_empty() || slash::is_drawer_command(&text) {
                    m.agent_drafts.remove(&key);
                } else {
                    m.agent_drafts.insert(key, text);
                }
            });
            cx.notify();
        });
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |this, cx| {
                    if this.model.read(cx).agent_state.working() {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        AgentSurface {
            focus,
            confirm_delete: false,
            expanded_acts: Default::default(),
            whole_acts: Default::default(),
            expanded_context: Default::default(),
            opened_turns: Default::default(),
            expanded_subagents: Default::default(),
            model,
            input,
            scroll: ScrollHandle::new(),
            scroll_to_bottom,
            _input_sub: input_sub,
            output,
            left,
            frame: Rc::new(RefCell::new(ControlFrame::default())),
            highlight: 0,
            list_for: String::new(),
            last_text,
            dismissed: None,
            said_of: None,
            provider: None,
            effort_after: None,
            seen_catalog_epoch,
            last_menu_kind: None,
            asked_offer: false,
            asked_efforts: Default::default(),
            asked_commands: None,
            was_choosing,
            _tick: tick,
        }
    }

    fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_text(text, cx));
    }

    pub(crate) fn action(&mut self, action: ThreadAction, window: &mut Window, cx: &mut Context<Self>) {
        // Folding/expanding thread details and Model-source controls remain
        // usable even while a history read or send is in flight.
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
        );
        let busy = {
            let s = &self.model.read(cx).agent_state;
            s.sending || s.loading
        };
        if busy && !allowed_while_busy {
            return;
        }
        match action {
            ThreadAction::List => {
                self.scroll_to_bottom = false;
                self.scroll.set_offset(point(px(0.), px(0.)));
                self.model.update(cx, |m, _| m.agent_state.list_view = true);
                self.confirm_delete = false;
                window.focus(&self.focus);
            }
            ThreadAction::New | ThreadAction::Open(_) => {
                if !self.model.read(cx).agent_state.connected {
                    return;
                }
                self.confirm_delete = false;
                let thread = match action {
                    ThreadAction::Open(t) => Some(t),
                    _ => None,
                };
                let draft = self.model.update(cx, |m, _| {
                    if m.agent_state.select_thread(thread.clone())
                        && let Some(tx) = &m.agent_tx
                    {
                        tx.select(thread);
                    }
                    m.agent_drafts
                        .get(&draft_key(&m.agent_state))
                        .cloned()
                        .unwrap_or_default()
                });
                self.set_text(draft, cx);
                self.expanded_acts.clear();
                self.whole_acts.clear();
                self.expanded_context.clear();
                self.opened_turns.clear();
                self.expanded_subagents.clear();
                self.asked_commands = None;
                self.provider = None;
                self.effort_after = None;
                self.scroll.set_offset(point(px(0.), px(0.)));
                self.scroll_to_bottom = true;
                window.focus(&self.input.focus_handle(cx));
            }
            ThreadAction::AskDelete => {
                self.confirm_delete = true;
                window.focus(&self.focus);
            }
            ThreadAction::CancelDelete => {
                self.confirm_delete = false;
                window.focus(&self.input.focus_handle(cx));
            }
            ThreadAction::Archive | ThreadAction::Delete => {
                let delete = matches!(action, ThreadAction::Delete);
                self.model.update(cx, |m, _| {
                    if delete && m.agent_state.can_stop() {
                        return;
                    }
                    if let (Some(thread), Some(tx)) = (m.agent_state.thread.clone(), &m.agent_tx) {
                        m.agent_drafts.remove(&thread);
                        m.agent_state.loading = true;
                        tx.lifecycle(thread, delete);
                    }
                });
                self.confirm_delete = false;
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
                let tx = self.model.update(cx, |m, _| {
                    if !m.agent_state.connected || m.agent_state.no_harness {
                        return None;
                    }
                    m.agent_state.error = None;
                    if enabled {
                        m.agent_state.sign_in_launched = false;
                    }
                    m.agent_tx.clone()
                });
                if let Some(tx) = tx {
                    tx.set_free_models(enabled);
                }
            }
            ThreadAction::SignIn => {
                let tx = self.model.update(cx, |m, _| {
                    if !m.agent_state.connected || m.agent_state.no_harness {
                        return None;
                    }
                    m.agent_state.error = None;
                    m.agent_state.sign_in_launched = true;
                    Some(m.tx.clone())
                });
                if let Some(tx) = tx {
                    session::agent_keyboard(false);
                    session::launch_agent_sign_in(tx);
                }
            }
            ThreadAction::RefreshModels => {
                let tx = self.model.update(cx, |m, _| {
                    if !m.agent_state.connected || m.agent_state.no_harness {
                        return None;
                    }
                    m.agent_state.error = None;
                    m.agent_state.modes_loading = true;
                    m.agent_state.modes_error = None;
                    m.agent_tx.clone()
                });
                if let Some(tx) = tx {
                    tx.refresh_models();
                }
            }
        }
        sync_agent_view(&self.model, cx);
        cx.notify();
    }

    /// A bordered control of the list of threads and of the question before
    /// a thread is deleted.
    fn button(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        action: ThreadAction,
        enabled: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let click = action.clone();
        let control = match &action {
            ThreadAction::New => "new-thread".to_string(),
            ThreadAction::List => "threads".into(),
            ThreadAction::Open(thread) => format!("thread:{thread}"),
            ThreadAction::Archive => "archive".into(),
            ThreadAction::AskDelete => "delete".into(),
            ThreadAction::Delete => "confirm-delete".into(),
            ThreadAction::CancelDelete => "cancel-delete".into(),
            ThreadAction::ChooseFreeModels(true) => "choose-free-models".into(),
            ThreadAction::ChooseFreeModels(false) => "disable-free-models".into(),
            ThreadAction::SignIn => "sign-in".into(),
            ThreadAction::RefreshModels => "refresh-models".into(),
            ThreadAction::Act(id) => format!("act:{id}"),
            ThreadAction::ActWhole(id) => format!("act-whole:{id}"),
            ThreadAction::Context(turn) => format!("context:{turn}"),
            ThreadAction::Acts(turn) => format!("acts:{turn}"),
            ThreadAction::Subagent(id) => format!("subagent:{id}"),
        };
        div()
            .id(id)
            .relative()
            .tab_index(0)
            .px(px(10.))
            .py(px(7.))
            .rounded(px(6.))
            .border_1()
            .border_color(rgb(p.line))
            .text_size(px(11.5))
            .text_color(rgb(p.text))
            .focus(move |s| s.border_color(rgb(p.focus)).bg(rgb(p.surface)))
            .when(!enabled, |s| s.opacity(0.5))
            .when(enabled, |s| s.cursor_pointer().hover(move |s| s.bg(rgb(p.surface))))
            .child(label.into())
            .child(tracked(&self.frame, control, true))
            .on_click(cx.listener(move |this, _, w, cx| {
                if enabled {
                    this.action(click.clone(), w, cx);
                }
            }))
            .on_action(cx.listener(move |this, _: &SendMessage, w, cx| {
                cx.stop_propagation();
                if enabled {
                    this.action(action.clone(), w, cx);
                }
            }))
    }

    /// A quiet line of the thread that opens or folds something: what it
    /// says, and a mark of which way it is.
    fn toggle(
        &self,
        label: String,
        open: bool,
        action: ThreadAction,
        color: u32,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let control = match &action {
            ThreadAction::Context(turn) => format!("context:{turn}"),
            ThreadAction::Acts(turn) => format!("acts:{turn}"),
            ThreadAction::Act(id) => format!("act:{id}"),
            ThreadAction::ActWhole(id) => format!("act-whole:{id}"),
            ThreadAction::Subagent(id) => format!("subagent:{id}"),
            _ => String::new(),
        };
        div()
            .id(SharedString::from(control.clone()))
            .relative()
            .flex()
            .items_center()
            .gap(px(6.))
            .py(px(2.))
            .text_size(px(11.))
            .text_color(rgb(color))
            .cursor_pointer()
            .hover(move |s| s.text_color(rgb(p.text)))
            .child(
                div()
                    .w(px(8.))
                    .font_family("IBM Plex Mono")
                    .text_size(px(10.))
                    .child(if open { "▾" } else { "▸" }),
            )
            .child(div().flex_1().min_w_0().text_ellipsis().child(label))
            .child(tracked(&self.frame, control, true))
            .on_click(cx.listener(move |this, _, w, cx| {
                this.action(action.clone(), w, cx);
                w.focus(&this.input.focus_handle(cx));
            }))
    }

    pub(crate) fn near_bottom(&self) -> bool {
        self.scroll.max_offset().height + self.scroll.offset().y <= px(2.)
    }

    /// Puts the surface away. The keyboard goes back to the window that had
    /// it, if the surface holds it. A working turn keeps going behind it.
    pub(crate) fn put_away(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        log::info!("SHELL agent-surface up=false");
        let prev_mark = self.model.read(cx).agent_state.mark;
        let cleared = self.model.update(cx, |m, _| {
            m.agent = None;
            m.agent_state.on_drawer_closed()
        });
        if cleared {
            log::info!(
                "SHELL agent-mark state={} prev={}",
                self.model.read(cx).agent_state.mark.label(),
                prev_mark.label()
            );
        }
        sync_mark_overlay(&self.model, cx);
        session::agent_keyboard(false);
        window.remove_window();
        cx.refresh_windows();
    }

    /// The list the message box calls for, unless Escape closed it.
    fn list(&self, cx: &App) -> Option<Menu> {
        let m = self.model.read(cx);
        if m.agent_state.needs_model_choice() || m.agent_state.list_view || self.confirm_delete {
            return None;
        }
        let text = self.input.read(cx).text();
        if self.dismissed.as_deref() == Some(text.as_str()) {
            return None;
        }
        slash::menu(&text, &m.agent_state, self.provider.as_deref())
    }

    /// Says why something was not done, where the drawer says such things.
    fn refuse(&mut self, why: String, cx: &mut Context<Self>) {
        self.said_of = Some(self.input.read(cx).text());
        self.model.update(cx, |m, _| m.agent_state.error = Some(why));
        sync_agent_view(&self.model, cx);
        cx.notify();
    }

    /// Takes the row of the open list the keys are on.
    fn take(&mut self, menu: &Menu, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = menu.items.get(self.highlight.min(menu.items.len().saturating_sub(1))) else {
            return;
        };
        match item.take.clone() {
            Take::Fill(text) => {
                self.model.update(cx, |m, _| m.agent_state.error = None);
                self.set_text(text, cx);
            }
            Take::Run(run) => self.run(run, window, cx),
            Take::Refuse(why) => self.refuse(why, cx),
        }
    }

    /// Carries out a command of the drawer's own.
    fn run(&mut self, run: Run, window: &mut Window, cx: &mut Context<Self>) {
        match run {
            Run::PickModel(model) => {
                let (tx, pick, known) = self.model.update(cx, |m, _| {
                    let pick = m.agent_state.pick_model(&model);
                    if pick != agent::SettingPick::Refused {
                        agent::save_picks(
                            m.agent_state.last_model.as_deref(),
                            m.agent_state.last_effort.as_deref(),
                        );
                    }
                    (
                        m.agent_tx.clone(),
                        pick,
                        m.agent_state.efforts.contains_key(&model),
                    )
                });
                if pick == agent::SettingPick::Refused {
                    return self.refuse("The Agent is working on this thread. /stop ends the turn.".into(), cx);
                }
                self.set_text("", cx);
                self.model.update(cx, |m, _| m.agent_state.error = None);
                self.provider = None;
                if let Some(tx) = tx {
                    if let agent::SettingPick::SetThread { thread, value } = pick {
                        tx.set_model(thread, value);
                    }
                    if !known && self.asked_efforts.insert(model.clone()) {
                        tx.fetch_efforts(model.clone());
                    }
                }
                // The Model's effort levels are offered next, once Lever has
                // said them and the thread is at the Model.
                self.effort_after = Some(model);
            }
            Run::PickEffort(effort) => {
                let (tx, pick) = self.model.update(cx, |m, _| {
                    let pick = m.agent_state.pick_effort(&effort);
                    if pick != agent::SettingPick::Refused {
                        agent::save_picks(
                            m.agent_state.last_model.as_deref(),
                            m.agent_state.last_effort.as_deref(),
                        );
                    }
                    (m.agent_tx.clone(), pick)
                });
                if pick == agent::SettingPick::Refused {
                    return self.refuse("The Agent is working on this thread. /stop ends the turn.".into(), cx);
                }
                self.set_text("", cx);
                self.model.update(cx, |m, _| m.agent_state.error = None);
                if let (Some(tx), agent::SettingPick::SetThread { thread, value }) = (tx, pick) {
                    tx.set_effort(thread, Some(value));
                }
            }
            Run::PickMode(mode) => {
                let (tx, pick) = self
                    .model
                    .update(cx, |m, _| (m.agent_tx.clone(), m.agent_state.pick_mode(&mode)));
                if pick == agent::ModePick::Refused {
                    return self.refuse("The Agent is working on this thread. /stop ends the turn.".into(), cx);
                }
                self.set_text("", cx);
                self.model.update(cx, |m, _| m.agent_state.error = None);
                if let (Some(tx), agent::ModePick::SetThread { thread, mode }) = (tx, pick) {
                    tx.set_mode(thread, mode);
                }
            }
            Run::PickDefaultMode(mode) => {
                self.set_text("", cx);
                self.model.update(cx, |m, _| m.agent_state.error = None);
                if let Some(tx) = self.model.read(cx).agent_tx.clone() {
                    tx.set_default_mode(mode);
                }
            }
            Run::SetFreeModels(enabled) => {
                self.set_text("", cx);
                self.action(ThreadAction::ChooseFreeModels(enabled), window, cx);
                return;
            }
            Run::SignIn => {
                self.set_text("", cx);
                self.action(ThreadAction::SignIn, window, cx);
                return;
            }
            Run::New => {
                self.set_text("", cx);
                self.model.update(cx, |m, _| m.agent_state.error = None);
                self.action(ThreadAction::New, window, cx);
            }
            Run::Threads => {
                self.set_text("", cx);
                self.model.update(cx, |m, _| m.agent_state.error = None);
                self.action(ThreadAction::List, window, cx);
            }
            Run::Stop => {
                self.set_text("", cx);
                self.model.update(cx, |m, _| m.agent_state.error = None);
                self.stop_turn(cx);
            }
            Run::Archive => {
                self.set_text("", cx);
                self.model.update(cx, |m, _| m.agent_state.error = None);
                self.action(ThreadAction::Archive, window, cx);
            }
            Run::Delete => {
                self.set_text("", cx);
                self.model.update(cx, |m, _| m.agent_state.error = None);
                self.action(ThreadAction::AskDelete, window, cx);
            }
        }
        sync_agent_view(&self.model, cx);
        cx.notify();
    }

    /// Enter in the message box: the row of an open list is taken, a
    /// command typed whole is carried out, and anything else is sent.
    pub(crate) fn enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let state = &self.model.read(cx).agent_state;
        if state.needs_model_choice() || state.list_view || self.confirm_delete {
            return;
        }
        if let Some(menu) = self.list(cx)
            && !menu.items.is_empty()
        {
            return self.take(&menu, window, cx);
        }
        let text = self.input.read(cx).text();
        let resolved = slash::resolve(text.trim_end(), &self.model.read(cx).agent_state);
        match resolved {
            Resolve::Send => self.send(cx),
            Resolve::Run(run) => self.run(run, window, cx),
            Resolve::Fill(text) => self.set_text(text, cx),
            Resolve::Refuse(why) => self.refuse(why, cx),
        }
    }

    fn send(&mut self, cx: &mut Context<Self>) {
        enum SendPlan {
            Ignore,
            Working,
            NoModel,
            Ready {
                tx: agent::CommandSender,
                thread: Option<String>,
                mode: Option<String>,
                model: Option<String>,
                effort: Option<String>,
            },
        }
        let query = self.input.read(cx).text();
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return;
        }
        let plan = self.model.update(cx, |m, _| {
            let state = &mut m.agent_state;
            if state.needs_model_choice() {
                return SendPlan::Ignore;
            }
            if state.no_models_available() {
                state.missing_model = Some(
                    "No Model is available. Turn on OpenCode's free hosted Models or sign in to a provider."
                        .into(),
                );
                return SendPlan::NoModel;
            }
            if !state.can_send() {
                return if state.refuses_while_working() {
                    SendPlan::Working
                } else {
                    SendPlan::Ignore
                };
            }
            let Some(tx) = m.agent_tx.clone() else {
                return SendPlan::Ignore;
            };
            state.sending = true;
            state.error = None;
            let thread = state.thread.clone();
            // A new thread starts with the mode, Model, and effort picked
            // for it, or with those the person last picked.
            let (mode, model, effort) = if thread.is_none() {
                (
                    state.pending_mode.clone(),
                    state.new_thread_model(),
                    state.new_thread_effort(),
                )
            } else {
                (None, None, None)
            };
            SendPlan::Ready {
                tx,
                thread,
                mode,
                model,
                effort,
            }
        });
        match plan {
            SendPlan::Ignore => {}
            SendPlan::NoModel => {
                sync_agent_view(&self.model, cx);
                cx.notify();
            }
            // The harness cannot take a message into a turn under way.
            SendPlan::Working => {
                self.refuse(
                    "The Agent can't take a message into this turn while it works. Send it when the turn ends, or /stop the turn.".into(),
                    cx,
                );
            }
            SendPlan::Ready {
                tx,
                thread,
                mode,
                model,
                effort,
            } => {
                let message = trimmed.to_string();
                let key = thread.clone().unwrap_or_default();
                self.model.update(cx, |m, _| {
                    m.agent_submitted = Some((key, query));
                });
                self.dismissed = None;
                sync_agent_view(&self.model, cx);
                tx.send_with(thread, message, mode, model, effort);
                cx.notify();
            }
        }
    }

    fn answer_request(&mut self, request: String, answer: String, cx: &mut Context<Self>) {
        let (tx, thread) = self.model.update(cx, |m, _| {
            (m.agent_tx.clone(), m.agent_state.begin_answer(&request, &answer))
        });
        if let (Some(tx), Some(thread)) = (tx, thread) {
            log::info!("SHELL agent-request answer={answer}");
            tx.answer(thread, request, answer);
        }
        sync_agent_view(&self.model, cx);
        cx.notify();
    }

    /// Answers the request that has waited longest with the answer at
    /// `place` among those it offers, as Alt and a digit ask.
    fn answer_waiting(&mut self, place: usize, cx: &mut Context<Self>) -> bool {
        let found = {
            let state = &self.model.read(cx).agent_state;
            if state.list_view || self.confirm_delete {
                return false;
            }
            state.turns.iter().find_map(|turn| {
                turn.requests
                    .iter()
                    .find(|req| agent::request_view(&turn.state, req) == agent::RequestView::Waiting)
                    .and_then(|req| req.answers.get(place).map(|a| (req.id.clone(), a.answer.clone())))
            })
        };
        match found {
            Some((request, answer)) => {
                self.answer_request(request, answer, cx);
                true
            }
            None => false,
        }
    }

    fn stop_turn(&mut self, cx: &mut Context<Self>) {
        let (tx, thread, turn) = {
            let m = self.model.read(cx);
            (
                m.agent_tx.clone(),
                m.agent_state.thread.clone(),
                m.agent_state.current_turn(),
            )
        };
        if let (Some(tx), Some(thread)) = (tx, thread) {
            tx.stop_turn(thread, turn);
        }
    }

    /// Moves the keys' row in the open list, round its ends.
    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let Some(menu) = self.list(cx) else {
            return;
        };
        let rows = menu.items.len() as isize;
        if rows > 0 {
            self.highlight = (self.highlight as isize + by).rem_euclid(rows) as usize;
            cx.notify();
        }
    }

    /// Moves to the provider before or after in the rail of the Models.
    fn step_provider(&mut self, by: isize, cx: &mut Context<Self>) {
        let Some(menu) = self.list(cx) else {
            return;
        };
        let at = menu
            .rail
            .iter()
            .position(|p| Some(&p.id) == menu.provider.as_ref())
            .unwrap_or(0) as isize;
        let rows = menu.rail.len() as isize;
        if rows > 0 {
            self.provider = Some(menu.rail[(at + by).rem_euclid(rows) as usize].id.clone());
            cx.notify();
        }
    }

    /// A request in its turn: the exact command, or the file and the
    /// change, and one control per answer while it waits.
    fn request_card(
        &self,
        turn_state: &agent::TurnState,
        req: &agent::Request,
        subagent_title: Option<&str>,
        hinted: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let view = agent::request_view(turn_state, req);
        let waiting = view == agent::RequestView::Waiting;
        let (status, status_color) = match &view {
            agent::RequestView::Waiting => ("Waiting for you".to_string(), p.focus),
            agent::RequestView::Sending(name) => (format!("Sending: {name}"), p.muted),
            agent::RequestView::Answered(name) => (format!("Answered: {name}"), p.muted),
            agent::RequestView::Withdrawn => ("Withdrawn".to_string(), p.muted),
        };
        let kind = match req.kind.as_str() {
            "command" => "Run a command",
            "edit" => "Edit a file",
            "delete" => "Delete a file",
            "move" => "Move a file",
            "fetch" => "Fetch",
            "read" => "Read a file",
            _ => "Act",
        };
        let header = match (req.subagent.as_deref(), subagent_title.filter(|s| !s.trim().is_empty())) {
            (Some(_), Some(sub)) => format!("Subagent · {sub} · {kind}"),
            (Some(_), None) => format!("Subagent · {kind}"),
            (None, _) => format!("Request · {kind}"),
        };
        let mut card = div()
            .w_full()
            .px(px(10.))
            .py(px(8.))
            .rounded(px(6.))
            .bg(rgb(p.surface))
            .border_1()
            .border_color(rgb(if waiting { p.accent } else { p.line }))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(p.muted))
                            .text_ellipsis()
                            .child(header),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(10.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(status_color))
                            .child(status),
                    ),
            );
        let title = req.title.trim();
        if !title.is_empty() && title != req.detail.trim() && Some(title) != req.path.as_deref() {
            card = card.child(
                div()
                    .text_size(px(12.))
                    .line_height(px(17.))
                    .text_color(rgb(p.text))
                    .child(title.to_string()),
            );
        }
        if let Some(path) = &req.path {
            card = card.child(
                div()
                    .font_family("IBM Plex Mono")
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .text_color(rgb(p.text))
                    .child(path.clone()),
            );
        }
        let lines = agent::request_detail_lines(&req.kind, &req.detail);
        if !lines.is_empty() {
            let mut detail = div()
                .w_full()
                .px(px(8.))
                .py(px(6.))
                .rounded(px(4.))
                .bg(rgb(p.canvas))
                .border_1()
                .border_color(rgb(p.line))
                .flex()
                .flex_col()
                .font_family("IBM Plex Mono")
                .text_size(px(11.))
                .line_height(px(16.));
            for (tone, line) in lines {
                let row = div()
                    .w_full()
                    .child(if line.is_empty() { " ".to_string() } else { line });
                detail = detail.child(match tone {
                    agent::DetailLine::Plain => row.text_color(rgb(p.text)),
                    agent::DetailLine::Added => row.text_color(rgb(p.focus)),
                    agent::DetailLine::Removed => row.text_color(rgb(p.muted)).line_through(),
                });
            }
            card = card.child(detail);
        }
        if waiting {
            let mut row = div().w_full().flex().flex_wrap().gap(px(6.));
            for (i, choice) in req.answers.iter().enumerate() {
                let id = format!("answer:{}:{}", req.id, choice.answer);
                let primary = i == 0;
                let request = req.id.clone();
                let answer = choice.answer.clone();
                row = row.child(
                    div()
                        .id(SharedString::from(id.clone()))
                        .relative()
                        .h(px(26.))
                        .px(px(10.))
                        .rounded(px(6.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .gap(px(6.))
                        .cursor_pointer()
                        .text_size(px(11.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(primary, |b| b.bg(rgb(p.accent)).text_color(rgb(p.on_accent)))
                        .when(!primary, |b| {
                            b.bg(rgb(p.canvas))
                                .border_1()
                                .border_color(rgb(p.line))
                                .text_color(rgb(p.text))
                        })
                        .child(choice.label())
                        // The keys that answer the request that has waited longest.
                        .when(hinted && i < 9, |b| {
                            b.child(
                                div()
                                    .font_family("IBM Plex Mono")
                                    .text_size(px(10.))
                                    .font_weight(FontWeight::NORMAL)
                                    .opacity(0.75)
                                    .child(format!("Alt+{}", i + 1)),
                            )
                        })
                        .child(tracked(&self.frame, id, true))
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.answer_request(request.clone(), answer.clone(), cx);
                            w.focus(&this.input.focus_handle(cx));
                        })),
                );
            }
            card = card.child(row);
        }
        card
    }

    /// One act of a turn, as a line: what kind it is, what it was, and how
    /// it stands. A click shows what it gave.
    fn act_row(&self, turn_state: &agent::TurnState, act: &agent::Act, p: Palette, cx: &mut Context<Self>) -> Div {
        let display = agent::act_display_status(turn_state, act);
        let state_color = match display.tone {
            agent::ActTone::Active => p.focus,
            agent::ActTone::Failed => p.text,
            agent::ActTone::Muted => p.muted,
        };
        let detail = act.detail.as_deref().map(str::trim).filter(|d| !d.is_empty());
        let open = self.expanded_acts.contains(&act.id);
        let id = format!("act:{}", act.id);
        let toggle = ThreadAction::Act(act.id.clone());
        let mut row = div().w_full().flex().flex_col().gap(px(3.)).child(
            div()
                .id(SharedString::from(id.clone()))
                .relative()
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .py(px(1.))
                .when(detail.is_some(), |r| {
                    r.cursor_pointer()
                        .child(tracked(&self.frame, id, true))
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.action(toggle.clone(), w, cx);
                            w.focus(&this.input.focus_handle(cx));
                        }))
                })
                .child(
                    div()
                        .flex_none()
                        .w(px(52.))
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(p.muted))
                        .child(act.kind_label()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .font_family("IBM Plex Mono")
                        .text_size(px(11.))
                        .text_color(rgb(p.text))
                        .text_ellipsis()
                        .child(act.title.clone()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(state_color))
                        .child(display.label),
                ),
        );
        if let (Some(detail), true) = (detail, open) {
            // What an act gave can be long: its first lines show, and the
            // rest when asked for.
            let first = agent::format_act_detail(detail);
            let long = first != detail;
            let whole = self.whole_acts.contains(&act.id);
            row = row.child(
                div()
                    .w_full()
                    .px(px(8.))
                    .py(px(6.))
                    .rounded(px(4.))
                    .bg(rgb(p.surface))
                    .font_family("IBM Plex Mono")
                    .text_size(px(10.5))
                    .line_height(px(15.))
                    .text_color(rgb(p.muted))
                    .child(if whole { detail.to_string() } else { first }),
            );
            if long {
                row = row.child(self.toggle(
                    if whole { "All of it".into() } else { "Show all of it".into() },
                    whole,
                    ThreadAction::ActWhole(act.id.clone()),
                    p.muted,
                    p,
                    cx,
                ));
            }
        }
        row
    }

    /// One subagent of a turn: a line the person clicks to open its prompt,
    /// acts, and messages. Requests it makes stay in the thread so they are
    /// always visible and answerable.
    fn subagent_section(
        &self,
        turn: &agent::Turn,
        sub: &agent::Subagent,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.expanded_subagents.contains(&sub.id);
        let control = format!("subagent:{}", sub.id);
        let action = ThreadAction::Subagent(sub.id.clone());
        let has_waiting = turn.requests.iter().any(|r| {
            r.subagent.as_deref() == Some(sub.id.as_str())
                && matches!(
                    agent::request_view(&turn.state, r),
                    agent::RequestView::Waiting | agent::RequestView::Sending(_)
                )
        });
        let (status, status_color) = if has_waiting {
            ("Waiting", p.focus)
        } else {
            match &sub.state {
                agent::SubagentState::Working => ("Working…", p.focus),
                agent::SubagentState::Failed(_) => ("Failed", p.text),
                other => (other.label(), p.muted),
            }
        };
        let act_count = match sub.acts.len() {
            0 => None,
            1 => Some("1 act".to_string()),
            n => Some(format!("{n} acts")),
        };
        let mut section = div().w_full().flex().flex_col().gap(px(6.)).child(
            div()
                .id(SharedString::from(control.clone()))
                .relative()
                .w_full()
                .px(px(10.))
                .py(px(7.))
                .rounded(px(6.))
                .bg(rgb(p.surface))
                .border_1()
                .border_color(rgb(if has_waiting || sub.state.is_working() {
                    p.accent
                } else {
                    p.line
                }))
                .flex()
                .items_center()
                .gap(px(8.))
                .cursor_pointer()
                .hover(move |s| s.border_color(rgb(p.focus)))
                .child(
                    div()
                        .flex_none()
                        .w(px(8.))
                        .font_family("IBM Plex Mono")
                        .text_size(px(10.))
                        .text_color(rgb(p.muted))
                        .child(if open { "▾" } else { "▸" }),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(p.muted))
                        .child("Subagent"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(11.5))
                        .font_weight(if open {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(rgb(p.text))
                        .text_ellipsis()
                        .child(sub.title.clone()),
                )
                .when_some(act_count, |row, count| {
                    row.child(
                        div()
                            .flex_none()
                            .text_size(px(10.))
                            .text_color(rgb(p.muted))
                            .child(count),
                    )
                })
                .child(
                    div()
                        .flex_none()
                        .text_size(px(10.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(status_color))
                        .child(status),
                )
                .child(tracked(&self.frame, control, true))
                .on_click(cx.listener(move |this, _, w, cx| {
                    this.action(action.clone(), w, cx);
                    w.focus(&this.input.focus_handle(cx));
                })),
        );
        if open {
            let mut inner = div()
                .w_full()
                .pl(px(12.))
                .py(px(2.))
                .border_l_2()
                .border_color(rgb(p.line))
                .flex()
                .flex_col()
                .gap(px(8.));
            let sub_turn_state = sub.state.as_turn_state();
            for msg in sub.messages.iter().filter(|m| m.role == "prompt") {
                if !msg.text.trim().is_empty() {
                    inner = inner.child(
                        div()
                            .w_full()
                            .px(px(8.))
                            .py(px(6.))
                            .rounded(px(6.))
                            .bg(rgb(p.surface))
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(p.muted))
                                    .child("Prompt"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .line_height(px(16.5))
                                    .text_color(rgb(p.muted))
                                    .child(msg.text.clone()),
                            ),
                    );
                }
            }
            if !sub.acts.is_empty() {
                let mut acts_col = div().w_full().flex().flex_col().gap(px(3.));
                for act in &sub.acts {
                    acts_col = acts_col.child(self.act_row(&sub_turn_state, act, p, cx));
                }
                inner = inner.child(acts_col);
            }
            for (idx, msg) in sub.messages.iter().filter(|m| m.role != "prompt").enumerate() {
                if !msg.text.trim().is_empty() {
                    inner = inner.child(answer_blocks(&format!("sub-{}-{idx}", sub.id), &msg.text, p));
                }
            }
            if let agent::SubagentState::Failed(err) = &sub.state {
                inner = inner.child(
                    div()
                        .px(px(10.))
                        .py(px(8.))
                        .rounded(px(6.))
                        .bg(rgb(p.surface))
                        .border_1()
                        .border_color(rgb(p.line))
                        .text_size(px(11.5))
                        .line_height(px(16.5))
                        .text_color(rgb(p.text))
                        .child(err.clone()),
                );
            }
            if sub.messages.is_empty()
                && sub.acts.is_empty()
                && !matches!(sub.state, agent::SubagentState::Failed(_))
            {
                inner = inner.child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(p.muted))
                        .child(if sub.state.is_working() {
                            "Working…"
                        } else {
                            "No messages recorded."
                        }),
                );
            }
            section = section.child(inner);
        }
        section
    }

    /// The list above the message box: the commands, or the choices of one.
    fn list_panel(&self, menu: &Menu, p: Palette, cx: &mut Context<Self>) -> Div {
        let highlight = self.highlight.min(menu.items.len().saturating_sub(1));
        // A long list shows the rows around the one the keys are on.
        let first = highlight.saturating_sub(LIST_ROWS - 1).min(menu.items.len().saturating_sub(LIST_ROWS));
        let commands = menu.kind == MenuKind::Commands;
        let mut rows = div().flex_1().min_w_0().flex().flex_col();
        for (at, item) in menu.items.iter().enumerate().skip(first).take(LIST_ROWS) {
            let on = at == highlight;
            let refused = item.refusal.is_some();
            rows = rows.child(
                div()
                    .id(SharedString::from(item.id.clone()))
                    .relative()
                    .w_full()
                    .h(px(28.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(6.))
                    .border_l_2()
                    .border_color(rgb(if on { p.focus } else { p.surface }))
                    .when(on, |r| r.bg(rgb(p.canvas)))
                    .when(refused, |r| r.opacity(0.55))
                    .cursor_pointer()
                    .child(
                        div()
                            .flex_none()
                            .when(commands, |d| d.font_family("IBM Plex Mono").text_size(px(11.5)))
                            .when(!commands, |d| d.text_size(px(12.5)))
                            .text_color(rgb(p.text))
                            .child(item.label.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.))
                            .text_color(rgb(p.muted))
                            .text_ellipsis()
                            .child(item.refusal.clone().unwrap_or_else(|| item.detail.clone())),
                    )
                    .when_some(
                        item.tag.clone().or_else(|| item.current.then(|| "Current".to_string())),
                        |r, tag| {
                            r.child(
                                div()
                                    .flex_none()
                                    .text_size(px(10.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(if item.current { p.focus } else { p.muted }))
                                    .child(tag),
                            )
                        },
                    )
                    .child(tracked(&self.frame, item.id.clone(), false))
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.highlight != at {
                            this.highlight = at;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.highlight = at;
                        if let Some(menu) = this.list(cx) {
                            this.take(&menu, w, cx);
                        }
                        w.focus(&this.input.focus_handle(cx));
                    })),
            );
        }
        let hidden = menu.items.len().saturating_sub(first + LIST_ROWS);
        if first > 0 || hidden > 0 {
            rows = rows.child(
                div()
                    .px(px(10.))
                    .pt(px(2.))
                    .text_size(px(10.))
                    .text_color(rgb(p.muted))
                    .child(format!("{} of {}", highlight + 1, menu.items.len())),
            );
        }
        if let Some(note) = &menu.note {
            rows = rows.child(
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .text_size(px(11.5))
                    .line_height(px(16.))
                    .text_color(rgb(p.muted))
                    .child(note.clone()),
            );
        }
        let mut body = div().w_full().flex().gap(px(6.));
        if !menu.rail.is_empty() {
            let mut rail = div()
                .flex_none()
                .w(px(108.))
                .pr(px(6.))
                .border_r_1()
                .border_color(rgb(p.line))
                .flex()
                .flex_col();
            for provider in &menu.rail {
                let on = Some(&provider.id) == menu.provider.as_ref();
                let id = format!("provider:{}", provider.id);
                let pick = provider.id.clone();
                rail = rail.child(
                    div()
                        .id(SharedString::from(id.clone()))
                        .relative()
                        .w_full()
                        .h(px(28.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .text_size(px(11.5))
                        .when(on, |r| {
                            r.bg(rgb(p.canvas))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(p.text))
                        })
                        .when(!on, |r| r.text_color(rgb(p.muted)).hover(move |s| s.text_color(rgb(p.text))))
                        .child(div().w_full().text_ellipsis().child(provider.name.clone()))
                        .child(tracked(&self.frame, id, false))
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.provider = Some(pick.clone());
                            w.focus(&this.input.focus_handle(cx));
                            cx.notify();
                        })),
                );
            }
            body = body.child(rail);
        }
        body = body.child(rows);

        let keys = match (menu.kind, menu.rail.is_empty()) {
            (MenuKind::Commands, _) => "↑↓ move   Enter take   Esc close",
            (_, false) => "↑↓ move   ←→ provider   Enter pick   Esc close",
            _ => "↑↓ move   Enter pick   Esc close",
        };
        let free_on = self.model.read(cx).agent_state.free_hosted_models;
        div()
            .w_full()
            .p(px(6.))
            .rounded(px(8.))
            .bg(rgb(p.surface))
            .border_1()
            .border_color(rgb(p.line))
            .flex()
            .flex_col()
            .gap(px(4.))
            .when_some(menu.title.clone(), |d, title| {
                d.child(
                    div()
                        .px(px(8.))
                        .pt(px(2.))
                        .text_size(px(10.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(p.muted))
                        .child(title),
                )
            })
            .child(body)
            .when(menu.kind == MenuKind::Models, |d| {
                let next_free = !free_on;
                d.child(
                    div()
                        .w_full()
                        .px(px(8.))
                        .pt(px(4.))
                        .border_t_1()
                        .border_color(rgb(p.line))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(8.))
                        .child(
                            div()
                                .id("model-picker-sign-in")
                                .relative()
                                .px(px(8.))
                                .py(px(4.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(rgb(p.line))
                                .bg(rgb(p.canvas))
                                .cursor_pointer()
                                .text_size(px(11.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(p.text))
                                .child("Sign in")
                                .child(tracked(&self.frame, "sign-in".into(), false))
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.set_text("", cx);
                                    this.action(ThreadAction::SignIn, w, cx);
                                })),
                        )
                        .child(
                            div()
                                .id("model-picker-free-models")
                                .relative()
                                .px(px(8.))
                                .py(px(4.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(rgb(p.line))
                                .bg(rgb(p.canvas))
                                .cursor_pointer()
                                .text_size(px(11.))
                                .text_color(rgb(p.muted))
                                .hover(move |s| s.text_color(rgb(p.text)))
                                .child(if free_on {
                                    "Free hosted Models: On"
                                } else {
                                    "Free hosted Models: Off"
                                })
                                .child(tracked(&self.frame, "toggle-free-models".into(), false))
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    this.set_text("", cx);
                                    this.action(ThreadAction::ChooseFreeModels(next_free), w, cx);
                                })),
                        ),
                )
            })
            .child(
                div()
                    .px(px(8.))
                    .pb(px(1.))
                    .font_family("IBM Plex Mono")
                    .text_size(px(10.))
                    .text_color(rgb(p.muted))
                    .child(keys),
            )
    }

    /// What the thread works with, in one quiet line under the message box.
    fn status_line(&self, state: &agent::AgentState, p: Palette) -> Div {
        let model = state.shown_model();
        let mut parts: Vec<String> = Vec::new();
        if !state.configured || !state.connected {
            parts.push("Offline".into());
        } else if state.no_harness {
            parts.push("No harness".into());
        } else {
            match &model {
                Some(model) => parts.push(state.model_label(model)),
                None => parts.extend(state.harnesses.first().cloned()),
            }
            // The effort a harness calls its default goes without saying.
            if let Some(effort) = state.shown_effort().filter(|effort| effort != "default") {
                parts.push(state.effort_label(model.as_deref(), &effort));
            }
            if let Some(mode) = state.shown_mode() {
                parts.push(agent::mode_name(&mode, &state.modes));
            }
        }
        div()
            .w_full()
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(px(11.))
            .text_color(rgb(p.muted))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_ellipsis()
                    .child(parts.join("  ·  ")),
            )
            .child(div().flex_none().child(palette_keycap("/", p)))
            .child(div().flex_none().child("commands"))
    }
}

/// A line of an answer, with each part in its look.
fn styled(id: String, line: &markdown::Line, p: Palette, weight: FontWeight) -> AnyElement {
    if line.text.is_empty() {
        return div().into_any_element();
    }
    let plain = |weight: FontWeight| {
        let mut face = font("Manrope");
        face.weight = weight;
        face
    };
    let run = |len: usize, look: Option<&markdown::Look>| -> TextRun {
        let Some(look) = look else {
            return TextRun {
                len,
                font: plain(weight),
                color: rgb(p.text).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
        };
        // Manrope comes in three weights and no slant, so words that are
        // emphasised are a weight heavier.
        let face = if look.code {
            font("IBM Plex Mono")
        } else if look.strong {
            plain(FontWeight::BOLD)
        } else if look.emphasis {
            plain(FontWeight::SEMIBOLD)
        } else {
            plain(weight)
        };
        let color: Hsla = rgb(if look.link.is_some() { p.focus } else { p.text }).into();
        TextRun {
            len,
            font: face,
            color,
            background_color: look.code.then(|| rgb(p.surface).into()),
            underline: look.link.is_some().then_some(UnderlineStyle {
                color: Some(color),
                thickness: px(1.),
                wavy: false,
            }),
            strikethrough: look.strike.then_some(StrikethroughStyle {
                color: Some(color),
                thickness: px(1.),
            }),
        }
    };
    let mut display = String::with_capacity(line.text.len());
    let mut runs = Vec::new();
    let mut link_ranges: Vec<std::ops::Range<usize>> = Vec::new();
    let mut link_urls: Vec<String> = Vec::new();
    let mut at = 0;
    for span in &line.spans {
        if span.range.start > at {
            let chunk = &line.text[at..span.range.start];
            display.push_str(chunk);
            runs.push(run(chunk.len(), None));
        }
        let start_in_display = display.len();
        let chunk = &line.text[span.range.clone()];
        display.push_str(chunk);
        runs.push(run(chunk.len(), Some(&span.look)));
        if let Some(url) = &span.look.link {
            // Show the destination beside a titled link so the person sees
            // where it goes; clicking copies the URL to the clipboard.
            if chunk != url.as_str() {
                let suffix = format!(" ({url})");
                display.push_str(&suffix);
                runs.push(TextRun {
                    len: suffix.len(),
                    font: plain(weight),
                    color: rgb(p.muted).into(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
            }
            link_ranges.push(start_in_display..display.len());
            link_urls.push(url.clone());
        }
        at = span.range.end;
    }
    if at < line.text.len() {
        let chunk = &line.text[at..];
        display.push_str(chunk);
        runs.push(run(chunk.len(), None));
    }
    let text = StyledText::new(display).with_runs(runs);
    if link_ranges.is_empty() {
        text.into_any_element()
    } else {
        InteractiveText::new(SharedString::from(id), text)
            .on_click(link_ranges, move |ix, _, cx| {
                if let Some(url) = link_urls.get(ix) {
                    cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
                }
            })
            .into_any_element()
    }
}

/// A message of the person's, set to the right.
fn message_bubble(text: String, p: Palette) -> Div {
    div().w_full().flex().justify_end().child(
        div()
            .max_w(relative(0.88))
            .px(px(12.))
            .py(px(8.))
            .rounded(px(10.))
            .bg(rgb(p.surface))
            .border_1()
            .border_color(rgb(p.line))
            .text_size(px(13.))
            .line_height(px(19.))
            .text_color(rgb(p.text))
            .child(text),
    )
}

/// An answer, laid out as the Markdown it is.
fn answer(turn: u64, text: &str, p: Palette) -> Div {
    answer_blocks(&format!("md-{turn}"), text, p)
}

fn answer_blocks(prefix: &str, text: &str, p: Palette) -> Div {
    let mut out = div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(8.))
        .text_size(px(13.))
        .line_height(px(19.5))
        .text_color(rgb(p.text));
    for (block_ix, block) in markdown::blocks(text).into_iter().enumerate() {
        let id = format!("{prefix}-{block_ix}");
        out = out.child(match block {
            markdown::Block::Paragraph(line) => div().w_full().child(styled(id, &line, p, FontWeight::NORMAL)),
            markdown::Block::Heading { level, line } => {
                let (size, weight) = match level {
                    1 => (16., FontWeight::BOLD),
                    2 => (14.5, FontWeight::SEMIBOLD),
                    _ => (13., FontWeight::SEMIBOLD),
                };
                div()
                    .w_full()
                    .pt(px(2.))
                    .text_size(px(size))
                    .line_height(px(size * 1.4))
                    .child(styled(id, &line, p, weight))
            }
            markdown::Block::Code { text, .. } => div()
                .w_full()
                .px(px(10.))
                .py(px(8.))
                .rounded(px(6.))
                .bg(rgb(p.surface))
                .border_1()
                .border_color(rgb(p.line))
                .font_family("IBM Plex Mono")
                .text_size(px(11.5))
                .line_height(px(17.))
                .child(if text.is_empty() { " ".to_string() } else { text }),
            markdown::Block::List(items) => {
                let mut list = div().w_full().flex().flex_col().gap(px(3.));
                for (item_ix, item) in items.into_iter().enumerate() {
                    let item_id = format!("{id}-{item_ix}");
                    list = list.child(
                        div()
                            .w_full()
                            .flex()
                            .items_start()
                            .gap(px(6.))
                            .pl(px(4. + item.depth as f32 * 14.))
                            .child(
                                div()
                                    .flex_none()
                                    .min_w(px(12.))
                                    .text_color(rgb(p.muted))
                                    .child(item.marker),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(styled(item_id, &item.line, p, FontWeight::NORMAL)),
                            ),
                    );
                }
                list
            }
            markdown::Block::Quote(line) => div()
                .w_full()
                .pl(px(10.))
                .border_l_2()
                .border_color(rgb(p.line))
                .child(styled(id, &line, p, FontWeight::NORMAL)),
            markdown::Block::Table(rows) => {
                let mut table = div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .font_family("IBM Plex Mono")
                    .text_size(px(11.))
                    .line_height(px(16.));
                for row in rows {
                    table = table.child(div().w_full().child(row));
                }
                table
            }
            markdown::Block::Rule => div().w_full().h(px(1.)).bg(rgb(p.line)),
        });
    }
    out
}

impl Render for AgentSurface {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.scroll_to_bottom {
            self.scroll_to_bottom = false;
            let scroll = self.scroll.clone();
            window.on_next_frame(move |window, _| {
                scroll.scroll_to_bottom();
                window.refresh();
            });
        }
        *self.frame.borrow_mut() = ControlFrame::default();
        let m = self.model.read(cx);
        let p = m.palette();
        let mark = ImageSource::Resource(Resource::Embedded(bar_mark_asset(&m.preferences).into()));
        let state = m.agent_state.clone();
        let agent_view = m.agent_view.clone();
        let tx = m.agent_tx.clone();
        let show_stop = state.can_stop();
        let needs_choice = state.needs_model_choice();
        if self.was_choosing && !needs_choice {
            self.was_choosing = false;
            if !state.list_view {
                window.focus(&self.input.focus_handle(cx));
            }
        } else if !self.was_choosing && needs_choice {
            self.was_choosing = true;
            window.focus(&self.focus);
        }
        let draft = self.input.read(cx).text();
        let now = agent::unix_now();

        // The Model just picked is at hand, and Lever has said its effort
        // levels: they are offered next, in the message box that is empty.
        if let Some(model) = self.effort_after.clone() {
            if state.error.is_some() {
                self.effort_after = None;
            } else if state.shown_model().as_deref() == Some(model.as_str())
                && let Some(levels) = state.efforts.get(&model)
            {
                self.effort_after = None;
                if !levels.is_empty() && draft.is_empty() && !state.mode_locked() {
                    self.set_text("/effort ", cx);
                }
            }
        }
        // Reset cached fetch guards whenever Lever connects, disconnects, or
        // changes free hosted Models / harness availability (not on errors,
        // which recover when a menu closes and reopens).
        if self.seen_catalog_epoch != state.catalog_epoch {
            self.seen_catalog_epoch = state.catalog_epoch;
            self.asked_offer = false;
            self.asked_efforts.clear();
            self.asked_commands = None;
        }
        let draft = self.input.read(cx).text();
        let can_send = state.can_send() && !draft.trim().is_empty();
        let menu = self.list(cx);
        let menu_kind = menu.as_ref().map(|m| m.kind);
        if self.last_menu_kind != menu_kind {
            if self.last_menu_kind == Some(MenuKind::Models) {
                self.provider = None;
            }
            if should_reset_offer_on_menu_close(self.last_menu_kind, &state) {
                self.asked_offer = false;
            }
            if self.last_menu_kind == Some(MenuKind::Efforts) {
                self.asked_efforts.retain(|m| state.efforts.contains_key(m));
            }
            if self.last_menu_kind == Some(MenuKind::Commands) && state.harness_commands().is_empty() {
                self.asked_commands = None;
            }
            self.last_menu_kind = menu_kind;
        }
        // The list starts on the choice the thread is at each time it is a
        // list of something else, or once its choices arrive from Lever.
        let list_for = menu
            .as_ref()
            .map(|menu| list_for_key(menu, &draft))
            .unwrap_or_default();
        if list_for != self.list_for {
            self.highlight = menu.as_ref().map(Menu::start).unwrap_or(0);
            self.list_for = list_for;
        }
        // A harness that is running says what it offers without being
        // started for it, so the thread's Model shows by its name.
        if let Some(tx) = &tx
            && !self.asked_offer
            && state.connected
            && state.harness_started
            && !state.offer_known
        {
            self.asked_offer = true;
            if self.model.update(cx, |m, _| m.agent_state.ask_offer()) {
                tx.fetch_modes();
            }
        }
        // What the open list needs of Lever is asked for once.
        if let (Some(menu), Some(tx)) = (&menu, &tx) {
            match &menu.wants {
                Some(Wants::Offer) if !self.asked_offer => {
                    self.asked_offer = true;
                    if self.model.update(cx, |m, _| m.agent_state.ask_offer()) {
                        tx.fetch_modes();
                    }
                }
                Some(Wants::Efforts(model)) if self.asked_efforts.insert(model.clone()) => {
                    tx.fetch_efforts(model.clone());
                }
                _ => {}
            }
            if menu.kind == MenuKind::Commands
                && state.connected
                && !state.no_harness
                && self.asked_commands.as_ref() != Some(&state.thread)
            {
                self.asked_commands = Some(state.thread.clone());
                tx.fetch_commands(state.thread.clone());
            }
        }
        let list_open = menu.is_some();
        let rail_open = menu.as_ref().is_some_and(|menu| !menu.rail.is_empty());
        self.input.update(cx, |input, _| {
            input.palette = p;
            input.list_open = list_open;
            input.rail_open = rail_open;
        });

        let header_sub = if !state.configured || !state.connected {
            "Offline".into()
        } else if state.no_harness {
            "No harness".into()
        } else if let Some(harness) = state.harnesses.first() {
            harness.clone()
        } else {
            "Ready".into()
        };

        let mut body = div()
            .id("agent-thread")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_scroll_wheel(cx.listener(|_, _, _, cx| cx.notify()))
            .px(px(16.))
            .py(px(16.))
            .flex()
            .flex_col()
            .gap(px(18.))
            .bg(rgb(p.canvas));

        if self.confirm_delete {
            body = body
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Delete this thread?"),
                )
                .child(div().line_height(px(19.)).child(
                    "Its messages, answers, and acts will be removed. Lever also asks the harness to delete its session. This cannot be undone.",
                ))
                .child(
                    div()
                        .flex()
                        .gap(px(8.))
                        .child(self.button("cancel-thread-delete", "Cancel", ThreadAction::CancelDelete, true, p, cx))
                        .child(self.button(
                            "confirm-thread-delete",
                            "Delete thread",
                            ThreadAction::Delete,
                            state.connected && !show_stop,
                            p,
                            cx,
                        )),
                );
        } else if needs_choice {
            let ready = state.connected && !state.no_harness;
            body = body
                .child(
                    div()
                        .pt(px(8.))
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(px(16.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(p.text))
                                .child("Choose where Models come from"),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(18.))
                                .text_color(rgb(p.muted))
                                .child(
                                    "Nothing leaves your machine until you choose. You can change this later from the drawer.",
                                ),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .p(px(14.))
                        .rounded(px(10.))
                        .bg(rgb(p.surface))
                        .border_1()
                        .border_color(rgb(p.line))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(p.text))
                                .child("OpenCode free hosted Models"),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(17.))
                                .text_color(rgb(p.muted))
                                .child(
                                    "Start right away with no account. Requests go to OpenCode's makers.",
                                ),
                        )
                        .child(
                            div().pt(px(2.)).flex().child(
                                self.button(
                                    "agent-choose-free-models",
                                    "Use free hosted Models",
                                    ThreadAction::ChooseFreeModels(true),
                                    ready,
                                    p,
                                    cx,
                                )
                                .bg(rgb(p.accent))
                                .border_color(rgb(p.accent))
                                .text_color(rgb(p.on_accent))
                                .font_weight(FontWeight::SEMIBOLD),
                            ),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .p(px(14.))
                        .rounded(px(10.))
                        .bg(rgb(p.surface))
                        .border_1()
                        .border_color(rgb(p.line))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(p.text))
                                .child("Sign in to a provider"),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(17.))
                                .text_color(rgb(p.muted))
                                .child(
                                    "Opens OpenCode's sign-in in a terminal. CrowbarOS keeps no credentials.",
                                ),
                        )
                        .child(
                            div().pt(px(2.)).flex().child(self.button(
                                "agent-sign-in",
                                "Sign in",
                                ThreadAction::SignIn,
                                ready,
                                p,
                                cx,
                            )),
                        ),
                )
                .when(state.sign_in_launched, |b| {
                    b.child(
                        div()
                            .text_size(px(11.5))
                            .line_height(px(17.))
                            .text_color(rgb(p.focus))
                            .child(
                                "Sign-in opened in a terminal. New Models will appear here as soon as it ends.",
                            ),
                    )
                })
                .when_some(state.modes_error.clone(), |b, err| {
                    b.child(
                        div()
                            .text_size(px(11.5))
                            .line_height(px(17.))
                            .text_color(rgb(p.muted))
                            .child(err),
                    )
                })
                .child(
                    div().flex().items_center().gap(px(8.)).child(self.button(
                        "agent-refresh-models",
                        if state.modes_loading {
                            "Checking Models…"
                        } else {
                            "Already signed in? Check Models"
                        },
                        ThreadAction::RefreshModels,
                        ready && !state.modes_loading,
                        p,
                        cx,
                    )),
                );
        } else if state.list_view {
            let free = state.connected && !state.loading && !state.sending;
            body = body.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Threads"),
                    )
                    .child(self.button("new-agent-thread", "New thread", ThreadAction::New, free, p, cx)),
            );
            let groups = state.groups();
            if groups.is_empty() {
                body = body.child(div().text_color(rgb(p.muted)).line_height(px(19.)).child(
                    if state.connected {
                        "Your threads will appear here, grouped by folder. Start a new thread in your home folder."
                    } else {
                        "The Agent is not reachable. Reconnecting automatically…"
                    },
                ));
            }
            for (folder, threads) in groups {
                let mut group = div().flex().flex_col().gap(px(5.)).child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_ellipsis()
                        .font_family("IBM Plex Mono")
                        .text_size(px(11.))
                        .text_color(rgb(p.muted))
                        .child(folder),
                );
                for thread in threads {
                    let title = if thread.title.trim().is_empty() {
                        "Untitled thread".to_string()
                    } else {
                        thread.title.clone()
                    };
                    let status = if thread.waiting {
                        "Waiting"
                    } else if thread.working {
                        "Working…"
                    } else {
                        match thread.state.as_str() {
                            "done" => "Done",
                            "stopped" => "Stopped",
                            "failed" => "Failed",
                            "interrupted" => "Interrupted",
                            _ => "Ready",
                        }
                    };
                    let status_line = match thread.subagents {
                        0 => status.to_string(),
                        1 => format!("{status}  ·  1 subagent"),
                        n => format!("{status}  ·  {n} subagents"),
                    };
                    group = group.child(
                        self.button(
                            SharedString::from(format!("thread-{}", thread.thread)),
                            "",
                            ThreadAction::Open(thread.thread),
                            free,
                            p,
                            cx,
                        )
                        .w_full()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .items_start()
                        .child(div().w_full().text_size(px(13.)).text_ellipsis().child(title))
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(rgb(if thread.working || thread.waiting {
                                    p.focus
                                } else {
                                    p.muted
                                }))
                                .child(status_line),
                        ),
                    );
                }
                body = body.child(group);
            }
        } else if state.loading && state.turns.is_empty() {
            body = body.child(div().text_color(rgb(p.muted)).child("Opening thread…"));
        } else if !state.turns.is_empty() {
            // Alt and a digit answer the request that has waited longest.
            let first_waiting = state.turns.iter().find_map(|turn| {
                turn.requests
                    .iter()
                    .find(|req| agent::request_view(&turn.state, req) == agent::RequestView::Waiting)
                    .map(|req| req.id.clone())
            });
            for (turn_index, turn) in state.turns.iter().enumerate() {
                let working = matches!(turn.state, agent::TurnState::Working);
                let mut turn_col = div().flex().flex_col().gap(px(8.));
                if turn.from_harness {
                    // A turn the harness began itself has no message: a quiet
                    // line says why it goes on.
                    turn_col = turn_col.child(
                        div()
                            .w_full()
                            .text_size(px(11.5))
                            .text_color(rgb(p.muted))
                            .child("The Agent went on after a subagent reported"),
                    );
                } else {
                    turn_col = turn_col.child(message_bubble(turn.message.clone(), p));
                }
                // The desktop context carried with the message.
                match &turn.context {
                    Some(ctx) => {
                        let open = turn_index == 0 || self.expanded_context.contains(&turn.turn);
                        let mut ctx_box = div().w_full().flex().flex_col().gap(px(2.));
                        if turn_index == 0 {
                            ctx_box = ctx_box.child(
                                div()
                                    .text_size(px(10.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(p.muted))
                                    .child("Desktop context"),
                            );
                        } else {
                            ctx_box = ctx_box.child(self.toggle(
                                "Desktop context".into(),
                                open,
                                ThreadAction::Context(turn.turn),
                                p.muted,
                                p,
                                cx,
                            ));
                        }
                        if open {
                            let mut lines = div()
                                .w_full()
                                .pl(px(10.))
                                .border_l_2()
                                .border_color(rgb(p.line))
                                .flex()
                                .flex_col();
                            for line in agent::format_context_summary(ctx) {
                                lines = lines.child(
                                    div()
                                        .font_family("IBM Plex Mono")
                                        .text_size(px(10.5))
                                        .line_height(px(15.))
                                        .text_color(rgb(p.muted))
                                        .child(line),
                                );
                            }
                            ctx_box = ctx_box.child(lines);
                        }
                        turn_col = turn_col.child(ctx_box);
                    }
                    // A turn the harness began carries no message, and so no context.
                    None if turn.from_harness => {}
                    None => {
                        turn_col = turn_col.child(
                            div()
                                .text_size(px(10.5))
                                .text_color(rgb(p.muted))
                                .child("No desktop context"),
                        );
                    }
                }
                // The turn's acts: each as it happens while the turn works,
                // and folded into one line once it has ended.
                let elapsed = turn.elapsed(now).map(|seconds| match seconds {
                    0 if !working => "under a second".to_string(),
                    seconds => agent::format_elapsed(seconds),
                });
                let acts = match turn.acts.len() {
                    0 => None,
                    1 => Some("1 act".to_string()),
                    n => Some(format!("{n} acts")),
                };
                if working {
                    let label = match &elapsed {
                        Some(elapsed) => format!("Working for {elapsed}"),
                        None => "Working…".to_string(),
                    };
                    turn_col = turn_col.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(p.focus))
                            .child(div().size(px(6.)).rounded_full().bg(rgb(p.focus)))
                            .child(label),
                    );
                } else if let Some(acts) = &acts {
                    let label = match (&turn.state, &elapsed) {
                        (agent::TurnState::Stopped, Some(elapsed)) => format!("Stopped after {elapsed}  ·  {acts}"),
                        (_, Some(elapsed)) => format!("Worked for {elapsed}  ·  {acts}"),
                        (_, None) => acts.clone(),
                    };
                    turn_col = turn_col.child(self.toggle(
                        label,
                        self.opened_turns.contains(&turn.turn),
                        ThreadAction::Acts(turn.turn),
                        p.muted,
                        p,
                        cx,
                    ));
                }
                if !turn.acts.is_empty() && (working || self.opened_turns.contains(&turn.turn)) {
                    let mut list = div()
                        .w_full()
                        .pl(px(10.))
                        .border_l_2()
                        .border_color(rgb(p.line))
                        .flex()
                        .flex_col()
                        .gap(px(3.));
                    for act in &turn.acts {
                        list = list.child(self.act_row(&turn.state, act, p, cx));
                    }
                    turn_col = turn_col.child(list);
                }
                // Subagents launched during this turn, each collapsible.
                let subagent_working = turn.subagents.iter().any(|s| s.state.is_working());
                if !working && subagent_working {
                    turn_col = turn_col.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(p.focus))
                            .child(div().size(px(6.)).rounded_full().bg(rgb(p.focus)))
                            .child("Subagent working…"),
                    );
                }
                for sub in &turn.subagents {
                    turn_col = turn_col.child(self.subagent_section(turn, sub, p, cx));
                }
                // The requests the harness (or a subagent) made in this turn.
                for req in &turn.requests {
                    let hinted = first_waiting.as_deref() == Some(req.id.as_str());
                    let sub_title = req.subagent.as_deref().and_then(|sid| {
                        turn.subagents
                            .iter()
                            .find(|s| s.id == sid)
                            .map(|s| s.title.as_str())
                    });
                    turn_col = turn_col.child(self.request_card(&turn.state, req, sub_title, hinted, p, cx));
                }
                // The notes recorded on this turn.
                for note in &turn.notes {
                    turn_col = turn_col.child(
                        div()
                            .w_full()
                            .pl(px(10.))
                            .py(px(2.))
                            .border_l_2()
                            .border_color(rgb(p.line))
                            .text_size(px(11.))
                            .line_height(px(16.))
                            .text_color(rgb(p.muted))
                            .child(note.text.clone()),
                    );
                }
                // The answer, with each message the harness took into the
                // turn where it took it, and how the turn ended when it did
                // not end done.
                for (part_ix, part) in turn.answer_parts().into_iter().enumerate() {
                    turn_col = turn_col.child(match part {
                        agent::AnswerPart::Text(text) if part_ix == 0 => answer(turn.turn, text, p),
                        agent::AnswerPart::Text(text) => {
                            answer_blocks(&format!("md-{}-{part_ix}", turn.turn), text, p)
                        }
                        agent::AnswerPart::Steered(steer) => message_bubble(steer.text.clone(), p),
                    });
                }
                let ended = match &turn.state {
                    agent::TurnState::Stopped if acts.is_none() => Some("Stopped."),
                    agent::TurnState::Interrupted => Some("Interrupted."),
                    _ => None,
                };
                if let Some(ended) = ended {
                    turn_col = turn_col.child(div().text_size(px(11.5)).text_color(rgb(p.muted)).child(ended));
                }
                if let agent::TurnState::Failed(err) = &turn.state {
                    turn_col = turn_col.child(
                        div()
                            .px(px(10.))
                            .py(px(8.))
                            .rounded(px(6.))
                            .bg(rgb(p.surface))
                            .border_1()
                            .border_color(rgb(p.line))
                            .text_size(px(12.))
                            .line_height(px(17.))
                            .text_color(rgb(p.text))
                            .child(err.clone()),
                    );
                }
                body = body.child(turn_col);
            }
        } else {
            let (title, says): (&str, String) = if !state.configured || !state.connected {
                (
                    "Agent is not reachable",
                    "The Agent isn't running right now. The drawer will reconnect automatically once it is.".into(),
                )
            } else if state.no_harness {
                (
                    "No harness available",
                    "No harness is configured to answer right now.".into(),
                )
            } else if let Some(reason) = state.missing_model.clone() {
                ("No Model available", reason)
            } else {
                (
                    "Start a thread",
                    "Each message carries your current desktop context: active window, open windows by workspace, and displays — never window contents. Type / for commands: /model picks the Model, and /mode how much the Agent asks before it acts.".into(),
                )
            };
            let show_model_actions = state.connected
                && !state.no_harness
                && (state.missing_model.is_some() || state.no_models_available());
            body = body.child(
                div()
                    .pt(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child(title))
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(rgb(p.muted))
                            .child(says),
                    )
                    .when(show_model_actions, |d| {
                        d.child(
                            div()
                                .pt(px(4.))
                                .flex()
                                .flex_wrap()
                                .gap(px(8.))
                                .child(self.button(
                                    "empty-choose-free-models",
                                    "Use free hosted Models",
                                    ThreadAction::ChooseFreeModels(true),
                                    !state.free_hosted_models,
                                    p,
                                    cx,
                                ))
                                .child(self.button(
                                    "empty-sign-in",
                                    "Sign in",
                                    ThreadAction::SignIn,
                                    true,
                                    p,
                                    cx,
                                )),
                        )
                    }),
            );
        }

        let in_thread = !needs_choice && !state.list_view && !self.confirm_delete;
        // Scrolled up from a thread's newest line: a way back to it.
        let show_newest = in_thread && !state.turns.is_empty() && !self.near_bottom();
        let error = state.error.clone().or_else(|| {
            if state.turns.is_empty() {
                None
            } else if !state.configured || !state.connected {
                Some("The Agent isn't running right now. Reconnecting automatically.".into())
            } else if state.no_harness {
                None
            } else {
                state.missing_model.clone()
            }
        });

        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context("AgentSurface")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &Next, w, _| w.focus_next()))
            .on_action(cx.listener(|_, _: &Previous, w, _| w.focus_prev()))
            // The keys the message box passes on while a list is open.
            .on_action(cx.listener(|this, _: &composer::Up, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &composer::Down, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &composer::Left, _, cx| this.step_provider(-1, cx)))
            .on_action(cx.listener(|this, _: &composer::Right, _, cx| this.step_provider(1, cx)))
            .on_action(cx.listener(|this, _: &composer::Take, w, cx| {
                if let Some(menu) = this.list(cx) {
                    this.take(&menu, w, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &composer::Dismiss, w, cx| {
                if this.confirm_delete {
                    this.action(ThreadAction::CancelDelete, w, cx);
                } else if this.list(cx).is_some() {
                    // Escape closes the list and keeps what was typed.
                    this.dismissed = Some(this.input.read(cx).text());
                    cx.notify();
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, w, cx| {
                let key = event.keystroke.key.as_str();
                let needs_choice = this.model.read(cx).agent_state.needs_model_choice();
                if needs_choice {
                    return;
                }
                if key == "enter" && should_take_composer_enter(false, &event.keystroke.modifiers, this.input.focus_handle(cx).is_focused(w)) {
                    cx.stop_propagation();
                    if should_take_composer_enter(event.is_held, &event.keystroke.modifiers, true) {
                        this.enter(w, cx);
                    }
                } else if event.keystroke.modifiers.control {
                    match key {
                        "n" => {
                            cx.stop_propagation();
                            this.action(ThreadAction::New, w, cx);
                        }
                        "l" => {
                            cx.stop_propagation();
                            this.action(ThreadAction::List, w, cx);
                        }
                        _ => {}
                    }
                } else if event.keystroke.modifiers.alt
                    && let Some(digit) = key.chars().next().and_then(|c| c.to_digit(10)).filter(|d| *d >= 1)
                {
                    let list_view = this.model.read(cx).agent_state.list_view;
                    if should_answer_with_alt_digit(event.is_held, list_view, this.confirm_delete)
                        && this.answer_waiting(digit as usize - 1, cx)
                    {
                        cx.stop_propagation();
                    }
                } else if key == "escape" && this.confirm_delete {
                    this.action(ThreadAction::CancelDelete, w, cx);
                }
            }))
            .font_family("Manrope")
            .text_size(px(13.))
            .text_color(rgb(p.text))
            .bg(rgb(p.canvas))
            .border_l_1()
            .border_color(rgb(p.line))
            .child(
                div()
                    .flex_none()
                    .h(px(AGENT_HEADER_HEIGHT))
                    .pl(px(16.))
                    .pr(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(rgb(p.line))
                    .child(img(mark).size(px(16.)).object_fit(ObjectFit::Contain))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_baseline()
                            .gap(px(8.))
                            .child(
                                div()
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Agent"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(11.))
                                    .text_color(rgb(p.muted))
                                    .text_ellipsis()
                                    .child(header_sub),
                            ),
                    )
                    .child(
                        div()
                            .id("close-agent-surface")
                            .relative()
                            .size(px(30.))
                            .rounded(px(10.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(icon("close", p.muted, 14.))
                            .child(tracked(&self.frame, "close".into(), false))
                            .on_click(cx.listener(|this, _, w, cx| this.put_away(w, cx))),
                    ),
            )
            .when(in_thread, |root| {
                // Which thread this is, and the way back to the list.
                root.child(
                    div()
                        .flex_none()
                        .h(px(36.))
                        .px(px(12.))
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .border_b_1()
                        .border_color(rgb(p.line))
                        .child(
                            div()
                                .id("agent-thread-list")
                                .relative()
                                .flex_none()
                                .h(px(24.))
                                .px(px(6.))
                                .rounded(px(6.))
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .cursor_pointer()
                                .text_size(px(11.5))
                                .text_color(rgb(p.muted))
                                .hover(move |s| s.bg(rgb(p.surface)).text_color(rgb(p.text)))
                                .child("‹")
                                .child("Threads")
                                .child(tracked(&self.frame, "threads".into(), false))
                                .on_click(cx.listener(|this, _, w, cx| this.action(ThreadAction::List, w, cx))),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_ellipsis()
                                .child(state.title.clone().unwrap_or_else(|| "New thread".into())),
                        ),
                )
            })
            .child({
                // The thread scrolls here; controls in it count as in view
                // only within these bounds.
                let frame = self.frame.clone();
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .flex()
                    .flex_col()
                    .child(body)
                    .child(
                        canvas(
                            move |bounds, _, _| frame.borrow_mut().thread_area = Some(bounds),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    .when(show_newest, |area| {
                        area.child(
                            div()
                                .absolute()
                                .bottom(px(10.))
                                .left_0()
                                .w_full()
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .id("agent-newest")
                                        .relative()
                                        .h(px(26.))
                                        .px(px(12.))
                                        .rounded(px(13.))
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .bg(rgb(p.surface))
                                        .border_1()
                                        .border_color(rgb(p.line))
                                        .cursor_pointer()
                                        .text_size(px(11.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(p.text))
                                        .child("↓")
                                        .child("Newest")
                                        .child(tracked(&self.frame, "newest".into(), false))
                                        .on_click(cx.listener(|this, _, w, cx| {
                                            this.scroll_to_bottom = true;
                                            w.focus(&this.input.focus_handle(cx));
                                            cx.notify();
                                        })),
                                ),
                        )
                    })
            })
            .when_some(error, |d, err| {
                d.child(
                    div()
                        .flex_none()
                        .px(px(16.))
                        .py(px(8.))
                        .border_t_1()
                        .border_color(rgb(p.line))
                        .bg(rgb(p.surface))
                        .text_size(px(11.5))
                        .line_height(px(16.))
                        .text_color(rgb(p.text))
                        .child(err),
                )
            })
            .when(in_thread, |root| {
                root.child(
                    div()
                        .flex_none()
                        .px(px(12.))
                        .pt(px(10.))
                        .pb(px(8.))
                        .flex()
                        .flex_col()
                        .gap(px(8.))
                        .border_t_1()
                        .border_color(rgb(p.line))
                        .when_some(menu.as_ref(), |d, menu| d.child(self.list_panel(menu, p, cx)))
                        .child(
                            div()
                                .w_full()
                                .pl(px(10.))
                                .pr(px(6.))
                                .py(px(5.))
                                .rounded(px(10.))
                                .bg(rgb(p.surface))
                                .border_1()
                                .border_color(rgb(p.line))
                                .flex()
                                .items_end()
                                .gap(px(8.))
                                .child(div().flex_1().min_w_0().child(self.input.clone()))
                                .when(show_stop, |row| {
                                    row.child(
                                        div()
                                            .id("stop-agent-turn")
                                            .relative()
                                            .px(px(8.))
                                            .h(px(26.))
                                            .rounded(px(6.))
                                            .bg(rgb(p.canvas))
                                            .border_1()
                                            .border_color(rgb(p.line))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .cursor_pointer()
                                            .text_size(px(11.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(p.text))
                                            .child("Stop")
                                            .child(tracked(&self.frame, "stop".into(), false))
                                            .on_click(cx.listener(|this, _, w, cx| {
                                                this.stop_turn(cx);
                                                w.focus(&this.input.focus_handle(cx));
                                            })),
                                    )
                                })
                                .when(!show_stop, |row| {
                                    row.child(
                                        div()
                                            .id("send-agent-message")
                                            .relative()
                                            .size(px(26.))
                                            .rounded(px(6.))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .when(can_send, |btn| {
                                                btn.bg(rgb(p.accent))
                                                    .cursor_pointer()
                                                    .child(icon("arrow", p.on_accent, 14.))
                                            })
                                            .when(!can_send, |btn| btn.child(icon("arrow", p.muted, 14.)))
                                            .child(tracked(&self.frame, "send".into(), false))
                                            .on_click(cx.listener(|this, _, w, cx| {
                                                this.enter(w, cx);
                                                w.focus(&this.input.focus_handle(cx));
                                            })),
                                    )
                                }),
                        )
                        .child(self.status_line(&state, p)),
                )
            })
            .child({
                // Last to prepaint: hands the frame's controls to the
                // control socket's snapshot, on the display's coordinates.
                let frame = self.frame.clone();
                let left = self.left;
                let output = self.output.clone();
                let list = menu.as_ref().map(|menu| agent::ListSnapshot {
                    kind: format!("{:?}", menu.kind).to_lowercase(),
                    items: menu.items.iter().map(|item| item.id.clone()).collect(),
                    highlighted: menu
                        .items
                        .get(self.highlight.min(menu.items.len().saturating_sub(1)))
                        .map(|item| item.id.clone()),
                    provider: menu.provider.clone(),
                    note: menu.note.clone(),
                });
                canvas(
                    move |_, _, _| {
                        let controls = controls_on_display(&frame.borrow(), left);
                        if let Ok(mut view) = agent_view.lock() {
                            view.controls = controls;
                            view.drawer_output = Some(output);
                            view.list = list;
                            view.draft = draft;
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_0()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        list_for_key, reconcile_draft_on_sent, should_answer_with_alt_digit,
        should_reset_offer_on_menu_close, should_take_composer_enter,
    };
    use crate::slash::MenuKind;
    use crate::{agent, slash};
    use gpui::Modifiers;
    use std::collections::HashMap;

    #[test]
    fn held_enter_and_modified_enter_are_ignored_in_composer() {
        let plain = Modifiers::default();
        assert!(should_take_composer_enter(false, &plain, true));
        assert!(!should_take_composer_enter(true, &plain, true));
        assert!(!should_take_composer_enter(false, &plain, false));
        assert!(!should_take_composer_enter(false, &Modifiers::shift(), true));
        assert!(!should_take_composer_enter(false, &Modifiers::control(), true));
        assert!(!should_take_composer_enter(false, &Modifiers::alt(), true));
    }

    #[test]
    fn alt_digit_is_ignored_when_held_or_away_from_thread() {
        assert!(should_answer_with_alt_digit(false, false, false));
        assert!(!should_answer_with_alt_digit(true, false, false));
        assert!(!should_answer_with_alt_digit(false, true, false));
        assert!(!should_answer_with_alt_digit(false, false, true));
    }

    #[test]
    fn list_for_key_changes_when_async_choices_arrive() {
        let mut state = agent::AgentState::new(true);
        state.connected = true;
        state.list_view = false;
        state.thread = Some("t-1".into());
        state.mode = Some("plan".into());
        let loading = slash::menu("/mode ", &state, None).unwrap();
        assert!(loading.items.is_empty());

        state.offer_known = true;
        state.modes = ["ask-first", "full-access", "plan"]
            .map(|id| agent::ModeInfo {
                id: id.into(),
                name: String::new(),
            })
            .to_vec();
        let loaded = slash::menu("/mode ", &state, None).unwrap();
        assert_eq!(loaded.start(), 2);
        assert_ne!(list_for_key(&loading, "/mode "), list_for_key(&loaded, "/mode "));
    }

    #[test]
    fn reconcile_draft_on_sent_preserves_post_send_edits_whether_open_or_closed() {
        // Exact match clears the draft (including new-thread "" key).
        let mut drafts = HashMap::from([("".to_string(), "hello".to_string())]);
        let next = reconcile_draft_on_sent(&mut drafts, "", "t-1", "hello", Some("hello"));
        assert_eq!(next.as_deref(), Some(""));
        assert!(!drafts.contains_key(""));
        assert!(!drafts.contains_key("t-1"));

        // Typed-ahead suffix is kept under the new thread ID.
        let mut drafts = HashMap::from([("".to_string(), "hello next".to_string())]);
        let next = reconcile_draft_on_sent(&mut drafts, "", "t-1", "hello", Some("hello next"));
        assert_eq!(next.as_deref(), Some("next"));
        assert_eq!(drafts.get("t-1").map(String::as_str), Some("next"));

        // Unrelated replacement while open is preserved, not wiped.
        let mut drafts = HashMap::from([("t-1".to_string(), "replacement".to_string())]);
        let next = reconcile_draft_on_sent(&mut drafts, "t-1", "t-1", "hello", Some("replacement"));
        assert_eq!(next, None);
        assert_eq!(drafts.get("t-1").map(String::as_str), Some("replacement"));

        // Closed drawer: exact submitted draft is cleared, but replaced edits stay.
        let mut drafts = HashMap::from([("".to_string(), "hello".to_string())]);
        reconcile_draft_on_sent(&mut drafts, "", "t-2", "hello", None);
        assert!(drafts.is_empty());

        let mut drafts = HashMap::from([("".to_string(), "rewritten before ack".to_string())]);
        reconcile_draft_on_sent(&mut drafts, "", "t-2", "hello", None);
        assert!(!drafts.contains_key(""));
        assert_eq!(drafts.get("t-2").map(String::as_str), Some("rewritten before ack"));
    }

    #[test]
    fn closing_effort_menu_resets_offer_guard_after_failed_model_lookup() {
        let mut state = agent::AgentState::new(true);
        state.modes_error = Some("Models and modes are not available right now.".into());
        assert!(should_reset_offer_on_menu_close(Some(MenuKind::Efforts), &state));
        assert!(should_reset_offer_on_menu_close(Some(MenuKind::Models), &state));
        assert!(!should_reset_offer_on_menu_close(Some(MenuKind::Commands), &state));

        state.modes_error = None;
        state.models.push(agent::ModelInfo {
            id: "acme/small".into(),
            name: "Small".into(),
            provider: Some("acme".into()),
            provider_name: Some("Acme".into()),
        });
        state.modes.push(agent::ModeInfo {
            id: "ask-first".into(),
            name: "Ask first".into(),
        });
        assert!(!should_reset_offer_on_menu_close(Some(MenuKind::Efforts), &state));
        // Closing `/model` always resets `asked_offer` so reopening picks up
        // out-of-band provider sign-ins even when the catalog was non-empty.
        assert!(should_reset_offer_on_menu_close(Some(MenuKind::Models), &state));
    }
}
