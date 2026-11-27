// SPDX-License-Identifier: MIT
use crowbar_calc::{Error, Value};
use crowbar_ui::{ButtonVariant, FONT_MONO, FONT_UI, ThemeWatcher, UiPalette, button, load_fonts, spawn_theme_watcher};
use crowbaros_calc::{Calculator, Page};
use gpui::{
    App, Application, Bounds, ClipboardItem, Context, Entity, FocusHandle, Focusable, KeyBinding, Subscription, Window,
    WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, size,
};
use gpui_component::{
    Root, Theme, ThemeMode,
    input::{Input, InputEvent, InputState},
};

actions!(
    calc,
    [
        Standard, Scientific, Programmer, Evaluate, CopyResult, Clear, Expression, Quit, Tab, BackTab
    ]
);
#[derive(Clone, Copy)]
enum Control {
    Page(Page),
    Width(u8),
    Insert(&'static str),
    Evaluate,
    Copy,
    Clear,
    ClearHistory,
    Reuse(usize),
    Close,
}
struct Calc {
    calculator: Calculator,
    input: Entity<InputState>,
    result: Option<Result<Value, Error>>,
    palette: UiPalette,
    message: String,
    focus: FocusHandle,
    controls: Vec<FocusHandle>,
    _input: Subscription,
}
fn appearance(p: UiPalette, cx: &mut App) {
    Theme::change(if p.is_dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    let t = Theme::global_mut(cx);
    t.font_family = FONT_UI.into();
    t.mono_font_family = FONT_MONO.into();
    t.background = rgb(p.canvas).into();
    t.foreground = rgb(p.text).into();
    t.border = rgb(p.line).into();
    t.ring = rgb(p.focus).into();
    t.caret = rgb(p.focus).into();
    t.input = rgb(p.surface).into();
    t.muted_foreground = rgb(p.muted).into();
}
impl Calc {
    fn new(w: &mut Window, cx: &mut Context<Self>) -> Self {
        w.set_window_title("Calculator");
        let palette = ThemeWatcher::for_user()
            .ok()
            .map(|t| UiPalette::from_active(&t.current()))
            .unwrap_or_default();
        appearance(palette, cx);
        spawn_theme_watcher(cx, |this, active, cx| {
            this.palette = UiPalette::from_active(&active);
            appearance(this.palette, cx);
            cx.notify();
        });
        let input = cx.new(|cx| {
            InputState::new(w, cx)
                .placeholder("Type an expression")
                .validate(|s, _| s.len() <= 4096)
        });
        let subscription = cx.subscribe_in(&input, w, |this, _, event, w, cx| match event {
            InputEvent::Change => this.refresh(cx),
            InputEvent::PressEnter { .. } => this.execute(Control::Evaluate, w, cx),
            _ => (),
        });
        input.read(cx).focus_handle(cx).focus(w);
        Self {
            calculator: Calculator::default(),
            input,
            result: None,
            palette,
            message: String::new(),
            focus: cx.focus_handle(),
            controls: (0..160).map(|_| cx.focus_handle().tab_stop(true)).collect(),
            _input: subscription,
        }
    }
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value();
        self.result = (!text.trim().is_empty()).then(|| self.calculator.evaluate(&text));
        self.message.clear();
        cx.notify();
    }
    fn execute(&mut self, action: Control, w: &mut Window, cx: &mut Context<Self>) {
        match action {
            Control::Page(page) => {
                self.calculator.page = page;
                self.refresh(cx);
            }
            Control::Width(width) => {
                self.calculator.width = width;
                self.refresh(cx);
            }
            Control::Insert(text) => {
                self.input.update(cx, |i, cx| i.replace(text, w, cx));
                self.refresh(cx);
            }
            Control::Clear => {
                self.input.update(cx, |i, cx| i.set_value("", w, cx));
                self.refresh(cx);
            }
            Control::Evaluate => {
                let text = self.input.read(cx).value();
                if !text.trim().is_empty() {
                    self.result = Some(self.calculator.commit(&text));
                    if let Some(Ok(value)) = self.result {
                        self.message = "Added to history".into();
                        if std::env::var_os("CROWBAR_CALC_CHECK").is_some() {
                            eprintln!(
                                "CALC mode={} width={} result={value}",
                                self.calculator.page.label(),
                                self.calculator.width
                            );
                        }
                    }
                }
            }
            Control::Copy => {
                if let Some(Ok(value)) = self.result {
                    cx.write_to_clipboard(ClipboardItem::new_string(value.to_string()));
                    self.message = "Result copied".into();
                }
            }
            Control::ClearHistory => {
                self.calculator.history.clear();
                self.message = "History cleared".into();
            }
            Control::Reuse(index) => {
                if let Some(text) = self.calculator.reuse(index) {
                    self.input.update(cx, |i, cx| i.set_value(text, w, cx));
                    self.refresh(cx);
                }
            }
            Control::Close => {
                w.remove_window();
                return;
            }
        }
        self.input.read(cx).focus_handle(cx).focus(w);
        cx.notify();
    }
    fn control(
        &self,
        index: usize,
        label: impl Into<gpui::SharedString>,
        action: Control,
        enabled: bool,
        w: &Window,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let active = matches!(action,Control::Page(p) if p == self.calculator.page)
            || matches!(action,Control::Width(n) if n == self.calculator.width)
            || matches!(action, Control::Evaluate);
        button(
            ("calc-control", index),
            &self.controls[index],
            label,
            self.palette,
            w,
            if active {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Secondary
            },
            enabled,
        )
        .when(enabled, |d| {
            d.on_click(cx.listener(move |m, _, w, cx| m.execute(action, w, cx)))
                .on_key_down(cx.listener(move |m, event: &gpui::KeyDownEvent, w, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        m.execute(action, w, cx);
                        cx.stop_propagation();
                    }
                }))
        })
    }
}
impl Focusable for Calc {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for Calc {
    fn render(&mut self, w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let valid = matches!(self.result, Some(Ok(_)));
        let result = match &self.result {
            Some(Ok(v)) => v.to_string(),
            Some(Err(e)) => e.to_string(),
            None => "0".into(),
        };
        let extra: &[&str] = match self.calculator.page {
            Page::Standard => &[],
            Page::Scientific => &[
                "sin(", "cos(", "tan(", "ln(", "log(", "sqrt(", "asin(", "acos(", "atan(", "exp(", "pi", "e",
            ],
            Page::Programmer => &[
                "0x", "0o", "0b", " AND ", " OR ", " XOR ", "NOT ", " << ", " >> ", "A", "B", "C", "D", "E", "F",
            ],
        };
        let mut keys = div().flex().flex_col().gap_2();
        for (row, group) in extra.chunks(6).enumerate() {
            keys = keys.child(div().flex().gap_2().children(group.iter().enumerate().map(|(col, &s)| {
                self.control(12 + row * 6 + col, s.trim(), Control::Insert(s), true, w, cx)
                    .flex_1()
                    .min_w_0()
                    .px_1()
            })));
        }
        for (row, group) in [
            ["(", ")", "%", "^"],
            ["7", "8", "9", "/"],
            ["4", "5", "6", "*"],
            ["1", "2", "3", "-"],
            ["0", ".", "=", "+"],
        ]
        .iter()
        .enumerate()
        {
            keys = keys.child(div().flex().gap_2().children(group.iter().enumerate().map(|(col, &s)| {
                self.control(
                    30 + row * 4 + col,
                    s,
                    if s == "=" {
                        Control::Evaluate
                    } else {
                        Control::Insert(s)
                    },
                    if s == "." {
                        self.calculator.page != Page::Programmer
                    } else if s == "=" {
                        valid
                    } else {
                        true
                    },
                    w,
                    cx,
                )
                .flex_1()
            })));
        }
        let mut display = div()
            .flex()
            .flex_col()
            .gap_2()
            .pb_3()
            .child(div().text_color(rgb(p.muted)).text_size(px(12.)).child("Expression"))
            .child(div().font_family(FONT_MONO).child(Input::new(&self.input)))
            .child(
                div()
                    .id("result")
                    .min_h(px(48.))
                    .overflow_x_scroll()
                    .font_family(FONT_MONO)
                    .text_size(px(if valid || self.result.is_none() { 28. } else { 13. }))
                    .text_color(rgb(if matches!(self.result, Some(Err(_))) {
                        p.danger
                    } else {
                        p.text
                    }))
                    .child(result),
            );
        if self.calculator.page == Page::Programmer {
            display = display.child(
                div()
                    .flex()
                    .gap_2()
                    .children([8, 16, 32, 64].into_iter().enumerate().map(|(i, n)| {
                        self.control(5 + i, format!("{n}-bit"), Control::Width(n), true, w, cx)
                            .flex_1()
                            .px_1()
                            .h(px(32.))
                    })),
            );
            let bases = self
                .result
                .as_ref()
                .and_then(|r| r.as_ref().ok())
                .and_then(|v| v.bases());
            display = display.child(div().flex().flex_col().gap_1().children(
                ["HEX", "DEC", "OCT", "BIN"].into_iter().enumerate().map(|(i, label)| {
                    div()
                        .flex()
                        .gap_3()
                        .text_size(px(12.))
                        .child(div().w(px(32.)).text_color(rgb(p.muted)).child(label))
                        .child(
                            div()
                                .id(("base", i))
                                .flex_1()
                                .min_w_0()
                                .overflow_x_scroll()
                                .font_family(FONT_MONO)
                                .child(bases.as_ref().map(|b| b[i].clone()).unwrap_or_else(|| "—".into())),
                        )
                }),
            ));
        }
        let history = div()
            .w(px(250.))
            .flex_shrink_0()
            .border_l_1()
            .border_color(rgb(p.line))
            .flex()
            .flex_col()
            .p_4()
            .gap_3()
            .child(
                div().flex().justify_between().items_center().child("History").child(
                    self.control(
                        10,
                        "Clear",
                        Control::ClearHistory,
                        !self.calculator.history.is_empty(),
                        w,
                        cx,
                    )
                    .h(px(30.)),
                ),
            )
            .child(
                div()
                    .id("history")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .when(self.calculator.history.is_empty(), |d| {
                        d.child(
                            div()
                                .text_color(rgb(p.muted))
                                .text_size(px(12.))
                                .child("Press Enter to keep a calculation here. Select an entry to use it again."),
                        )
                    })
                    .children(self.calculator.history.iter().enumerate().map(|(i, e)| {
                        self.control(60 + i, "", Control::Reuse(i), true, w, cx)
                            .h_auto()
                            .p_3()
                            .flex_col()
                            .items_start()
                            .gap_1()
                            .child(div().text_size(px(11.)).text_color(rgb(p.muted)).child(e.page.label()))
                            .child(
                                div()
                                    .w_full()
                                    .font_family(FONT_MONO)
                                    .text_ellipsis()
                                    .child(e.expression.clone()),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .font_family(FONT_MONO)
                                    .text_ellipsis()
                                    .child(format!("= {}", e.value)),
                            )
                    })),
            );
        div()
            .key_context("Calculator")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(p.canvas))
            .text_color(rgb(p.text))
            .font_family(FONT_UI)
            .text_size(px(13.))
            .on_action(cx.listener(|m, _: &Standard, w, cx| m.execute(Control::Page(Page::Standard), w, cx)))
            .on_action(cx.listener(|m, _: &Scientific, w, cx| m.execute(Control::Page(Page::Scientific), w, cx)))
            .on_action(cx.listener(|m, _: &Programmer, w, cx| m.execute(Control::Page(Page::Programmer), w, cx)))
            .on_action(cx.listener(|m, _: &Evaluate, w, cx| m.execute(Control::Evaluate, w, cx)))
            .capture_action(cx.listener(|m, _: &CopyResult, w, cx| m.execute(Control::Copy, w, cx)))
            .capture_action(cx.listener(|m, _: &gpui_component::input::Copy, w, cx| m.execute(Control::Copy, w, cx)))
            .on_action(cx.listener(|m, _: &Clear, w, cx| m.execute(Control::Clear, w, cx)))
            .on_action(cx.listener(|m, _: &Expression, w, cx| m.input.read(cx).focus_handle(cx).focus(w)))
            .on_action(cx.listener(|m, _: &Quit, w, cx| m.execute(Control::Close, w, cx)))
            .on_action(|_: &Tab, w, _| w.focus_next())
            .on_action(|_: &BackTab, w, _| w.focus_prev())
            .child(crowbar_ui::titlebar(
                "Calculator",
                self.control(11, "Close", Control::Close, true, w, cx).h(px(30.)),
                p,
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(rgb(p.line))
                    .children(
                        [Page::Standard, Page::Scientific, Page::Programmer]
                            .into_iter()
                            .enumerate()
                            .map(|(i, page)| self.control(i, page.label(), Control::Page(page), true, w, cx)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("calculator-body")
                            .flex_1()
                            .min_w_0()
                            .p_4()
                            .overflow_y_scroll()
                            .child(display)
                            .child(keys)
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .mt_3()
                                    .child(
                                        self.control(3, "Clear expression", Control::Clear, true, w, cx)
                                            .flex_1(),
                                    )
                                    .child(self.control(4, "Copy result", Control::Copy, valid, w, cx).flex_1()),
                            )
                            .child(div().mt_3().text_size(px(12.)).text_color(rgb(p.muted)).child(
                                match self.calculator.page {
                                    Page::Standard => "Type freely · Enter keeps the result · Ctrl+C copies",
                                    Page::Scientific => "Radians · log is base 10 · ^ raises to a power",
                                    Page::Programmer => "Unsigned words · NOT and shifts stay within the width",
                                },
                            )),
                    )
                    .child(history),
            )
            .child(
                div()
                    .h(px(32.))
                    .flex_shrink_0()
                    .px_4()
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(rgb(p.line))
                    .text_size(px(12.))
                    .text_color(rgb(p.muted))
                    .child(if self.message.is_empty() {
                        "Alt+1 / 2 / 3 changes mode · Ctrl+L focuses the expression".into()
                    } else {
                        self.message.clone()
                    }),
            )
    }
}
fn main() {
    if std::env::args().any(|a| a == "--version") {
        println!("crowbaros-calc {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    Application::new().with_assets(gpui_component_assets::Assets).run(|cx| {
        gpui_component::init(cx);
        load_fonts(cx);
        cx.bind_keys([
            KeyBinding::new("alt-1", Standard, Some("Calculator")),
            KeyBinding::new("alt-2", Scientific, Some("Calculator")),
            KeyBinding::new("alt-3", Programmer, Some("Calculator")),
            KeyBinding::new("ctrl-enter", Evaluate, Some("Calculator")),
            KeyBinding::new("ctrl-c", CopyResult, Some("Calculator")),
            KeyBinding::new("escape", Clear, Some("Calculator")),
            KeyBinding::new("ctrl-l", Expression, Some("Calculator")),
            KeyBinding::new("ctrl-q", Quit, Some("Calculator")),
            KeyBinding::new("tab", Tab, Some("Calculator")),
            KeyBinding::new("shift-tab", BackTab, Some("Calculator")),
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(900.), px(760.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(780.), px(620.))),
                app_id: Some("com.crowbaros.Calc".into()),
                ..Default::default()
            },
            |w, cx| {
                let calc = cx.new(|cx| Calc::new(w, cx));
                cx.new(|cx| Root::new(calc, w, cx))
            },
        )
        .expect("open Calculator window");
    });
}
