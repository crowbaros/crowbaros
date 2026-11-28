// SPDX-License-Identifier: MIT
//! Installed-application search and inline calculator launcher overlay.

use gpui::{layer_shell::*, prelude::*, *};

use crate::{
    catalog::{self, AppEntry},
    input,
    ui::{
        ActivePopover, Close, Model, Next, Open, Previous, clear_active_popover_if,
        close_active_popover, icon, layer, palette_keycap,
    },
};

pub struct Launcher {
    model: Entity<Model>,
    input: Entity<input::TextInput>,
    apps: Vec<AppEntry>,
    query: String,
    selected: usize,
    pub error: Option<String>,
    pub busy: bool,
    _subscription: Subscription,
}

impl Launcher {
    fn results(&self) -> Vec<AppEntry> {
        if crowbar_calc::launcher_expression(&self.query) {
            return Vec::new();
        }
        catalog::ranked(&self.apps, &self.query)
            .into_iter()
            .map(|i| self.apps[i].clone())
            .collect()
    }

    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        clear_active_popover_if(
            &self.model,
            |p| matches!(p, ActivePopover::Launcher(_)),
            cx,
        );
        window.remove_window();
    }

    fn step(&mut self, delta: i32, cx: &mut Context<Self>) {
        let len = self.results().len();
        if len > 0 {
            self.selected = (self.selected as i32 + delta).rem_euclid(len as i32) as usize;
        }
        cx.notify();
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if let Some(result) = crowbar_calc::launcher_result(&self.query) {
            if let Ok(value) = result {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(value.to_string()));
                self.close(window, cx);
            }
            return;
        }
        if let Some(app) = self.results().get(self.selected) {
            let m = self.model.read(cx);
            if let Some(client) = m
                .snapshot
                .clients
                .iter()
                .find(|c| app.matches_class(&c.class))
            {
                let address = client.address.clone();
                let compositor = m.compositor.clone();
                self.close(window, cx);
                compositor.focus_window(address);
            } else {
                let compositor = m.compositor.clone();
                let launcher_client = m.launcher_client.clone();
                let tx = m.tx.clone();
                self.busy = true;
                self.error = None;
                launcher_client.launch(app.path.clone(), compositor, tx);
                cx.notify();
            }
        }
    }
}

