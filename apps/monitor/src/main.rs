// SPDX-License-Identifier: MIT
use crowbar_ui::{ButtonVariant, FONT_MONO, FONT_UI, ThemeWatcher, UiPalette, button, load_fonts, spawn_theme_watcher};
use crowbaros_monitor::{metrics::*, system::*};
use gpui::{
    App, Application, Bounds, Context, Entity, FocusHandle, Focusable, KeyBinding, PathBuilder, ScrollStrategy,
    Subscription, Task, UniformListScrollHandle, Window, WindowBounds, WindowOptions, actions, canvas, div, point,
    prelude::*, px, rgb, size, uniform_list,
};
use gpui_component::{
    Root, Theme, ThemeMode,
    input::{Input, InputEvent, InputState},
};
use std::{collections::BTreeMap, time::Duration};

actions!(
    monitor,
    [
        Overview, Processes, Services, Search, End, Force, Confirm, Dismiss, Next, Previous, Quit, Tab, BackTab
    ]
);
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    Processes,
    Services,
}
#[derive(Clone, Copy)]
enum Control {
    Page(Page),
    Sort(Sort),
    End,
    Force,
    Confirm,
    Cancel,
    Close,
}

struct Monitor {
    page: Page,
    palette: UiPalette,
    snapshot: Option<Snapshot>,
    error: Option<String>,
    message: String,
    histories: BTreeMap<String, History>,
    query: Entity<InputState>,
    rows: Vec<usize>,
    scroll: UniformListScrollHandle,
    sort: Sort,
    descending: bool,
    selected: Option<(u32, u64)>,
    confirmation: Option<Process>,
    busy: bool,
    focus: FocusHandle,
    controls: Vec<FocusHandle>,
    _search: Subscription,
    _poll: Task<()>,
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
impl Monitor {
    fn new(w: &mut Window, cx: &mut Context<Self>) -> Self {
        w.set_window_title("Monitor");
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
        let query = cx.new(|cx| InputState::new(w, cx).placeholder("Search by name, PID, or user"));
        let search = cx.subscribe(&query, |this, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.refilter(cx);
                cx.notify();
            }
        });
        let poll = cx.spawn_in(w, async move |this, cx| {
            let mut collector = Collector::new();
            loop {
                let Some(entity) = this.upgrade() else {
                    break;
                };
                let details = entity.read_with(cx, |m, _| m.page == Page::Services).unwrap_or(false);
                drop(entity);
                let (next, result) = cx
                    .background_executor()
                    .spawn(async move {
                        let result = collector.sample(details);
                        (collector, result)
                    })
                    .await;
                collector = next;
                if this
                    .update(cx, |m, cx| {
                        m.receive(result, cx);
                    })
                    .is_err()
                {
                    break;
                }
                let active = cx.update(|w, _| w.is_window_active()).unwrap_or(false);
                // Inactive includes hidden workspaces. No frame loop or overlapping samples.
                cx.background_executor()
                    .timer(Duration::from_secs(if active { 1 } else { 5 }))
                    .await;
            }
        });
        let focus = cx.focus_handle();
        focus.focus(w);
        Self {
            page: Page::Overview,
            palette,
            snapshot: None,
            error: None,
            message: String::new(),
            histories: BTreeMap::new(),
            query,
            rows: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            sort: Sort::Cpu,
            descending: true,
            selected: None,
            confirmation: None,
            busy: false,
            focus,
            controls: (0..14).map(|_| cx.focus_handle().tab_stop(true)).collect(),
            _search: search,
            _poll: poll,
        }
    }
    fn refilter(&mut self, cx: &App) {
        self.rows = self
            .snapshot
            .as_ref()
            .map(|s| {
                process_rows(
                    &s.processes,
                    self.query.read(cx).value().as_str(),
                    self.sort,
                    self.descending,
                )
            })
            .unwrap_or_default();
        if let Some(snapshot) = &self.snapshot
            && !self
                .rows
                .iter()
                .any(|i| Some((snapshot.processes[*i].pid, snapshot.processes[*i].start)) == self.selected)
        {
            self.selected = None;
        }
    }
    fn receive(&mut self, result: Result<Snapshot, String>, cx: &mut Context<Self>) {
        match result {
            Err(error) => {
                self.error = Some(error);
                for h in self.histories.values_mut() {
                    h.push(None);
                }
            }
            Ok(s) => {
                self.error = None;
                if std::env::var_os("CROWBAR_MONITOR_CHECK").is_some() {
                    eprintln!(
                        "MONITOR sample cpu={} memory={} processes={} details={}",
                        s.cpu.get("cpu").copied().flatten().is_some(),
                        s.memory.total > 0,
                        s.processes.len(),
                        !s.details.storage.is_empty()
                    );
                }
                let mut values = s.cpu.clone();
                values.insert("Memory".into(), Some(s.memory.used as f64));
                values.insert("Swap".into(), Some(s.memory.swap_used as f64));
                values.insert("Disk read".into(), s.disk_read);
                values.insert("Disk write".into(), s.disk_write);
                values.insert("Network received".into(), s.net_read);
                values.insert("Network sent".into(), s.net_write);
                for g in &s.gpus {
                    values.insert(format!("GPU {}", g.name), g.utilization);
                }
                self.histories.retain(|key, _| values.contains_key(key));
                for (key, value) in values {
                    self.histories
                        .entry(key)
                        .or_insert_with(|| History::new(60))
                        .push(value);
                }
                if self
                    .selected
                    .is_some_and(|id| !s.processes.iter().any(|p| (p.pid, p.start) == id))
                {
                    self.selected = None;
                }
                self.snapshot = Some(s);
                self.refilter(cx);
            }
        }
        cx.notify();
    }
    fn selection(&self) -> Option<&Process> {
        self.snapshot
            .as_ref()?
            .processes
            .iter()
            .find(|p| Some((p.pid, p.start)) == self.selected)
    }
    fn execute(&mut self, control: Control, w: &mut Window, cx: &mut Context<Self>) {
        if self.confirmation.is_some() && !matches!(control, Control::Confirm | Control::Cancel | Control::Close) {
            return;
        }
        match control {
            Control::Page(page) => {
                self.page = page;
                self.focus.focus(w);
            }
            Control::Sort(sort) => {
                if self.sort == sort {
                    self.descending = !self.descending;
                } else {
                    self.sort = sort;
                    self.descending = matches!(sort, Sort::Cpu | Sort::Memory);
                }
                self.refilter(cx);
            }
            Control::End => {
                if let Some(p) = self.selection().cloned() {
                    self.signal(p, Signal::Terminate, cx);
                }
            }
            Control::Force => {
                if !self.busy
                    && let Some(p) = self
                        .selection()
                        .filter(|p| can_signal(p, person_uid(), std::process::id()))
                        .cloned()
                {
                    self.confirmation = Some(p);
                    self.controls[12].focus(w);
                }
            }
            Control::Confirm => {
                if let Some(p) = self.confirmation.take() {
                    self.signal(p, Signal::Kill, cx);
                    self.focus.focus(w);
                }
            }
            Control::Cancel => {
                self.confirmation = None;
                self.focus.focus(w);
            }
            Control::Close => w.remove_window(),
        }
        if std::env::var_os("CROWBAR_MONITOR_CHECK").is_some() {
            eprintln!(
                "MONITOR page={} confirmation={}",
                match self.page {
                    Page::Overview => "overview",
                    Page::Processes => "processes",
                    Page::Services => "services",
                },
                self.confirmation.is_some()
            );
        }
        cx.notify();
    }
    fn signal(&mut self, p: Process, signal: Signal, cx: &mut Context<Self>) {
        if self.error.is_some() || self.busy || !can_signal(&p, person_uid(), std::process::id()) {
            return;
        }
        self.busy = true;
        self.message = format!("Sending signal to {} ({})…", p.name, p.pid);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { signal_process(&p, signal) })
                .await;
            let _ = this.update(cx, |m, cx| {
                m.busy = false;
                m.message = match result {
                    Ok(()) => "Signal sent. The process list will refresh.".into(),
                    Err(e) => e,
                };
                cx.notify();
            });
        })
        .detach();
    }
    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.page != Page::Processes || self.confirmation.is_some() || self.rows.is_empty() {
            return;
        }
        if let Some(s) = &self.snapshot {
            let index = self
                .rows
                .iter()
                .position(|i| Some((s.processes[*i].pid, s.processes[*i].start)) == self.selected);
            let next = index
                .map(|i| (i as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize)
                .unwrap_or(0);
            let p = &s.processes[self.rows[next]];
            self.selected = Some((p.pid, p.start));
            self.scroll.scroll_to_item(next, ScrollStrategy::Top);
            cx.notify();
        }
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
        let enabled = enabled
            && (self.confirmation.is_none() || matches!(action, Control::Confirm | Control::Cancel | Control::Close));
        button(
            ("control", index),
            &self.controls[index],
            label,
            self.palette,
            w,
            if matches!(action, Control::Page(page) if page == self.page) {
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
    fn chart(&self, label: &str, key: &str, value: String, maximum: Option<f64>) -> gpui::Div {
        let p = self.palette;
        let values = self.histories.get(key).map(History::values).unwrap_or_default();
        let scale = maximum
            .unwrap_or_else(|| values.iter().flatten().copied().fold(1., f64::max))
            .max(1.);
        div()
            .flex()
            .flex_col()
            .gap_2()
            .py_3()
            .border_b_1()
            .border_color(rgb(p.line))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .gap_3()
                    .child(label.to_string())
                    .child(div().font_family(FONT_MONO).child(value)),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, _, w, _| {
                        let width: f32 = bounds.size.width.into();
                        let height: f32 = bounds.size.height.into();
                        let mut path = PathBuilder::stroke(px(1.5));
                        let mut connected = false;
                        let mut segments = 0;
                        for (i, v) in values.iter().enumerate() {
                            let Some(v) = v else {
                                connected = false;
                                continue;
                            };
                            let pt = bounds.origin
                                + point(
                                    px(i as f32 * width / 59.),
                                    px(height * (1. - (*v / scale).clamp(0., 1.) as f32)),
                                );
                            if connected {
                                path.line_to(pt);
                                segments += 1;
                            } else {
                                path.move_to(pt);
                            }
                            connected = true;
                        }
                        if segments > 0
                            && let Ok(path) = path.build()
                        {
                            w.paint_path(path, rgb(p.focus));
                        }
                    },
                )
                .w_full()
                .h(px(56.)),
            )
    }
    fn overview(&self) -> gpui::AnyElement {
        let mut body = div()
            .id("overview")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_2();
        let Some(s) = &self.snapshot else {
            return body.child("Reading system metrics…").into_any_element();
        };
        body =
            body.child(div().text_size(px(28.)).child("Overview"))
                .child(
                    div()
                        .text_color(rgb(self.palette.muted))
                        .child("Last 60 samples · 1 second active / 5 seconds inactive"),
                )
                .child(self.chart(
                    "CPU · all cores",
                    "cpu",
                    percent(s.cpu.get("cpu").copied().flatten()),
                    Some(100.),
                ))
                .child(
                    div().flex().flex_wrap().gap_3().children(
                        s.cpu
                            .iter()
                            .filter(|(name, _)| name.as_str() != "cpu")
                            .map(|(name, value)| {
                                div()
                                    .w(px(150.))
                                    .child(self.chart(name, name, percent(*value), Some(100.)))
                            }),
                    ),
                )
                .child(self.chart(
                    "Memory",
                    "Memory",
                    format!("{} / {}", bytes(s.memory.used as f64), bytes(s.memory.total as f64)),
                    Some(s.memory.total as f64),
                ))
                .child(self.chart(
                    "Swap",
                    "Swap",
                    if s.memory.swap_total == 0 {
                        "Not configured".into()
                    } else {
                        format!(
                            "{} / {}",
                            bytes(s.memory.swap_used as f64),
                            bytes(s.memory.swap_total as f64)
                        )
                    },
                    Some(s.memory.swap_total as f64),
                ));
        for (label, value) in [
            ("Disk read", s.disk_read),
            ("Disk write", s.disk_write),
            ("Network received", s.net_read),
            ("Network sent", s.net_write),
        ] {
            body = body.child(self.chart(label, label, throughput(value), None));
        }
        if s.gpus.is_empty() {
            body = body.child("GPU metrics are not exposed by this system.");
        }
        for gpu in &s.gpus {
            let vram = match (gpu.used, gpu.total) {
                (Some(used), Some(total)) => format!("{} / {} VRAM", bytes(used as f64), bytes(total as f64)),
                _ => "VRAM unavailable".into(),
            };
            body = body.child(self.chart(
                &format!("GPU · {}", gpu.name),
                &format!("GPU {}", gpu.name),
                format!("{} · {vram}", percent(gpu.utilization)),
                Some(100.),
            ));
        }
        body.into_any_element()
    }
    fn processes(&self, w: &Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = self.palette;
        let headers = [
            ("Name", Sort::Name),
            ("PID", Sort::Pid),
            ("User", Sort::User),
            ("CPU", Sort::Cpu),
            ("Memory", Sort::Memory),
            ("State", Sort::State),
        ];
        let mut header = div().flex().gap_1();
        for (i, (name, sort)) in headers.into_iter().enumerate() {
            let label = if self.sort == sort {
                format!("{name} {}", if self.descending { "↓" } else { "↑" })
            } else {
                name.into()
            };
            header = header.child(
                div().when(i == 0, |d| d.flex_1()).when(i > 0, |d| d.w(px(100.))).child(
                    self.control(i + 3, label, Control::Sort(sort), true, w, cx)
                        .w_full()
                        .px_1(),
                ),
            );
        }
        let entity = cx.entity();
        let list = uniform_list("process-list", self.rows.len(), move |range, _, cx| {
            entity.update(cx, |m, cx| {
                range
                    .filter_map(|row| {
                        let process = m.snapshot.as_ref()?.processes.get(*m.rows.get(row)?)?;
                        let id = (process.pid, process.start);
                        let selected = m.selected == Some(id);
                        Some(
                            div()
                                .id(("pid", process.pid as usize))
                                .h(px(36.))
                                .flex()
                                .items_center()
                                .gap_1()
                                .px_2()
                                .bg(rgb(if selected { p.surface } else { p.canvas }))
                                .border_b_1()
                                .border_color(rgb(p.line))
                                .when(selected, |d| d.border_1().border_color(rgb(p.focus)))
                                .cursor_pointer()
                                .hover(|d| d.bg(rgb(p.surface)))
                                .on_click(cx.listener(move |m, _, w, cx| {
                                    if m.confirmation.is_none() {
                                        m.selected = Some(id);
                                        m.focus.focus(w);
                                        cx.notify();
                                    }
                                }))
                                .child(div().flex_1().min_w_0().overflow_hidden().child(process.name.clone()))
                                .children(
                                    [
                                        process.pid.to_string(),
                                        process.user.clone(),
                                        percent(process.cpu),
                                        bytes(process.rss as f64),
                                        process.state.clone(),
                                    ]
                                    .into_iter()
                                    .map(|v| {
                                        div()
                                            .w(px(100.))
                                            .flex_shrink_0()
                                            .overflow_hidden()
                                            .font_family(FONT_MONO)
                                            .child(v)
                                    }),
                                ),
                        )
                    })
                    .collect::<Vec<_>>()
            })
        })
        .track_scroll(self.scroll.clone())
        .flex_1()
        .min_h_0();
        let allowed = self.error.is_none()
            && !self.busy
            && self
                .selection()
                .is_some_and(|p| can_signal(p, person_uid(), std::process::id()));
        div()
            .flex_1()
            .min_h_0()
            .p_6()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_size(px(28.)).child("Processes"))
            .child(Input::new(&self.query))
            .child(div().text_color(rgb(p.muted)).child(format!(
                "{} processes · CPU uses 100% per core · ↑/↓ select · Ctrl+E end · Ctrl+K force quit",
                self.rows.len()
            )))
            .child(header)
            .when(self.rows.is_empty(), |d| d.child("No processes match your search."))
            .child(list)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(self.control(9, "End Process", Control::End, allowed, w, cx))
                    .child(self.control(10, "Force Quit…", Control::Force, allowed, w, cx))
                    .child(
                        div()
                            .text_color(rgb(p.muted))
                            .child("Only your own processes can be ended."),
                    ),
            )
            .into_any_element()
    }
    fn services(&self) -> gpui::AnyElement {
        let d = self.snapshot.as_ref().map(|s| &s.details);
        div()
            .id("services")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(div().text_size(px(28.)).child("Services & Storage"))
            .child(
                div()
                    .text_color(rgb(self.palette.muted))
                    .child("Read-only · refreshes every 10 seconds while this view is open"),
            )
            .child(div().text_size(px(16.)).child("Root filesystem"))
            .child(
                div().font_family(FONT_MONO).child(
                    d.map(|d| {
                        if d.storage.is_empty() {
                            "Reading storage…".into()
                        } else {
                            d.storage.clone()
                        }
                    })
                    .unwrap_or_else(|| "Reading storage…".into()),
                ),
            )
            .child(div().text_size(px(16.)).child("Runit services"))
            .child(
                div().font_family(FONT_MONO).child(
                    d.map(|d| {
                        if d.services.is_empty() {
                            "Reading services…".into()
                        } else {
                            d.services.join("\n")
                        }
                    })
                    .unwrap_or_else(|| "Reading services…".into()),
                ),
            )
            .child(div().text_size(px(16.)).child("Recent system logs"))
            .child(
                div().font_family(FONT_MONO).child(
                    d.map(|d| {
                        if d.logs.is_empty() {
                            "No recent log lines.".into()
                        } else {
                            d.logs.clone()
                        }
                    })
                    .unwrap_or_default(),
                ),
            )
            .into_any_element()
    }
}
impl Focusable for Monitor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}
impl Render for Monitor {
    fn render(&mut self, w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let content = match self.page {
            Page::Overview => self.overview(),
            Page::Processes => self.processes(w, cx),
            Page::Services => self.services(),
        };
        let mut root = div()
            .key_context("Monitor")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .font_family(FONT_UI)
            .text_size(px(13.))
            .bg(rgb(p.canvas))
            .text_color(rgb(p.text))
            .on_action(cx.listener(|m, _: &Overview, w, cx| m.execute(Control::Page(Page::Overview), w, cx)))
            .on_action(cx.listener(|m, _: &Processes, w, cx| m.execute(Control::Page(Page::Processes), w, cx)))
            .on_action(cx.listener(|m, _: &Services, w, cx| m.execute(Control::Page(Page::Services), w, cx)))
            .on_action(cx.listener(|m, _: &Search, w, cx| {
                if m.confirmation.is_none() {
                    m.page = Page::Processes;
                    m.query.read(cx).focus_handle(cx).focus(w);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|m, _: &End, w, cx| m.execute(Control::End, w, cx)))
            .on_action(cx.listener(|m, _: &Force, w, cx| m.execute(Control::Force, w, cx)))
            .on_action(cx.listener(|m, _: &Confirm, w, cx| m.execute(Control::Confirm, w, cx)))
            .on_action(cx.listener(|m, _: &Dismiss, w, cx| m.execute(Control::Cancel, w, cx)))
            .on_action(cx.listener(|m, _: &Next, _, cx| m.move_selection(1, cx)))
            .on_action(cx.listener(|m, _: &Previous, _, cx| m.move_selection(-1, cx)))
            .on_action(cx.listener(|m, _: &Quit, w, cx| m.execute(Control::Close, w, cx)))
            .on_action(|_: &Tab, w, _| w.focus_next())
            .on_action(|_: &BackTab, w, _| w.focus_prev())
            .child(crowbar_ui::titlebar(
                "Monitor",
                self.control(13, "Close", Control::Close, true, w, cx).h(px(30.)),
                p,
            ))
            .child(
                div()
                    .flex()
                    .px_6()
                    .py_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(rgb(p.line))
                    .child(self.control(0, "Overview", Control::Page(Page::Overview), true, w, cx))
                    .child(self.control(1, "Processes", Control::Page(Page::Processes), true, w, cx))
                    .child(self.control(2, "Services & Storage", Control::Page(Page::Services), true, w, cx)),
            )
            .child(content);
        if let Some(process) = &self.confirmation {
            root = root.child(
                div()
                    .px_6()
                    .py_4()
                    .border_t_1()
                    .border_color(rgb(p.danger))
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(format!(
                        "Force quit {} ({})? Unsaved work may be lost.",
                        process.name, process.pid
                    ))
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(self.control(12, "Cancel", Control::Cancel, true, w, cx))
                            .child(self.control(11, "Force Quit", Control::Confirm, !self.busy, w, cx)),
                    ),
            );
        }
        let status = self
            .error
            .as_ref()
            .map(|e| format!("{e} Displaying the last sample; retrying automatically."))
            .unwrap_or_else(|| {
                if !self.message.is_empty() {
                    self.message.clone()
                } else {
                    self.snapshot
                        .as_ref()
                        .map(|s| s.warnings.join(" · "))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| {
                            "Alt+1 Overview · Alt+2 Processes · Alt+3 Services & Storage · Ctrl+F Search".into()
                        })
                }
            });
        root.child(crowbar_ui::status_footer(status, p))
    }
}
fn percent(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.1}%"))
        .unwrap_or_else(|| "Unavailable".into())
}
fn throughput(value: Option<f64>) -> String {
    value
        .map(|v| format!("{}/s", bytes(v)))
        .unwrap_or_else(|| "Waiting for samples".into())
}
fn main() {
    if std::env::args().any(|a| a == "--sample") {
        let mut collector = Collector::new();
        match collector.sample(true) {
            Ok(s) => println!("{}", serde_json::to_string(&s).expect("finite metrics")),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return;
    }
    Application::new().with_assets(gpui_component_assets::Assets).run(|cx| {
        gpui_component::init(cx);
        load_fonts(cx);
        cx.bind_keys([
            KeyBinding::new("alt-1", Overview, Some("Monitor")),
            KeyBinding::new("alt-2", Processes, Some("Monitor")),
            KeyBinding::new("alt-3", Services, Some("Monitor")),
            KeyBinding::new("ctrl-f", Search, Some("Monitor")),
            KeyBinding::new("ctrl-e", End, Some("Monitor")),
            KeyBinding::new("ctrl-k", Force, Some("Monitor")),
            KeyBinding::new("ctrl-enter", Confirm, Some("Monitor")),
            KeyBinding::new("escape", Dismiss, Some("Monitor")),
            KeyBinding::new("down", Next, Some("Monitor && !Input")),
            KeyBinding::new("up", Previous, Some("Monitor && !Input")),
            KeyBinding::new("ctrl-q", Quit, Some("Monitor")),
            KeyBinding::new("tab", Tab, Some("Monitor")),
            KeyBinding::new("shift-tab", BackTab, Some("Monitor")),
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1000.), px(760.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(840.), px(560.))),
                app_id: Some("com.crowbaros.Monitor".into()),
                ..Default::default()
            },
            |w, cx| {
                let monitor = cx.new(|cx| Monitor::new(w, cx));
                cx.new(|cx| Root::new(monitor, w, cx))
            },
        )
        .expect("open Monitor window");
    });
}
