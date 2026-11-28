//! The launcher's keyboard-first companions: volatile history and characters.
use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    History,
    Characters,
}
pub struct Picker {
    model: Entity<Model>,
    kind: Kind,
    input: Entity<input::TextInput>,
    clear_focus: FocusHandle,
    query: String,
    selected: usize,
    items: Vec<String>,
    loading: bool,
    reload: bool,
    error: bool,
    _subscription: Subscription,
}
struct Row {
    text: String,
    label: String,
    detail: String,
}
impl Picker {
    fn rows(&self) -> Vec<Row> {
        match self.kind {
            Kind::History => self
                .items
                .iter()
                .filter(|s| s.to_lowercase().contains(&self.query.to_lowercase()))
                .map(|s| Row {
                    text: s.clone(),
                    label: s
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(160)
                        .collect(),
                    detail: format!("{} characters", s.chars().count()),
                })
                .collect(),
            Kind::Characters => symbols::search(&self.query)
                .into_iter()
                .map(|s| Row {
                    text: s.glyph.into(),
                    label: s.name.into(),
                    detail: s.category.into(),
                })
                .collect(),
        }
    }
    fn close(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        self.items.clear();
        self.model.update(cx, |m, _| m.picker = None);
        w.remove_window();
        log::info!("SHELL picker open=false");
    }
    fn step(&mut self, delta: i32, w: &mut Window, cx: &mut Context<Self>) {
        if self.input.read(cx).is_composing() {
            return;
        }
        w.focus(&self.input.focus_handle(cx));
        let len = self.rows().len();
        if len > 0 {
            self.selected = (self.selected as i32 + delta).rem_euclid(len as i32) as usize;
        }
        cx.notify();
    }
    fn copy(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        if self.input.read(cx).is_composing() {
            return;
        }
        if self.clear_focus.is_focused(w) {
            self.load(true, w, cx);
            return;
        }
        if self.loading {
            return;
        }
        if let Some(row) = self.rows().get(self.selected) {
            cx.write_to_clipboard(ClipboardItem::new_string(row.text.clone()));
            self.close(w, cx);
        }
    }
    pub fn load(&mut self, clear: bool, w: &mut Window, cx: &mut Context<Self>) {
        if self.kind != Kind::History {
            return;
        }
        if self.loading {
            self.reload |= !clear;
            return;
        }
        self.loading = true;
        self.error = false;
        if clear {
            self.items.clear();
            self.selected = 0;
        }
        let task = cx
            .background_executor()
            .spawn(async move { session::clipboard_history(clear) });
        cx.spawn_in(w, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, w, cx| {
                this.loading = false;
                match result {
                    Ok(items) => this.items = items,
                    Err(_) => this.error = true,
                }
                log::info!("SHELL picker history loaded={} error={}", this.items.len(), this.error);
                this.selected = this.selected.min(this.rows().len().saturating_sub(1));
                if std::mem::take(&mut this.reload) {
                    this.load(false, w, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
impl Render for Picker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.model.read(cx).palette();
        self.input.update(cx, |input, _| input.palette = p);
        let history = self.kind == Kind::History;
        let loading = self.loading;
        let clearing = self.clear_focus.is_focused(window);
        let rows = self.rows();
        let total = rows.len();
        // Keep search and Clear visible at large display scales, too.
        let reserved = if history { 272. } else { 232. };
        let page_size = ((f32::from(window.viewport_size().height) - reserved) / 44.)
            .floor()
            .clamp(1., 6.) as usize;
        let page = self.selected / page_size * page_size;
        div()
            .id("picker-backdrop")
            .size_full()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(56.))
            .bg(rgba(0x080d1280))
            .font_family("Manrope")
            .text_size(px(13.))
            .text_color(rgb(p.text))
            .key_context("ClipboardPicker")
            .on_action(cx.listener(|this, _: &Next, w, cx| this.step(1, w, cx)))
            .on_action(cx.listener(|this, _: &Previous, w, cx| this.step(-1, w, cx)))
            .on_action(cx.listener(|this, _: &Open, w, cx| this.copy(w, cx)))
            .on_action(cx.listener(|this, _: &Close, w, cx| this.close(w, cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, w, cx| {
                if event.keystroke.key == "space" && this.clear_focus.is_focused(w) {
                    this.load(true, w, cx);
                    cx.stop_propagation();
                }
                if event.keystroke.key == "tab" {
                    if this.kind == Kind::History && !this.clear_focus.is_focused(w) {
                        w.focus(&this.clear_focus);
                    } else {
                        w.focus(&this.input.focus_handle(cx));
                    }
                    cx.stop_propagation();
                }
            }))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, w, cx| this.close(w, cx)))
            .child(
                div()
                    .id("picker-panel")
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
                            .px(px(16.))
                            .pt(px(16.))
                            .pb(px(12.))
                            .flex()
                            .justify_between()
                            .items_center()
                            .child(
                                div()
                                    .text_size(px(18.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if history {
                                        "Clipboard history"
                                    } else {
                                        "Characters & emoji"
                                    }),
                            )
                            .child(palette_keycap(if history { "Super+V" } else { "Super+." }, p)),
                    )
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
                            .child(div().flex_1().min_w_0().child(self.input.clone())),
                    )
                    .child(
                        div()
                            .min_h(px(page_size as f32 * 44. + 16.))
                            .px(px(8.))
                            .py(px(8.))
                            .flex()
                            .flex_col()
                            .when(total == 0, |d| {
                                d.child(
                                    div()
                                        .h(px(page_size as f32 * 44.))
                                        .flex()
                                        .flex_col()
                                        .justify_center()
                                        .items_center()
                                        .gap(px(8.))
                                        .child(if self.loading {
                                            "Reading clipboard history…"
                                        } else if self.error {
                                            "Could not read clipboard history"
                                        } else if !self.query.is_empty() {
                                            "No matches"
                                        } else {
                                            "Nothing copied yet"
                                        })
                                        .child(div().text_color(rgb(p.muted)).child(if self.error {
                                            "Close and reopen to try again."
                                        } else if !self.query.is_empty() {
                                            "Try another name or keyword."
                                        } else {
                                            "Copy text in an app to keep it here."
                                        })),
                                )
                            })
                            .children(rows.into_iter().enumerate().skip(page).take(page_size).map(|(i, row)| {
                                let selected = i == self.selected;
                                div()
                                    .id(("picker-row", i))
                                    .h(px(44.))
                                    .px(px(10.))
                                    .flex()
                                    .gap(px(12.))
                                    .items_center()
                                    .rounded(px(6.))
                                    .bg(rgb(if selected { p.surface } else { p.canvas }))
                                    .border_1()
                                    .border_color(rgb(if selected { p.focus } else { p.canvas }))
                                    .hover(move |s| s.bg(rgb(p.surface)))
                                    .cursor_pointer()
                                    .when(!history, |d| {
                                        d.child(
                                            div()
                                                .w(px(34.))
                                                .text_size(px(24.))
                                                .when(row.detail == "Emoji", |d| d.font_family("Noto Color Emoji"))
                                                .child(row.text.clone()),
                                        )
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(row.label),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_size(px(11.))
                                            .text_color(rgb(p.muted))
                                            .child(row.detail),
                                    )
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.selected = i;
                                        w.focus(&this.input.focus_handle(cx));
                                        this.copy(w, cx);
                                    }))
                            })),
                    )
                    .child(
                        div()
                            .px(px(16.))
                            .py(px(12.))
                            .border_t_1()
                            .border_color(rgb(p.line))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(12.))
                            .child(div().text_size(px(11.)).text_color(rgb(p.muted)).child(if total == 0 {
                                "0 results".into()
                            } else {
                                format!("{} of {total}", self.selected + 1)
                            }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .text_size(px(11.))
                                    .text_color(rgb(p.muted))
                                    .child(palette_keycap("↑ ↓", p))
                                    .child("choose")
                                    .child(palette_keycap("Enter", p))
                                    .child(if clearing { "clear" } else { "copy" })
                                    .child(palette_keycap("Esc", p))
                                    .child("close"),
                            ),
                    )
                    .when(history, |d| {
                        d.child(
                            div()
                                .px(px(16.))
                                .pb(px(12.))
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(rgb(p.muted))
                                        .child("Kept in memory · clears on lock"),
                                )
                                .child(
                                    div()
                                        .id("clear-history")
                                        .opacity(if loading { 0.45 } else { 1. })
                                        .track_focus(&self.clear_focus)
                                        .px(px(10.))
                                        .py(px(5.))
                                        .rounded(px(6.))
                                        .border_1()
                                        .border_color(rgb(p.line))
                                        .cursor_pointer()
                                        .focus(move |s| s.border_color(rgb(p.focus)))
                                        .hover(move |s| s.bg(rgb(p.surface)))
                                        .child("Clear")
                                        .on_click(cx.listener(|this, _, w, cx| this.load(true, w, cx))),
                                ),
                        )
                    }),
            )
    }
}
pub fn close(model: &Entity<Model>, cx: &mut App) {
    if let Some(w) = model.update(cx, |m, _| m.picker.take()) {
        let _ = w.update(cx, |this, w, _| {
            this.items.clear();
            w.remove_window();
        });
        log::info!("SHELL picker open=false");
    }
}
pub fn toggle(model: Entity<Model>, kind: Kind, cx: &mut App) {
    if model.read(cx).snapshot.locked {
        return;
    }
    if let Some(w) = model.read(cx).picker {
        let same = w.read(cx).is_ok_and(|view| view.kind == kind);
        close(&model, cx);
        if same {
            return;
        }
    }
    close_launcher(&model, cx);
    close_quick_settings(&model, cx);
    close_calendar(&model, cx);
    theme_switcher::close(&model, cx);
    let view_model = model.clone();
    let opened = cx.open_window(
        layer(
            "picker",
            Layer::Overlay,
            Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            0.,
            0.,
            Some(-1.),
            KeyboardInteractivity::Exclusive,
        ),
        move |w, cx| {
            let input = cx.new(|cx| {
                input::TextInput::with_placeholder(
                    if kind == Kind::History {
                        "Search copied text…"
                    } else {
                        "Search names or keywords…"
                    },
                    256,
                    cx,
                )
            });
            w.focus(&input.focus_handle(cx));
            cx.new(|cx| {
                let subscription = cx.observe(&input, |this: &mut Picker, input, cx| {
                    let query = input.read(cx).query();
                    if this.query != query {
                        this.query = query;
                        this.selected = 0;
                        cx.notify();
                    }
                });
                let mut picker = Picker {
                    model: view_model,
                    kind,
                    input,
                    clear_focus: cx.focus_handle(),
                    query: String::new(),
                    selected: 0,
                    items: Vec::new(),
                    loading: false,
                    reload: false,
                    error: false,
                    _subscription: subscription,
                };
                picker.load(false, w, cx);
                picker
            })
        },
    );
    match opened {
        Ok(w) => {
            model.update(cx, |m, _| m.picker = Some(w));
            log::info!("SHELL picker open=true kind={kind:?}");
        }
        Err(error) => log::error!("Opening picker: {error}"),
    }
}