impl Render for Launcher {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.model.read(cx).palette();
        let calculation = crowbar_calc::launcher_result(&self.query);
        let is_math = calculation.is_some();
        let math_ready = matches!(calculation, Some(Ok(_)));
        let results = self.results();
        let total = results.len();
        let selected = self.selected;
        let offset = (selected / 6) * 6;
        let busy = self.busy;
        self.input.update(cx, |input, _| input.palette = p);
        div()
            .id("launcher-backdrop")
            .size_full()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(56.))
            .bg(rgba(0x080d1280))
            .font_family("Manrope")
            .text_color(rgb(p.text))
            .text_size(px(13.))
            .key_context("Launcher")
            .on_action(cx.listener(|this, _: &Next, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &Previous, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &Open, w, cx| this.open(w, cx)))
            .on_action(cx.listener(|this, _: &Close, w, cx| this.close(w, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, w, cx| this.close(w, cx)),
            )
            .child(
                div()
                    .id("launcher-panel")
                    .w(px(520.))
                    .mx(px(24.))
                    .flex()
                    .flex_col()
                    .rounded(px(12.))
                    .bg(rgb(p.canvas))
                    .border_1()
                    .border_color(rgb(p.line))
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .h(px(46.))
                            .px(px(16.))
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .border_b_1()
                            .border_color(rgb(p.line))
                            .child(icon("search", p.muted, 18.))
                            .child(div().flex_1().min_w_0().child(self.input.clone()))
                            .child(
                                div()
                                    .id("close-launcher")
                                    .size(px(26.))
                                    .rounded(px(6.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(rgb(p.surface)))
                                    .child(icon("close", p.muted, 14.))
                                    .on_click(cx.listener(|this, _, w, cx| this.close(w, cx))),
                            ),
                    )
                    .child(
                        div()
                            .py(px(6.))
                            .px(px(8.))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .when_some(calculation, |d, result| {
                                let valid = result.is_ok();
                                let text = result
                                    .map(|v| v.to_string())
                                    .unwrap_or_else(|e| e.to_string());
                                d.child(
                                    div()
                                        .id("math-result")
                                        .px(px(12.))
                                        .py(px(16.))
                                        .rounded(px(7.))
                                        .bg(rgb(p.surface))
                                        .border_1()
                                        .border_color(rgb(p.focus))
                                        .child(
                                            div()
                                                .font_family("IBM Plex Mono")
                                                .text_size(px(if valid { 24. } else { 13. }))
                                                .text_ellipsis()
                                                .child(text),
                                        )
                                        .child(
                                            div()
                                                .mt(px(6.))
                                                .text_size(px(12.))
                                                .text_color(rgb(p.muted))
                                                .child(if valid {
                                                    "Enter to copy result"
                                                } else {
                                                    "Edit the expression to try again"
                                                }),
                                        )
                                        .when(valid, |d| {
                                            d.cursor_pointer().on_click(
                                                cx.listener(|this, _, w, cx| this.open(w, cx)),
                                            )
                                        }),
                                )
                            })
                            .when(total == 0 && !is_math, |d| {
                                d.child(
                                    div()
                                        .px(px(12.))
                                        .py(px(20.))
                                        .child(
                                            div()
                                                .text_size(px(14.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child("No applications found"),
                                        )
                                        .child(
                                            div()
                                                .mt(px(4.))
                                                .text_size(px(12.))
                                                .text_color(rgb(p.muted))
                                                .child("Try a shorter name or clear your search."),
                                        ),
                                )
                            })
                            .children(results.into_iter().enumerate().skip(offset).take(6).map(
                                |(index, app)| {
                                    let is_selected = index == selected;
                                    div()
                                        .id(("result", index))
                                        .relative()
                                        .h(px(44.))
                                        .pl(px(10.))
                                        .pr(px(12.))
                                        .flex()
                                        .items_center()
                                        .gap(px(10.))
                                        .rounded(px(7.))
                                        .overflow_hidden()
                                        .bg(rgb(if is_selected {
                                            p.surface
                                        } else {
                                            p.canvas
                                        }))
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(rgb(p.surface)))
                                        .when(is_selected, |d| {
                                            d.child(
                                                div()
                                                    .absolute()
                                                    .left_0()
                                                    .top(px(6.))
                                                    .bottom(px(6.))
                                                    .w(px(2.))
                                                    .rounded_full()
                                                    .bg(rgb(p.focus)),
                                            )
                                        })
                                        .child(
                                            div()
                                                .size(px(28.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .child(icon(
                                                    app.icon(),
                                                    if is_selected { p.focus } else { p.muted },
                                                    20.,
                                                )),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .flex()
                                                .flex_col()
                                                .justify_center()
                                                .child(
                                                    div()
                                                        .text_size(px(13.))
                                                        .line_height(px(17.))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .text_color(rgb(p.text))
                                                        .text_ellipsis()
                                                        .child(app.name),
                                                )
                                                .when(!app.description.is_empty(), |d| {
                                                    d.child(
                                                        div()
                                                            .text_size(px(11.5))
                                                            .line_height(px(15.))
                                                            .text_color(rgb(p.muted))
                                                            .text_ellipsis()
                                                            .child(app.description),
                                                    )
                                                }),
                                        )
                                        .when(is_selected, |d| {
                                            d.child(icon("arrow", p.focus, 15.))
                                        })
                                        .on_mouse_move(cx.listener(move |this, _, _, cx| {
                                            if this.selected != index {
                                                this.selected = index;
                                                cx.notify();
                                            }
                                        }))
                                        .on_click(cx.listener(move |this, _, w, cx| {
                                            this.selected = index;
                                            this.open(w, cx)
                                        }))
                                },
                            )),
                    )
                    .when(self.error.is_some(), |d| {
                        d.child(
                            div()
                                .px(px(16.))
                                .py(px(6.))
                                .text_size(px(11.5))
                                .text_color(rgb(p.text))
                                .child(self.error.clone().unwrap_or_default()),
                        )
                    })
                    .child(
                        div()
                            .h(px(34.))
                            .px(px(16.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_t_1()
                            .border_color(rgb(p.line))
                            .text_size(px(11.))
                            .text_color(rgb(p.muted))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .child(if busy {
                                        "Opening application…".into()
                                    } else if is_math {
                                        "Calculator".into()
                                    } else if total > 6 {
                                        format!(
                                            "{}–{} of {total}",
                                            offset + 1,
                                            (offset + 6).min(total)
                                        )
                                    } else if total == 1 {
                                        "1 application".into()
                                    } else {
                                        format!("{total} applications")
                                    })
                                    .when(total > 6, |d| {
                                        d.child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(6.))
                                                .child(
                                                    div()
                                                        .id("previous-page")
                                                        .cursor_pointer()
                                                        .px(px(4.))
                                                        .py(px(2.))
                                                        .rounded(px(4.))
                                                        .hover(move |s| s.bg(rgb(p.surface)))
                                                        .child("Prev")
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.selected =
                                                                this.selected.saturating_sub(6);
                                                            cx.notify();
                                                        })),
                                                )
                                                .child(
                                                    div()
                                                        .id("next-page")
                                                        .cursor_pointer()
                                                        .px(px(4.))
                                                        .py(px(2.))
                                                        .rounded(px(4.))
                                                        .hover(move |s| s.bg(rgb(p.surface)))
                                                        .child("Next")
                                                        .on_click(cx.listener(
                                                            move |this, _, _, cx| {
                                                                this.selected = (this.selected + 6)
                                                                    .min(total - 1);
                                                                cx.notify();
                                                            },
                                                        )),
                                                ),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(10.))
                                    .when(!is_math, |d| {
                                        d.child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(4.))
                                                .child(palette_keycap("↑ ↓", p))
                                                .child("choose"),
                                        )
                                    })
                                    .when(!is_math || math_ready, |d| {
                                        d.child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(4.))
                                                .child(palette_keycap("Enter", p))
                                                .child(if is_math { "copy" } else { "open" }),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(4.))
                                            .child(palette_keycap("Esc", p))
                                            .child("close"),
                                    ),
                            ),
                    ),
            )
    }
}

