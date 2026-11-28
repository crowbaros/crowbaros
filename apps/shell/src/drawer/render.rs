// SPDX-License-Identifier: MIT
//! Purely declarative GPUI rendering for [`AgentSurface`].
//! Performs zero Lever IPC and zero `Model` mutations during paint.

use gpui::{prelude::*, *};

use super::{
    AGENT_HEADER_HEIGHT, AgentSurface, ControlFrame, controls_on_display,
    should_answer_with_alt_digit, should_take_composer_enter, tracked,
};
use crate::{
    agent::{
        self, AgentController, AgentIntent, ThreadAction, display_folder_path, join_project_place,
        place_suggestions, validate_project_form,
    },
    appearance::Palette,
    chrome, composer, markdown,
    slash::{Menu, MenuKind},
    ui::{Next, Previous, SendMessage, icon, palette_keycap},
};

/// The most rows of a list shown at once. A longer list moves with the keys.
const LIST_ROWS: usize = 10;

impl AgentSurface {
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
            ThreadAction::AskMakeProject => "make-project".into(),
            ThreadAction::CancelMakeProject => "cancel-make-project".into(),
            ThreadAction::ConfirmMakeProject => "confirm-make-project".into(),
            ThreadAction::PickProjectPlace(place) => format!("project-place:{place}"),
            ThreadAction::ToggleProjectBrowse => "project-browse".into(),
            ThreadAction::BrowseProjectDir(dir) => format!("project-dir:{dir}"),
            ThreadAction::UseProjectFolder(path) => format!("project-use:{path}"),
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
            ThreadAction::Unqueue(id) => format!("unqueue:{id}"),
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
            .when(enabled, |s| {
                s.cursor_pointer().hover(move |s| s.bg(rgb(p.surface)))
            })
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
        let header = match (
            req.subagent.as_deref(),
            subagent_title.filter(|s| !s.trim().is_empty()),
        ) {
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
                            this.dispatch_intent(
                                AgentIntent::AnswerRequest {
                                    request: request.clone(),
                                    answer: answer.clone(),
                                },
                                w,
                                cx,
                            );
                            w.focus(&this.input.focus_handle(cx));
                        })),
                );
            }
            card = card.child(row);
        }
        card
    }

    fn act_row(
        &self,
        controller: &AgentController,
        turn_state: &agent::TurnState,
        act: &agent::Act,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let display = agent::act_display_status(turn_state, act);
        let state_color = match display.tone {
            agent::ActTone::Active => p.focus,
            agent::ActTone::Failed => p.text,
            agent::ActTone::Muted => p.muted,
        };
        let detail = act
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        let open = controller.expanded_acts.contains(&act.id);
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
            let first = agent::format_act_detail(detail);
            let long = first != detail;
            let whole = controller.whole_acts.contains(&act.id);
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
                    if whole {
                        "All of it".into()
                    } else {
                        "Show all of it".into()
                    },
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

    fn subagent_section(
        &self,
        controller: &AgentController,
        turn: &agent::Turn,
        sub: &agent::Subagent,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = controller.expanded_subagents.contains(&sub.id);
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
                    acts_col =
                        acts_col.child(self.act_row(controller, &sub_turn_state, act, p, cx));
                }
                inner = inner.child(acts_col);
            }
            for (idx, msg) in sub
                .messages
                .iter()
                .filter(|m| m.role != "prompt")
                .enumerate()
            {
                if !msg.text.trim().is_empty() {
                    inner =
                        inner.child(answer_blocks(&format!("sub-{}-{idx}", sub.id), &msg.text, p));
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

    fn list_panel(
        &self,
        controller: &AgentController,
        menu: &Menu,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let highlight = controller
            .highlight
            .min(menu.items.len().saturating_sub(1));
        let first = highlight
            .saturating_sub(LIST_ROWS - 1)
            .min(menu.items.len().saturating_sub(LIST_ROWS));
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
                            .when(commands, |d| {
                                d.font_family("IBM Plex Mono").text_size(px(11.5))
                            })
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
                        item.tag
                            .clone()
                            .or_else(|| item.current.then(|| "Current".to_string())),
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
                    .on_mouse_move(cx.listener(move |this, _, w, cx| {
                        this.dispatch_intent(AgentIntent::HoverMenuRow(at), w, cx);
                    }))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.dispatch_intent(AgentIntent::ClickMenuRow(at), w, cx);
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
                        .when(!on, |r| {
                            r.text_color(rgb(p.muted))
                                .hover(move |s| s.text_color(rgb(p.text)))
                        })
                        .child(div().w_full().text_ellipsis().child(provider.name.clone()))
                        .child(tracked(&self.frame, id, false))
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.dispatch_intent(
                                AgentIntent::SelectProvider(pick.clone()),
                                w,
                                cx,
                            );
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
        let free_on = controller.state.free_hosted_models;
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
                                    this.dispatch_intent(AgentIntent::ModelPickerSignIn, w, cx);
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
                                    this.dispatch_intent(
                                        AgentIntent::ModelPickerFreeModels(next_free),
                                        w,
                                        cx,
                                    );
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
            markdown::Block::Paragraph(line) => {
                div().w_full().child(styled(id, &line, p, FontWeight::NORMAL))
            }
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
                .child(if text.is_empty() {
                    " ".to_string()
                } else {
                    text
                }),
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
                                div().flex_1().min_w_0().child(styled(
                                    item_id,
                                    &item.line,
                                    p,
                                    FontWeight::NORMAL,
                                )),
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
        let (p, mark_asset, controller, agent_view) = {
            let m = self.model.read(cx);
            (
                m.palette(),
                chrome::bar_mark_asset(&m.preferences),
                m.agent.clone(),
                m.agent_view.clone(),
            )
        };
        let mark = ImageSource::Resource(Resource::Embedded(mark_asset.into()));
        let state = &controller.state;
        let show_stop = state.can_stop();
        let needs_choice = state.needs_model_choice();
        let draft = controller.current_draft().to_string();
        let now = agent::unix_now();
        let can_send = state.can_send() && !draft.trim().is_empty();
        let menu = controller.menu();

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

        let showing_project_form = controller.project_form_open();
        if controller.confirm_delete {
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
                        .child(self.button(
                            "cancel-thread-delete",
                            "Cancel",
                            ThreadAction::CancelDelete,
                            true,
                            p,
                            cx,
                        ))
                        .child(self.button(
                            "confirm-thread-delete",
                            "Delete thread",
                            ThreadAction::Delete,
                            state.connected && !show_stop,
                            p,
                            cx,
                        )),
                );
        } else if showing_project_form {
            let proj = &controller.project;
            let can_confirm = state.connected
                && !state.loading
                && !state.mode_locked()
                && validate_project_form(&proj.name, &proj.folder, proj.place.as_deref()).is_ok();
            let known_home = proj.browse_home.as_deref();
            let places = place_suggestions(state, known_home);
            let mut place_row = div().flex().flex_wrap().gap(px(6.));
            for place in places {
                let id = SharedString::from(format!("project-place-btn-{}", place.value));
                place_row = place_row.child(self.button(
                    id,
                    place.label,
                    ThreadAction::PickProjectPlace(place.value),
                    true,
                    p,
                    cx,
                ));
            }
            body = body
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(16.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Make this a project"),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(18.))
                                .text_color(rgb(p.muted))
                                .child("Creates the folder if missing, or uses an existing folder without changing anything inside it."),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(px(11.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(p.muted))
                                .child("Name"),
                        )
                        .child(
                            div()
                                .relative()
                                .w_full()
                                .px(px(8.))
                                .py(px(4.))
                                .rounded(px(8.))
                                .bg(rgb(p.surface))
                                .border_1()
                                .border_color(rgb(p.line))
                                .child(self.project_name_input.clone())
                                .child(tracked(&self.frame, "project-name".into(), true)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(p.muted))
                                        .child("Folder"),
                                )
                                .child(self.button(
                                    "toggle-project-browse",
                                    if proj.browse_open {
                                        "Hide folders"
                                    } else {
                                        "Choose folder…"
                                    },
                                    ThreadAction::ToggleProjectBrowse,
                                    state.connected,
                                    p,
                                    cx,
                                )),
                        )
                        .child(
                            div()
                                .relative()
                                .w_full()
                                .px(px(8.))
                                .py(px(4.))
                                .rounded(px(8.))
                                .bg(rgb(p.surface))
                                .border_1()
                                .border_color(rgb(p.line))
                                .child(self.project_folder_input.clone())
                                .child(tracked(&self.frame, "project-folder".into(), true)),
                        )
                        .child(place_row),
                );
            if proj.browse_open {
                let mut picker = div()
                    .w_full()
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .flex_col()
                    .gap(px(6.));
                if let Some(browse_dir) = proj.browse_dir.clone() {
                    let shown_dir = display_folder_path(&browse_dir, known_home);
                    picker = picker.child(
                        div()
                            .font_family("IBM Plex Mono")
                            .text_size(px(11.))
                            .text_color(rgb(p.muted))
                            .text_ellipsis()
                            .child(shown_dir.clone()),
                    );
                    let mut use_row = div().flex().flex_wrap().gap(px(6.)).child(self.button(
                        "project-use-exact",
                        format!("Use {shown_dir}"),
                        ThreadAction::UseProjectFolder(browse_dir.clone()),
                        true,
                        p,
                        cx,
                    ));
                    let trimmed_name = proj.name.trim();
                    let with_name_label = if trimmed_name.is_empty() {
                        format!("Put project in {shown_dir}")
                    } else {
                        format!("Use {}", join_project_place(&shown_dir, trimmed_name))
                    };
                    use_row = use_row.child(self.button(
                        "project-use-with-name",
                        with_name_label,
                        ThreadAction::PickProjectPlace(browse_dir),
                        true,
                        p,
                        cx,
                    ));
                    picker = picker.child(use_row);
                    if let Some(parent) = proj.browse_parent.clone() {
                        picker = picker.child(
                            self.button(
                                "project-dir-parent",
                                ".. (up one folder)",
                                ThreadAction::BrowseProjectDir(parent),
                                true,
                                p,
                                cx,
                            )
                            .w_full(),
                        );
                    }
                    for (ix, entry) in proj.browse_entries.iter().enumerate() {
                        picker = picker.child(
                            self.button(
                                SharedString::from(format!("project-dir-entry-{ix}")),
                                format!("{}/", entry.name),
                                ThreadAction::BrowseProjectDir(entry.path.clone()),
                                true,
                                p,
                                cx,
                            )
                            .w_full(),
                        );
                    }
                } else if proj.browse_loading {
                    picker = picker.child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgb(p.muted))
                            .child("Loading folders…"),
                    );
                }
                body = body.child(picker);
            }
            if let Some(err) = proj.error.clone().or_else(|| state.error.clone()) {
                body = body.child(
                    div()
                        .text_size(px(11.5))
                        .line_height(px(17.))
                        .text_color(rgb(p.focus))
                        .child(err),
                );
            }
            body = body.child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(self.button(
                        "cancel-make-project-btn",
                        "Cancel",
                        ThreadAction::CancelMakeProject,
                        true,
                        p,
                        cx,
                    ))
                    .child(
                        self.button(
                            "confirm-make-project-btn",
                            if state.loading {
                                "Making project…"
                            } else {
                                "Make project"
                            },
                            ThreadAction::ConfirmMakeProject,
                            can_confirm,
                            p,
                            cx,
                        )
                        .when(can_confirm, |b| {
                            b.bg(rgb(p.accent))
                                .border_color(rgb(p.accent))
                                .text_color(rgb(p.on_accent))
                                .font_weight(FontWeight::SEMIBOLD)
                        }),
                    ),
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
                                .child("Nothing leaves your machine until you choose. You can change this later from the drawer."),
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
                                .child("Start right away with no account. Requests go to OpenCode's makers."),
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
                                .child("Opens OpenCode's sign-in in a terminal. CrowbarOS keeps no credentials."),
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
                            .child("Sign-in opened in a terminal. New Models will appear here as soon as it ends."),
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
                    .child(self.button(
                        "new-agent-thread",
                        "New thread",
                        ThreadAction::New,
                        free,
                        p,
                        cx,
                    )),
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
                match &turn.context {
                    Some(ctx) => {
                        let open =
                            turn_index == 0 || controller.expanded_context.contains(&turn.turn);
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
                        (agent::TurnState::Stopped, Some(elapsed)) => {
                            format!("Stopped after {elapsed}  ·  {acts}")
                        }
                        (_, Some(elapsed)) => format!("Worked for {elapsed}  ·  {acts}"),
                        (_, None) => acts.clone(),
                    };
                    turn_col = turn_col.child(self.toggle(
                        label,
                        controller.opened_turns.contains(&turn.turn),
                        ThreadAction::Acts(turn.turn),
                        p.muted,
                        p,
                        cx,
                    ));
                }
                if !turn.acts.is_empty()
                    && (working || controller.opened_turns.contains(&turn.turn))
                {
                    let mut list = div()
                        .w_full()
                        .pl(px(10.))
                        .border_l_2()
                        .border_color(rgb(p.line))
                        .flex()
                        .flex_col()
                        .gap(px(3.));
                    for act in &turn.acts {
                        list = list.child(self.act_row(&controller, &turn.state, act, p, cx));
                    }
                    turn_col = turn_col.child(list);
                }
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
                    turn_col =
                        turn_col.child(self.subagent_section(&controller, turn, sub, p, cx));
                }
                for req in &turn.requests {
                    let hinted = first_waiting.as_deref() == Some(req.id.as_str());
                    let sub_title = req.subagent.as_deref().and_then(|sid| {
                        turn.subagents
                            .iter()
                            .find(|s| s.id == sid)
                            .map(|s| s.title.as_str())
                    });
                    turn_col =
                        turn_col.child(self.request_card(&turn.state, req, sub_title, hinted, p, cx));
                }
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
                    turn_col = turn_col.child(
                        div()
                            .text_size(px(11.5))
                            .text_color(rgb(p.muted))
                            .child(ended),
                    );
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
                for queued in &turn.queued {
                    let waits = queued.state == "queued";
                    let mut row = div()
                        .w_full()
                        .flex()
                        .flex_col()
                        .items_end()
                        .gap(px(4.))
                        .opacity(0.7)
                        .child(message_bubble(queued.text.clone(), p))
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(rgb(p.muted))
                                .child(if waits {
                                    "Waiting for the turn to end"
                                } else {
                                    "Not sent: the turn was stopped"
                                }),
                        );
                    if waits {
                        row = row.child(self.button(
                            SharedString::from(format!("unqueue-{}", queued.id)),
                            "Take back",
                            ThreadAction::Unqueue(queued.id.clone()),
                            true,
                            p,
                            cx,
                        ));
                    }
                    turn_col = turn_col.child(row);
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
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
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

        let in_thread =
            !needs_choice && !state.list_view && !controller.confirm_delete && !showing_project_form;
        let show_newest = in_thread && !state.turns.is_empty() && !self.near_bottom();
        let error = if showing_project_form {
            None
        } else {
            state.error.clone().or_else(|| {
                if state.turns.is_empty() {
                    None
                } else if !state.configured || !state.connected {
                    Some("The Agent isn't running right now. Reconnecting automatically.".into())
                } else if state.no_harness {
                    None
                } else {
                    state.missing_model.clone()
                }
            })
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context("AgentSurface")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &Next, w, _| w.focus_next()))
            .on_action(cx.listener(|_, _: &Previous, w, _| w.focus_prev()))
            .on_action(cx.listener(|this, _: &composer::Up, w, cx| {
                this.dispatch_intent(AgentIntent::StepMenu(-1), w, cx);
            }))
            .on_action(cx.listener(|this, _: &composer::Down, w, cx| {
                this.dispatch_intent(AgentIntent::StepMenu(1), w, cx);
            }))
            .on_action(cx.listener(|this, _: &composer::Left, w, cx| {
                this.dispatch_intent(AgentIntent::StepProvider(-1), w, cx);
            }))
            .on_action(cx.listener(|this, _: &composer::Right, w, cx| {
                this.dispatch_intent(AgentIntent::StepProvider(1), w, cx);
            }))
            .on_action(cx.listener(|this, _: &composer::Take, w, cx| {
                this.dispatch_intent(AgentIntent::TakeMenuRow, w, cx);
            }))
            .on_action(cx.listener(|this, _: &composer::Dismiss, w, cx| {
                this.dispatch_intent(AgentIntent::Dismiss, w, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, w, cx| {
                let key = event.keystroke.key.as_str();
                let (needs_choice, form_open, list_view, confirm_delete) = {
                    let a = &this.model.read(cx).agent;
                    (
                        a.state.needs_model_choice(),
                        a.project_form_open(),
                        a.state.list_view,
                        a.confirm_delete,
                    )
                };
                if needs_choice {
                    return;
                }
                if form_open {
                    if key == "escape" {
                        cx.stop_propagation();
                        this.action(ThreadAction::CancelMakeProject, w, cx);
                        return;
                    }
                    let in_project_input = this.project_name_input.focus_handle(cx).is_focused(w)
                        || this.project_folder_input.focus_handle(cx).is_focused(w);
                    if key == "enter"
                        && should_take_composer_enter(
                            event.is_held,
                            &event.keystroke.modifiers,
                            in_project_input,
                        )
                    {
                        cx.stop_propagation();
                        this.action(ThreadAction::ConfirmMakeProject, w, cx);
                        return;
                    }
                }
                if key == "enter"
                    && should_take_composer_enter(
                        false,
                        &event.keystroke.modifiers,
                        this.input.focus_handle(cx).is_focused(w),
                    )
                {
                    cx.stop_propagation();
                    if should_take_composer_enter(event.is_held, &event.keystroke.modifiers, true) {
                        this.dispatch_intent(AgentIntent::Enter, w, cx);
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
                    && let Some(digit) = key
                        .chars()
                        .next()
                        .and_then(|c| c.to_digit(10))
                        .filter(|d| *d >= 1)
                {
                    if should_answer_with_alt_digit(
                        event.is_held,
                        list_view,
                        confirm_delete || form_open,
                    ) && this.dispatch_intent(
                        AgentIntent::AnswerWaiting(digit as usize - 1),
                        w,
                        cx,
                    ) {
                        cx.stop_propagation();
                    }
                } else if key == "escape" && confirm_delete {
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
                let folder_label = state
                    .cwd
                    .as_deref()
                    .map(|cwd| display_folder_path(cwd, controller.project.browse_home.as_deref()));
                let can_project =
                    state.thread.is_some() && state.connected && !state.mode_locked();
                root.child(
                    div()
                        .flex_none()
                        .min_h(px(36.))
                        .px(px(12.))
                        .py(px(6.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
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
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.action(ThreadAction::List, w, cx)
                                })),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .w_full()
                                        .text_size(px(12.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_ellipsis()
                                        .child(
                                            state
                                                .title
                                                .clone()
                                                .unwrap_or_else(|| "New thread".into()),
                                        ),
                                )
                                .when_some(folder_label, |d, folder| {
                                    d.child(
                                        div()
                                            .w_full()
                                            .font_family("IBM Plex Mono")
                                            .text_size(px(10.))
                                            .text_color(rgb(p.muted))
                                            .text_ellipsis()
                                            .child(folder),
                                    )
                                }),
                        )
                        .when(state.thread.is_some(), |row| {
                            row.child(
                                div()
                                    .id("agent-make-project")
                                    .relative()
                                    .flex_none()
                                    .h(px(24.))
                                    .px(px(8.))
                                    .rounded(px(6.))
                                    .border_1()
                                    .border_color(rgb(p.line))
                                    .flex()
                                    .items_center()
                                    .text_size(px(11.))
                                    .text_color(rgb(p.text))
                                    .when(!can_project, |s| s.opacity(0.45))
                                    .when(can_project, |s| {
                                        s.cursor_pointer().hover(move |s| s.bg(rgb(p.surface)))
                                    })
                                    .child("Make this a project")
                                    .child(tracked(&self.frame, "make-project".into(), false))
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        if can_project {
                                            this.action(ThreadAction::AskMakeProject, w, cx);
                                        }
                                    })),
                            )
                        }),
                )
            })
            .child({
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
                        .when_some(menu.as_ref(), |d, menu| {
                            d.child(self.list_panel(&controller, menu, p, cx))
                        })
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
                                                this.dispatch_intent(AgentIntent::StopTurn, w, cx);
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
                                            .when(!can_send, |btn| {
                                                btn.child(icon("arrow", p.muted, 14.))
                                            })
                                            .child(tracked(&self.frame, "send".into(), false))
                                            .on_click(cx.listener(|this, _, w, cx| {
                                                this.dispatch_intent(AgentIntent::Enter, w, cx);
                                                w.focus(&this.input.focus_handle(cx));
                                            })),
                                    )
                                }),
                        )
                        .child(self.status_line(state, p)),
                )
            })
            .child({
                let frame = self.frame.clone();
                let left = self.left;
                let output = self.output.clone();
                let list = controller.list_snapshot(menu.as_ref());
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
