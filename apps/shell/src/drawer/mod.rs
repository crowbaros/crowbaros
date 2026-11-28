// SPDX-License-Identifier: MIT
//! The Agent's surface (ADR 0035): GPUI drawer view driven by
//! `agent::AgentController` (with zero Lever IPC or `Model`/entity mutations
//! during `Render::render`), plus the shell-side effect executor, summon
//! gesture handler, and control-socket bounds tracker.

mod render;

use gpui::{layer_shell::*, prelude::*, *};
use std::{cell::RefCell, rc::Rc, time::Duration};

use crate::{
    agent::{
        self, AgentEffect, AgentIntent, FocusTarget, ThreadAction, fullscreen_mark_overlay_output,
    },
    chrome,
    composer::Composer,
    compositor, input,
    ui::{Model, close_active_popover, layer, layer_with_margin},
};

pub const AGENT_HEADER_HEIGHT: f32 = 48.;

/// What the summon gesture, Super+Space, does (ADR 0035).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// Bring the surface up, with the keyboard.
    Show,
    /// The surface is up and the keyboard elsewhere: bring it back.
    TakeKeyboard,
    /// The keyboard is in the surface: put it away, and give the keyboard
    /// back to the window that had it.
    PutAway,
}

pub fn gesture_does(up: bool, keyboard_in_surface: bool) -> Gesture {
    match (up, keyboard_in_surface) {
        (false, _) => Gesture::Show,
        (true, false) => Gesture::TakeKeyboard,
        (true, true) => Gesture::PutAway,
    }
}

/// How wide the Agent's surface is on a display: about a third of it.
pub fn agent_surface_width(display_width: f32) -> f32 {
    (display_width / 3.).round()
}

/// Plain Enter in the focused message box counts once per physical press;
/// held key repeats and modified Enters (like Shift+Enter) are not taken.
pub(crate) fn should_take_composer_enter(
    is_held: bool,
    modifiers: &Modifiers,
    input_focused: bool,
) -> bool {
    input_focused
        && !is_held
        && !modifiers.shift
        && !modifiers.control
        && !modifiers.alt
        && !modifiers.platform
}

/// Alt+digit answers a waiting permission request only on an initial key press
/// while viewing the thread.
pub(crate) fn should_answer_with_alt_digit(
    is_held: bool,
    list_view: bool,
    confirm_delete: bool,
) -> bool {
    !is_held && !list_view && !confirm_delete
}

/// The controls the drawer lays out in one frame, gathered as it prepaints
/// so the control socket can say where each is.
#[derive(Default)]
pub(crate) struct ControlFrame {
    pub(crate) controls: Vec<(String, Bounds<Pixels>, bool)>,
    pub(crate) thread_area: Option<Bounds<Pixels>>,
}