pub fn toggle(model: Entity<Model>, cx: &mut App) {
    if model.read(cx).is_launcher_open() {
        log::info!("SHELL launcher open=false");
        close_active_popover(&model, cx);
        return;
    }
    close_active_popover(&model, cx);
    log::info!("SHELL launcher open=true");
    let apps = catalog::discover();
    model.update(cx, |m, _| m.apps = apps.clone());
    let model2 = model.clone();
    let result = cx.open_window(
        layer(
            "launcher",
            Layer::Overlay,
            Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            0.,
            0.,
            Some(-1.),
            KeyboardInteractivity::Exclusive,
        ),
        move |w, cx| {
            let input = cx.new(input::TextInput::new);
            w.focus(&input.focus_handle(cx));
            cx.new(|cx| {
                let subscription = cx.observe(&input, |this: &mut Launcher, input, cx| {
                    let query = input.read(cx).query();
                    if query != this.query {
                        this.query = query;
                        this.selected = 0;
                        this.error = None;
                        cx.notify();
                    }
                });
                Launcher {
                    model: model2,
                    input,
                    apps,
                    query: String::new(),
                    selected: 0,
                    error: None,
                    busy: false,
                    _subscription: subscription,
                }
            })
        },
    );
    match result {
        Ok(w) => model.update(cx, |m, _| {
            m.active_popover = Some(ActivePopover::Launcher(w));
        }),
        Err(e) => log::error!("Opening launcher: {e}"),
    }
}
