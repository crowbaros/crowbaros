// SPDX-License-Identifier: MIT
//! Bar clock month calendar and quick timer popover.

use gpui::{layer_shell::*, prelude::*, *};

use super::{CalendarMonth, TIMER_PRESETS_SECS, WEEKDAY_HEADERS, month_grid};
use crate::ui::{
    ActivePopover, BAR_HEIGHT, Close, Model, NextMonth, PrevMonth, clear_active_popover_if,
    close_active_popover, layer,
};

pub struct CalendarPopover {
    model: Entity<Model>,
    focus: FocusHandle,
    month: CalendarMonth,
}

impl CalendarPopover {
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.read(cx).is_calendar_open() {
            log::info!("SHELL calendar open=false");
        }
        clear_active_popover_if(
            &self.model,
            |p| matches!(p, ActivePopover::Calendar(_)),
            cx,
        );
        window.remove_window();
        cx.refresh_windows();
    }
}

impl Render for CalendarPopover {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let timer = m.timer.clone();
        let today = chrono::Local::now().date_naive();
        let cells = month_grid(self.month.year, self.month.month, today);

        let mut grid = div().flex().flex_col().gap(px(4.));
        grid = grid.child(div().flex().children(WEEKDAY_HEADERS.into_iter().map(|day| {
            div()
                .flex_1()
                .h(px(22.))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(p.muted))
                .child(day)
        })));
        for week in cells.chunks(7) {
            grid = grid.child(div().flex().gap(px(2.)).children(week.iter().map(|cell| {
                div()
                    .flex_1()
                    .h(px(28.))
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.))
                    .when(cell.is_today, |d| {
                        d.bg(rgb(p.accent))
                            .text_color(rgb(p.on_accent))
                            .font_weight(FontWeight::BOLD)
                    })
                    .when(!cell.is_today, |d| {
                        d.text_color(rgb(if cell.in_month { p.text } else { p.muted }))
                            .when(!cell.in_month, |d| d.opacity(0.45))
                    })
                    .child(cell.day.to_string())
            })));
        }

        let panel = div()
            .id("calendar-panel")
            .w(px(312.))
            .p(px(14.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .rounded(px(12.))
            .bg(rgb(p.canvas))
            .border_1()
            .border_color(rgb(p.line))
            .text_color(rgb(p.text))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .id("cal-prev")
                            .w(px(26.))
                            .h(px(26.))
                            .rounded(px(6.))
                            .bg(rgb(p.surface))
                            .border_1()
                            .border_color(rgb(p.line))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.85))
                            .child("‹")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.month = this.month.prev();
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("cal-today")
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(px(6.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .text_size(px(13.))
                            .font_weight(FontWeight::BOLD)
                            .child(self.month.title())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.month = CalendarMonth::current();
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("cal-next")
                            .w(px(26.))
                            .h(px(26.))
                            .rounded(px(6.))
                            .bg(rgb(p.surface))
                            .border_1()
                            .border_color(rgb(p.line))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.85))
                            .child("›")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.month = this.month.next();
                                cx.notify();
                            })),
                    ),
            )
            .child(grid)
            .child(
                div()
                    .pt(px(10.))
                    .border_t_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(if timer.finished {
                                        "Timer complete"
                                    } else {
                                        "Quick Timer"
                                    }),
                            )
                            .child(
                                div()
                                    .font_family("IBM Plex Mono")
                                    .text_size(px(14.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(rgb(if timer.running || timer.finished {
                                        p.accent
                                    } else {
                                        p.text
                                    }))
                                    .child(timer.formatted()),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(6.))
                            .child(div().flex().gap(px(4.)).children(
                                TIMER_PRESETS_SECS.into_iter().map(|secs| {
                                    let selected = timer.preset_secs == secs;
                                    div()
                                        .id(("timer-preset", secs as usize))
                                        .h(px(24.))
                                        .px(px(6.))
                                        .rounded(px(5.))
                                        .bg(rgb(if selected { p.accent } else { p.surface }))
                                        .text_color(rgb(if selected {
                                            p.on_accent
                                        } else {
                                            p.muted
                                        }))
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .text_size(px(10.5))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(format!("{}m", secs / 60))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.model
                                                .update(cx, |m, _| m.timer.select_preset(secs));
                                            cx.notify();
                                        }))
                                }),
                            ))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(4.))
                                    .child(
                                        div()
                                            .id("timer-toggle")
                                            .h(px(24.))
                                            .px(px(8.))
                                            .rounded(px(5.))
                                            .bg(rgb(p.accent))
                                            .text_color(rgb(p.on_accent))
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .text_size(px(11.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(if timer.running { "Pause" } else { "Start" })
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.model.update(cx, |m, _| m.timer.toggle());
                                                cx.notify();
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id("timer-reset")
                                            .h(px(24.))
                                            .px(px(7.))
                                            .rounded(px(5.))
                                            .bg(rgb(p.surface))
                                            .border_1()
                                            .border_color(rgb(p.line))
                                            .flex()
                                            .items_center()
                                            .cursor_pointer()
                                            .text_size(px(11.))
                                            .child("Reset")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.model.update(cx, |m, _| m.timer.reset());
                                                cx.notify();
                                            })),
                                    ),
                            ),
                    ),
            );

        div()
            .id("calendar-backdrop")
            .key_context("Calendar")
            .track_focus(&self.focus)
            .size_full()
            .pt(px(BAR_HEIGHT + 8.))
            .flex()
            .items_start()
            .justify_center()
            .font_family("Manrope")
            .on_action(cx.listener(|this, _: &Close, w, cx| this.close(w, cx)))
            .on_action(cx.listener(|this, _: &PrevMonth, _, cx| {
                this.month = this.month.prev();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &NextMonth, _, cx| {
                this.month = this.month.next();
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, w, cx| this.close(w, cx)),
            )
            .child(panel)
    }
}

pub fn toggle(model: Entity<Model>, cx: &mut App) {
    if model.read(cx).is_calendar_open() {
        close_active_popover(&model, cx);
        return;
    }
    close_active_popover(&model, cx);
    log::info!("SHELL calendar open=true");
    let model2 = model.clone();
    let result = cx.open_window(
        layer(
            "calendar",
            Layer::Overlay,
            Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            0.,
            0.,
            Some(-1.),
            KeyboardInteractivity::Exclusive,
        ),
        move |w, cx| {
            let focus = cx.focus_handle();
            w.focus(&focus);
            cx.new(|_| CalendarPopover {
                model: model2,
                focus,
                month: CalendarMonth::current(),
            })
        },
    );
    match result {
        Ok(w) => {
            model.update(cx, |m, _| {
                m.active_popover = Some(ActivePopover::Calendar(w));
            });
            cx.refresh_windows();
        }
        Err(e) => log::error!("Opening calendar: {e}"),
    }
}