/// An empty element over its parent that records the parent's bounds under
/// `id` as the drawer prepaints.
pub(crate) fn tracked(
    frame: &Rc<RefCell<ControlFrame>>,
    id: String,
    in_thread: bool,
) -> impl IntoElement {
    let frame = frame.clone();
    canvas(
        move |bounds, _, _| frame.borrow_mut().controls.push((id, bounds, in_thread)),
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// The controls of a frame in the display's logical pixels, leaving out
/// those of the thread scrolled out of view.
pub(crate) fn controls_on_display(frame: &ControlFrame, left: f32) -> Vec<agent::ControlBounds> {
    let round = |v: f32| (v * 10.).round() / 10.;
    let inside = |b: &Bounds<Pixels>, area: &Bounds<Pixels>| {
        let (top, bottom): (f32, f32) = (b.top().into(), b.bottom().into());
        let (area_top, area_bottom): (f32, f32) = (area.top().into(), area.bottom().into());
        top >= area_top - 0.5 && bottom <= area_bottom + 0.5
    };
    frame
        .controls
        .iter()
        .filter(|(_, b, in_thread)| {
            !in_thread
                || frame
                    .thread_area
                    .as_ref()
                    .is_some_and(|area| inside(b, area))
        })
        .map(|(id, b, _)| {
            let (x, y, w, h): (f32, f32, f32, f32) = (
                b.origin.x.into(),
                b.origin.y.into(),
                b.size.width.into(),
                b.size.height.into(),
            );
            agent::ControlBounds {
                id: id.clone(),
                x: round(left + x),
                y: round(y),
                w: round(w),
                h: round(h),
            }
        })
        .collect()
}

/// Keeps the `crowbar-shell`-only control-socket snapshot of the drawer and
/// mark up to date.
pub fn sync_agent_view(model: &Entity<Model>, cx: &App) {
    let m = model.read(cx);
    if let Ok(mut view) = m.agent_view.lock() {
        view.drawer_up = m.agent_window.is_some();
        view.mark_overlay = m.mark_overlay.is_some();
        view.mark_overlay_output = m.mark_overlay.as_ref().map(|(_, out)| out.clone());
        view.state = m.agent.state().clone();
        if !view.drawer_up {
            view.controls.clear();
            view.drawer_output = None;
            view.list = None;
            view.draft.clear();
        }
    }
}

/// Opens, moves, or removes the floating fullscreen mark overlay to match
/// whether a fullscreen window is covering the bar while the mark is active.
pub fn sync_mark_overlay(model: &Entity<Model>, cx: &mut App) -> bool {
    let (want_output, existing_output, mark) = {
        let m = model.read(cx);
        let want = fullscreen_mark_overlay_output(
            m.agent.state().mark,
            &m.snapshot,
            m.agent_window.is_some(),
        );
        let existing = m.mark_overlay.as_ref().map(|(_, out)| out.clone());
        (want, existing, m.agent.state().mark)
    };
    let mut overlay_changed = false;
    if want_output != existing_output {
        if let Some((w, _)) = model.update(cx, |m, _| m.mark_overlay.take()) {
            log::info!("SHELL agent-mark overlay=false state={}", mark.label());
            let _ = w.update(cx, |_, w, _| w.remove_window());
            overlay_changed = true;
        }
        if let Some(target_output) = want_output {
            let overlay_model = model.clone();
            match cx.open_window(
                layer_with_margin(
                    "mark",
                    Layer::Overlay,
                    Anchor::TOP | Anchor::RIGHT,
                    112.,
                    28.,
                    Some(-1.),
                    Some((px(8.), px(12.), px(0.), px(0.))),
                    KeyboardInteractivity::None,
                ),
                move |_, cx| {
                    cx.new(|_| chrome::MarkOverlay {
                        model: overlay_model,
                    })
                },
            ) {
                Ok(w) => {
                    log::info!(
                        "SHELL agent-mark overlay=true output={} state={}",
                        target_output,
                        mark.label()
                    );
                    model.update(cx, |m, _| m.mark_overlay = Some((w, target_output)));
                    overlay_changed = true;
                }
                Err(e) => log::error!("Opening the Agent mark overlay: {e}"),
            }
        }
    }
    sync_agent_view(model, cx);
    overlay_changed
}

#[derive(Debug, PartialEq, Eq)]
enum ShellEffect {
    SendLeverCommand(agent::types::AgentCommand),
    SavePicks {
        model: Option<String>,
        effort: Option<String>,
    },
    LaunchSignIn,
    AgentKeyboard(bool),
    SyncMarkOverlay,
    SyncAgentView,
    LogMarkChange {
        prev: agent::Mark,
        next: agent::Mark,
    },
    LogRequestAnswer(String),
    NotifyMarkOverlay,
    RefreshWindows,
}

#[derive(Debug, PartialEq, Eq)]
enum ViewEffect {
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
    NotifyDrawer,
}

#[derive(Debug, PartialEq, Eq)]
enum RoutedEffect {
    Shell(ShellEffect),
    View(ViewEffect),
}

/// Exhaustively routes every `AgentEffect` to either the shell runtime or the
/// `AgentSurface` view so adding a new effect is a compile error if unhandled.
fn classify_effect(effect: AgentEffect) -> RoutedEffect {
    match effect {
        AgentEffect::SendLeverCommand(cmd) => RoutedEffect::Shell(ShellEffect::SendLeverCommand(cmd)),
        AgentEffect::SavePicks { model, effort } => {
            RoutedEffect::Shell(ShellEffect::SavePicks { model, effort })
        }
        AgentEffect::LaunchSignIn => RoutedEffect::Shell(ShellEffect::LaunchSignIn),
        AgentEffect::AgentKeyboard(to_surface) => {
            RoutedEffect::Shell(ShellEffect::AgentKeyboard(to_surface))
        }
        AgentEffect::SyncMarkOverlay => RoutedEffect::Shell(ShellEffect::SyncMarkOverlay),
        AgentEffect::SyncAgentView => RoutedEffect::Shell(ShellEffect::SyncAgentView),
        AgentEffect::LogMarkChange { prev, next } => {
            RoutedEffect::Shell(ShellEffect::LogMarkChange { prev, next })
        }
        AgentEffect::LogRequestAnswer(answer) => {
            RoutedEffect::Shell(ShellEffect::LogRequestAnswer(answer))
        }
        AgentEffect::NotifyMarkOverlay => RoutedEffect::Shell(ShellEffect::NotifyMarkOverlay),
        AgentEffect::RefreshWindows => RoutedEffect::Shell(ShellEffect::RefreshWindows),
        AgentEffect::SetComposerText(text) => RoutedEffect::View(ViewEffect::SetComposerText(text)),
        AgentEffect::SetProjectNameText(text) => {
            RoutedEffect::View(ViewEffect::SetProjectNameText(text))
        }
        AgentEffect::SetProjectFolderText(text) => {
            RoutedEffect::View(ViewEffect::SetProjectFolderText(text))
        }
        AgentEffect::SyncComposerMenu {
            list_open,
            rail_open,
        } => RoutedEffect::View(ViewEffect::SyncComposerMenu {
            list_open,
            rail_open,
        }),
        AgentEffect::Focus(target) => RoutedEffect::View(ViewEffect::Focus(target)),
        AgentEffect::ScrollToBottom => RoutedEffect::View(ViewEffect::ScrollToBottom),
        AgentEffect::ScrollToBottomIfNearBottom => {
            RoutedEffect::View(ViewEffect::ScrollToBottomIfNearBottom)
        }
        AgentEffect::ResetScroll => RoutedEffect::View(ViewEffect::ResetScroll),
        AgentEffect::NotifyDrawer => RoutedEffect::View(ViewEffect::NotifyDrawer),
    }
}

/// Executes one shell-level (non-drawer-view) effect. Returns `true` when
/// windows should be refreshed.
fn execute_shell_effect(model: &Entity<Model>, effect: ShellEffect, cx: &mut App) -> bool {
    match effect {
        ShellEffect::SendLeverCommand(cmd) => {
            if let Some(tx) = model.read(cx).agent_tx.clone() {
                tx.dispatch(cmd);
            }
            false
        }
        ShellEffect::SavePicks {
            model: pick_model,
            effort,
        } => {
            agent::save_picks(pick_model.as_deref(), effort.as_deref());
            false
        }
        ShellEffect::LaunchSignIn => {
            let (launcher, compositor, tx) = {
                let m = model.read(cx);
                (
                    m.launcher_client.clone(),
                    m.compositor.clone(),
                    m.tx.clone(),
                )
            };
            launcher.launch_agent_sign_in(compositor, tx);
            false
        }
        ShellEffect::AgentKeyboard(to_surface) => {
            model.read(cx).compositor.agent_keyboard(to_surface);
            false
        }
        ShellEffect::SyncMarkOverlay => sync_mark_overlay(model, cx),
        ShellEffect::SyncAgentView => {
            sync_agent_view(model, cx);
            false
        }
        ShellEffect::LogMarkChange { prev, next } => {
            log::info!(
                "SHELL agent-mark state={} prev={}",
                next.label(),
                prev.label()
            );
            false
        }
        ShellEffect::LogRequestAnswer(answer) => {
            log::info!("SHELL agent-request answer={answer}");
            false
        }
        ShellEffect::NotifyMarkOverlay => {
            if let Some((w, _)) = model.read(cx).mark_overlay {
                let _ = w.update(cx, |_, _, cx| cx.notify());
            }
            false
        }
        ShellEffect::RefreshWindows => true,
    }
}

/// Applies effects emitted outside `AgentSurface` (e.g. from Lever events or
/// summon gestures). Returns `true` when GPUI windows should be refreshed.
pub fn apply_effects_from_app(
    model: &Entity<Model>,
    effects: Vec<AgentEffect>,
    cx: &mut App,
) -> bool {
    let mut refresh = false;
    for effect in effects {
        match classify_effect(effect) {
            RoutedEffect::View(view_effect) => {
                if let Some(w) = model.read(cx).agent_window {
                    let _ = w.update(cx, |view, w, cx| {
                        view.apply_view_effect(view_effect, Some(w), cx)
                    });
                }
            }
            RoutedEffect::Shell(shell_effect) => {
                if execute_shell_effect(model, shell_effect, cx) {
                    refresh = true;
                }
            }
        }
    }
    refresh
}

/// GPUI view for the Agent's drawer surface.
/// Holds only GPUI handles, inputs, scroll/layout tracking, and subscriptions;
/// all domain and UI state lives in `Model::agent` (`AgentController`).
/// `Render::render` performs zero Lever IPC and zero `Model`/entity mutations
/// (it only clears the view-local scroll flag and publishes prepaint control
/// bounds to `agent_view`).
pub struct AgentSurface {
    pub(crate) model: Entity<Model>,
    pub(crate) focus: FocusHandle,
    pub(crate) input: Entity<Composer>,
    pub(crate) project_name_input: Entity<input::TextInput>,
    pub(crate) project_folder_input: Entity<input::TextInput>,
    pub(crate) scroll: ScrollHandle,
    pub(crate) scroll_to_bottom: bool,
    pub(crate) output: String,
    pub(crate) left: f32,
    pub(crate) frame: Rc<RefCell<ControlFrame>>,
    _input_sub: Subscription,
    _project_name_sub: Subscription,
    _project_folder_sub: Subscription,
    _release_sub: Subscription,
    _tick: Task<()>,
}

impl AgentSurface {
    pub fn new(
        model: Entity<Model>,
        output: String,
        left: f32,
        cx: &mut Context<Self>,
    ) -> Self {
        let palette = model.read(cx).palette();
        let input = cx.new(|cx| {
            let mut composer = Composer::new("Ask the Agent. / for commands", 16384, cx);
            composer.palette = palette;
            composer
        });
        let project_name_input = cx.new(|cx| {
            let mut field = input::TextInput::with_placeholder("Project name", 128, cx);
            field.palette = palette;
            field
        });
        let project_folder_input = cx.new(|cx| {
            let mut field = input::TextInput::with_placeholder("~/Projects/<name>", 512, cx);
            field.palette = palette;
            field
        });
        let focus = cx.focus_handle();

        let input_sub = cx.observe(&input, |this, input, cx| {
            let text = input.read(cx).text();
            let effects = this
                .model
                .update(cx, |m, _| m.agent.handle_intent(AgentIntent::DraftChanged(text)));
            this.apply_effects(effects, None, cx);
        });
        let project_name_sub = cx.observe(&project_name_input, |this, name_input, cx| {
            let name = name_input.read(cx).query();
            let effects = this.model.update(cx, |m, _| {
                m.agent.handle_intent(AgentIntent::ProjectNameChanged(name))
            });
            this.apply_effects(effects, None, cx);
        });
        let project_folder_sub = cx.observe(&project_folder_input, |this, folder_input, cx| {
            let folder = folder_input.read(cx).query();
            let effects = this.model.update(cx, |m, _| {
                m.agent
                    .handle_intent(AgentIntent::ProjectFolderChanged(folder))
            });
            this.apply_effects(effects, None, cx);
        });
        let release_sub = cx.on_release(|this, cx| {
            if reconcile_drawer_window(&this.model, cx) {
                cx.refresh_windows();
            }
        });
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let alive = this.update(cx, |this, cx| {
                    if this.model.read(cx).agent.state().working() {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });

        Self {
            model,
            focus,
            input,
            project_name_input,
            project_folder_input,
            scroll: ScrollHandle::new(),
            scroll_to_bottom: false,
            output,
            left,
            frame: Rc::new(RefCell::new(ControlFrame::default())),
            _input_sub: input_sub,
            _project_name_sub: project_name_sub,
            _project_folder_sub: project_folder_sub,
            _release_sub: release_sub,
            _tick: tick,
        }
    }

    /// Synchronizes child input palettes whenever the active theme changes, so
    /// `Render::render` performs zero entity updates.
    pub fn sync_palette(&mut self, cx: &mut Context<Self>) {
        let p = self.model.read(cx).palette();
        self.input.update(cx, |input, _| input.palette = p);
        self.project_name_input
            .update(cx, |input, _| input.palette = p);
        self.project_folder_input
            .update(cx, |input, _| input.palette = p);
        cx.notify();
    }

    pub(crate) fn near_bottom(&self) -> bool {
        self.scroll.max_offset().height + self.scroll.offset().y <= px(2.)
    }

    /// Dispatches an `AgentIntent` from a view handler where `window` is at
    /// hand, returning `true` if any effects were emitted.
    pub(crate) fn dispatch_intent(
        &mut self,
        intent: AgentIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let effects = self
            .model
            .update(cx, |m, _| m.agent.handle_intent(intent));
        let had_effects = !effects.is_empty();
        self.apply_effects(effects, Some(window), cx);
        had_effects
    }

    /// Single exhaustive effect executor used by both windowed handlers and
    /// entity observers (`window = None`).
    fn apply_effects(
        &mut self,
        effects: Vec<AgentEffect>,
        mut window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let mut refresh = false;
        for effect in effects {
            match classify_effect(effect) {
                RoutedEffect::View(view_effect) => {
                    self.apply_view_effect(view_effect, window.as_deref_mut(), cx);
                }
                RoutedEffect::Shell(shell_effect) => {
                    if execute_shell_effect(&self.model, shell_effect, cx) {
                        refresh = true;
                    }
                }
            }
        }
        if refresh {
            cx.refresh_windows();
        }
    }

    fn apply_view_effect(
        &mut self,
        effect: ViewEffect,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        match effect {
            ViewEffect::SetComposerText(text) => {
                if self.input.read(cx).text() != text {
                    self.input.update(cx, |input, cx| input.set_text(text, cx));
                }
            }
            ViewEffect::SetProjectNameText(text) => {
                if self.project_name_input.read(cx).query() != text {
                    self.project_name_input
                        .update(cx, |input, cx| input.set_text(text, cx));
                }
            }
            ViewEffect::SetProjectFolderText(text) => {
                if self.project_folder_input.read(cx).query() != text {
                    self.project_folder_input
                        .update(cx, |input, cx| input.set_text(text, cx));
                }
            }
            ViewEffect::SyncComposerMenu {
                list_open,
                rail_open,
            } => {
                self.input.update(cx, |input, _| {
                    input.list_open = list_open;
                    input.rail_open = rail_open;
                });
            }
            ViewEffect::Focus(target) => {
                if let Some(window) = window {
                    match target {
                        FocusTarget::Surface => window.focus(&self.focus),
                        FocusTarget::Composer => window.focus(&self.input.focus_handle(cx)),
                        FocusTarget::ProjectName => {
                            window.focus(&self.project_name_input.focus_handle(cx))
                        }
                        FocusTarget::ProjectFolder => {
                            window.focus(&self.project_folder_input.focus_handle(cx))
                        }
                    }
                } else {
                    log::warn!("SHELL agent-surface dropped Focus({target:?}) without window");
                }
            }
            ViewEffect::ScrollToBottom => {
                self.scroll_to_bottom = true;
                cx.notify();
            }
            ViewEffect::ScrollToBottomIfNearBottom => {
                if self.near_bottom() {
                    self.scroll_to_bottom = true;
                    self.scroll.scroll_to_bottom();
                }
            }
            ViewEffect::ResetScroll => {
                self.scroll_to_bottom = false;
                self.scroll.set_offset(point(px(0.), px(0.)));
            }
            ViewEffect::NotifyDrawer => {
                cx.notify();
            }
        }
    }

    pub(crate) fn action(
        &mut self,
        action: ThreadAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dispatch_intent(AgentIntent::Action(action), window, cx);
    }

    /// Puts the surface away. The keyboard goes back to the window that had
    /// it, if the surface holds it. A working turn keeps going behind it.
    pub fn put_away(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        log::info!("SHELL agent-surface up=false");
        let effects = self.model.update(cx, |m, _| {
            m.agent_window = None;
            m.agent.handle_intent(AgentIntent::DrawerClosed)
        });
        window.remove_window();
        self.apply_effects(effects, Some(window), cx);
    }
}

/// Checks whether the tracked drawer window is still live in GPUI. If the
/// compositor closed it externally (for example, when removing its display),
/// clears the stale handle and runs `AgentIntent::DrawerClosed` so subsequent
/// Lever events never treat a vanished drawer as open. Returns `true` when
/// windows should be refreshed.
pub fn reconcile_drawer_window(model: &Entity<Model>, cx: &mut App) -> bool {
    let Some(w) = model.read(cx).agent_window else {
        return false;
    };
    if w.update(cx, |_, _, _| ()).is_ok() {
        return false;
    }
    log::info!("SHELL agent-surface up=false reason=external-close");
    let effects = model.update(cx, |m, _| {
        m.agent_window = None;
        m.agent.handle_intent(AgentIntent::DrawerClosed)
    });
    apply_effects_from_app(model, effects, cx)
}

/// Brings the Agent's surface up on the display the gesture named, a third
/// of it wide, and asks the compositor to give it the keyboard.
fn show_agent_surface(model: &Entity<Model>, summon: &compositor::Summon, cx: &mut App) {
    let width = agent_surface_width(summon.width);
    log::info!(
        "SHELL agent-surface up=true output={} width={width}",
        summon.output
    );
    let surface = model.clone();
    let output = summon.output.clone();
    let left = (summon.width - width).max(0.);
    let result = cx.open_window(
        layer(
            "agent",
            Layer::Overlay,
            Anchor::TOP | Anchor::BOTTOM | Anchor::RIGHT,
            width,
            0.,
            Some(-1.),
            KeyboardInteractivity::OnDemand,
        ),
        move |_, cx| cx.new(|cx| AgentSurface::new(surface, output, left, cx)),
    );
    match result {
        Ok(w) => {
            let effects = model.update(cx, |m, _| {
                m.agent_window = Some(w);
                m.agent.handle_intent(AgentIntent::DrawerOpened)
            });
            if apply_effects_from_app(model, effects, cx) {
                cx.refresh_windows();
            }
        }
        Err(e) => log::error!("Summoning the Agent's surface: {e}"),
    }
}

/// The summon gesture: closes any active exclusive shell popover (so it cannot
/// retain keyboard focus over the Agent's surface), reconciles any external
/// compositor close of the drawer window, then brings the surface up, takes the
/// keyboard back, or puts it away.
pub fn summoned(model: &Entity<Model>, summon: compositor::Summon, cx: &mut App) {
    close_active_popover(model, cx);
    if reconcile_drawer_window(model, cx) {
        cx.refresh_windows();
    }
    let agent = model.read(cx).agent_window;
    let up = agent.is_some();
    match gesture_does(up, summon.keyboard) {
        Gesture::Show => show_agent_surface(model, &summon, cx),
        Gesture::TakeKeyboard => {
            log::info!("SHELL agent-surface keyboard=true");
            let effects = model.update(cx, |m, _| m.agent.handle_intent(AgentIntent::TakeKeyboard));
            if apply_effects_from_app(model, effects, cx) {
                cx.refresh_windows();
            }
        }
        Gesture::PutAway => {
            if let Some(w) = agent {
                let _ = w.update(cx, |view, w, cx| view.put_away(w, cx));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Gesture, agent_surface_width, gesture_does, should_answer_with_alt_digit,
        should_take_composer_enter,
    };
    use crate::agent::{
        AgentState,
        controller::{
            PlaceSuggestion, browse_start_path, display_folder_path, join_project_place,
            list_for_key, place_suggestions, reconcile_draft_on_sent,
            should_reset_offer_on_menu_close, validate_project_form,
        },
        types::{ModeInfo, ModelInfo, ThreadSummary},
    };
    use crate::slash::{self, MenuKind};
    use gpui::Modifiers;
    use std::collections::HashMap;

    #[test]
    fn classify_effect_routes_shell_and_view_effects_exhaustively() {
        use super::{RoutedEffect, ShellEffect, ViewEffect, classify_effect};
        use crate::agent::{AgentEffect, FocusTarget, Mark, types::AgentCommand};

        assert!(matches!(
            classify_effect(AgentEffect::SendLeverCommand(AgentCommand::RefreshStatus)),
            RoutedEffect::Shell(ShellEffect::SendLeverCommand(AgentCommand::RefreshStatus))
        ));
        assert!(matches!(
            classify_effect(AgentEffect::LogRequestAnswer("once".into())),
            RoutedEffect::Shell(ShellEffect::LogRequestAnswer(a)) if a == "once"
        ));
        assert!(matches!(
            classify_effect(AgentEffect::LogMarkChange {
                prev: Mark::Idle,
                next: Mark::Working,
            }),
            RoutedEffect::Shell(ShellEffect::LogMarkChange { .. })
        ));
        assert!(matches!(
            classify_effect(AgentEffect::Focus(FocusTarget::Composer)),
            RoutedEffect::View(ViewEffect::Focus(FocusTarget::Composer))
        ));
        assert!(matches!(
            classify_effect(AgentEffect::ScrollToBottom),
            RoutedEffect::View(ViewEffect::ScrollToBottom)
        ));
        assert!(matches!(
            classify_effect(AgentEffect::ScrollToBottomIfNearBottom),
            RoutedEffect::View(ViewEffect::ScrollToBottomIfNearBottom)
        ));
        assert!(matches!(
            classify_effect(AgentEffect::SyncComposerMenu {
                list_open: true,
                rail_open: false,
            }),
            RoutedEffect::View(ViewEffect::SyncComposerMenu {
                list_open: true,
                rail_open: false,
            })
        ));
    }

    #[test]
    fn the_summon_gesture_shows_the_agents_surface_returns_the_keyboard_or_puts_it_away() {
        assert_eq!(gesture_does(false, false), Gesture::Show);
        assert_eq!(gesture_does(true, false), Gesture::TakeKeyboard);
        assert_eq!(gesture_does(true, true), Gesture::PutAway);
        assert_eq!(gesture_does(false, true), Gesture::Show);
    }

    #[test]
    fn the_agents_surface_is_a_third_of_its_display() {
        assert_eq!(agent_surface_width(1280.), 427.);
        assert_eq!(agent_surface_width(2560.), 853.);
        assert_eq!(agent_surface_width(1024.), 341.);
    }

    #[test]
    fn held_enter_and_modified_enter_are_ignored_in_composer() {
        let plain = Modifiers::default();
        assert!(should_take_composer_enter(false, &plain, true));
        assert!(!should_take_composer_enter(true, &plain, true));
        assert!(!should_take_composer_enter(false, &plain, false));
        assert!(!should_take_composer_enter(false, &Modifiers::shift(), true));
        assert!(!should_take_composer_enter(
            false,
            &Modifiers::control(),
            true
        ));
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
        let mut state = AgentState::new(true);
        state.connected = true;
        state.list_view = false;
        state.thread = Some("t-1".into());
        state.mode = Some("plan".into());
        let loading = slash::menu("/mode ", &state, None).unwrap();
        assert!(loading.items.is_empty());

        state.offer_known = true;
        state.modes = ["ask-first", "full-access", "plan"]
            .map(|id| ModeInfo {
                id: id.into(),
                name: String::new(),
            })
            .to_vec();
        let loaded = slash::menu("/mode ", &state, None).unwrap();
        assert_eq!(loaded.start(), 2);
        assert_ne!(
            list_for_key(&loading, "/mode "),
            list_for_key(&loaded, "/mode ")
        );
    }

    #[test]
    fn reconcile_draft_on_sent_preserves_post_send_edits_whether_open_or_closed() {
        let mut drafts = HashMap::from([("".to_string(), "hello".to_string())]);
        let next = reconcile_draft_on_sent(&mut drafts, "", "t-1", "hello", Some("hello"));
        assert_eq!(next.as_deref(), Some(""));
        assert!(!drafts.contains_key(""));
        assert!(!drafts.contains_key("t-1"));

        let mut drafts = HashMap::from([("".to_string(), "hello next".to_string())]);
        let next = reconcile_draft_on_sent(&mut drafts, "", "t-1", "hello", Some("hello next"));
        assert_eq!(next.as_deref(), Some("next"));
        assert_eq!(drafts.get("t-1").map(String::as_str), Some("next"));

        let mut drafts = HashMap::from([("t-1".to_string(), "replacement".to_string())]);
        let next = reconcile_draft_on_sent(&mut drafts, "t-1", "t-1", "hello", Some("replacement"));
        assert_eq!(next, None);
        assert_eq!(drafts.get("t-1").map(String::as_str), Some("replacement"));

        let mut drafts = HashMap::from([("".to_string(), "hello".to_string())]);
        reconcile_draft_on_sent(&mut drafts, "", "t-2", "hello", None);
        assert!(drafts.is_empty());

        let mut drafts = HashMap::from([("".to_string(), "rewritten before ack".to_string())]);
        reconcile_draft_on_sent(&mut drafts, "", "t-2", "hello", None);
        assert!(!drafts.contains_key(""));
        assert_eq!(
            drafts.get("t-2").map(String::as_str),
            Some("rewritten before ack")
        );
    }

    #[test]
    fn closing_effort_menu_resets_offer_guard_after_failed_model_lookup() {
        let mut state = AgentState::new(true);
        state.modes_error = Some("Models and modes are not available right now.".into());
        assert!(should_reset_offer_on_menu_close(
            Some(MenuKind::Efforts),
            &state
        ));
        assert!(should_reset_offer_on_menu_close(
            Some(MenuKind::Models),
            &state
        ));
        assert!(!should_reset_offer_on_menu_close(
            Some(MenuKind::Commands),
            &state
        ));

        state.modes_error = None;
        state.models.push(ModelInfo {
            id: "acme/small".into(),
            name: "Small".into(),
            provider: Some("acme".into()),
            provider_name: Some("Acme".into()),
        });
        state.modes.push(ModeInfo {
            id: "ask-first".into(),
            name: "Ask first".into(),
        });
        assert!(!should_reset_offer_on_menu_close(
            Some(MenuKind::Efforts),
            &state
        ));
        assert!(should_reset_offer_on_menu_close(
            Some(MenuKind::Models),
            &state
        ));
    }

    #[test]
    fn make_project_form_offers_projects_folder_and_allows_custom_folder() {
        assert_eq!(join_project_place("~/Projects", ""), "~/Projects");
        assert_eq!(
            join_project_place("~/Projects", "  notes "),
            "~/Projects/notes"
        );
        assert_eq!(join_project_place("~/Code", "notes"), "~/Code/notes");
        assert_eq!(join_project_place("~/Code/", ""), "~/Code");
        assert_eq!(join_project_place("~/notes ", "my-app"), "~/notes /my-app");
        assert_eq!(join_project_place("~/notes ", ""), "~/notes ");
        assert_eq!(browse_start_path("  ~/notes "), Some("~/notes ".into()));
        assert_eq!(browse_start_path("   "), None);
        assert_eq!(
            display_folder_path("/home/crowbar/Projects/notes", Some("/home/crowbar")),
            "~/Projects/notes"
        );
        assert_eq!(
            display_folder_path("/home/crowbar", Some("/home/crowbar")),
            "~"
        );

        assert_eq!(
            validate_project_form("notes", "~/Projects/notes", Some("~/Projects")),
            Ok((Some("notes".into()), None, None))
        );
        assert!(validate_project_form("", "~/Projects", Some("~/Projects")).is_err());
        assert!(validate_project_form("a/b", "~/Projects/a/b", Some("~/Projects")).is_err());
        assert!(validate_project_form("bad\nname", "~/Projects/bad", Some("~/Projects")).is_err());

        assert!(validate_project_form("", "~/Code", Some("~/Code")).is_err());
        assert!(validate_project_form("", "~", Some("~")).is_err());
        assert_eq!(
            validate_project_form("notes", "~/Code/notes", Some("~/Code")),
            Ok((Some("notes".into()), Some("~/Code".into()), None))
        );

        assert_eq!(
            validate_project_form("notes", "~/Code/notes", None),
            Ok((None, None, Some("~/Code/notes".into())))
        );
        assert_eq!(
            validate_project_form("notes", "~/notes ", None),
            Ok((None, None, Some("~/notes ".into())))
        );
        assert_eq!(
            validate_project_form("my-app", "~/notes /my-app", Some("~/notes ")),
            Ok((Some("my-app".into()), Some("~/notes ".into()), None))
        );
        assert_eq!(
            validate_project_form("", "/home/crowbar/ExistingWork", None),
            Ok((None, None, Some("/home/crowbar/ExistingWork".into())))
        );
        assert!(validate_project_form("", "relative/path", None).is_err());

        let mut state = AgentState::new(true);
        state.cwd = Some("/home/shared/team/app".into());
        assert_eq!(
            display_folder_path("/home/shared/team/app", None),
            "/home/shared/team/app"
        );
        assert_eq!(
            display_folder_path("/home/shared/team/app", Some("/home/person")),
            "/home/shared/team/app"
        );
        state.threads.push(ThreadSummary {
            archived: false,
            thread: "t-1".into(),
            title: "t".into(),
            cwd: "/home/shared/team/app".into(),
            model: None,
            started: 0,
            updated: 0,
            turns: 1,
            state: "done".into(),
            working: false,
            waiting: false,
            subagents: 0,
            mode: None,
        });
        state.threads.push(ThreadSummary {
            archived: false,
            thread: "t-2".into(),
            title: "t2".into(),
            cwd: "/home/person/Work/app".into(),
            model: None,
            started: 0,
            updated: 0,
            turns: 1,
            state: "done".into(),
            working: false,
            waiting: false,
            subagents: 0,
            mode: None,
        });
        state.threads.push(ThreadSummary {
            archived: false,
            thread: "t-3".into(),
            title: "t3".into(),
            cwd: "/home/person/Projects/existing".into(),
            model: None,
            started: 0,
            updated: 0,
            turns: 1,
            state: "done".into(),
            working: false,
            waiting: false,
            subagents: 0,
            mode: None,
        });
        assert_eq!(
            place_suggestions(&state, None),
            vec![
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
                PlaceSuggestion {
                    label: "/home/shared/team".into(),
                    value: "/home/shared/team".into(),
                },
                PlaceSuggestion {
                    label: "/home/person/Work".into(),
                    value: "/home/person/Work".into(),
                },
                PlaceSuggestion {
                    label: "/home/person/Projects".into(),
                    value: "/home/person/Projects".into(),
                },
            ]
        );
        assert_eq!(
            place_suggestions(&state, Some("/home/person")),
            vec![
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
                PlaceSuggestion {
                    label: "/home/shared/team".into(),
                    value: "/home/shared/team".into(),
                },
                PlaceSuggestion {
                    label: "~/Work".into(),
                    value: "/home/person/Work".into(),
                },
            ]
        );
    }
}
