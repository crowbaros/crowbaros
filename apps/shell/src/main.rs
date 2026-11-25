// SPDX-License-Identifier: MIT
mod agent;
mod calendar;
mod catalog;
mod input;
mod quick_settings;
mod session;
mod theme;
use catalog::AppEntry;
use gpui::{layer_shell::*, prelude::*, *};
use futures::StreamExt;
use std::{borrow::Cow, path::PathBuf, sync::{Arc, Mutex}, time::{Duration, Instant}};
use theme::{Palette, Preferences};
actions!(shell, [Next, Previous, Open, Close, Unlock, SendMessage, PrevMonth, NextMonth]);
struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Some(Cow::Borrowed(match path {
            "apps" => include_bytes!("../assets/apps.svg"),
            "files" => include_bytes!("../assets/files.svg"),
            "browser" => include_bytes!("../assets/browser.svg"),
            "terminal" => include_bytes!("../assets/terminal.svg"),
            "settings" => include_bytes!("../assets/settings.svg"),
            "lock" => include_bytes!("../assets/lock.svg"),
            "application" => include_bytes!("../assets/application.svg"),
            "search" => include_bytes!("../assets/search.svg"),
            "close" => include_bytes!("../assets/close.svg"),
            "arrow" => include_bytes!("../assets/arrow.svg"),
            "user" => include_bytes!("../assets/user.svg"),
            "mark-micro" => include_bytes!("../assets/mark-micro.png"),
            "mark-dark" => include_bytes!("../assets/mark-dark.png"),
            "mark-frost" => include_bytes!("../assets/mark-frost.png"),
            "logo-horizontal-primary" => include_bytes!("../assets/logo-horizontal-primary.png"),
            "logo-horizontal-frost" => include_bytes!("../assets/logo-horizontal-frost.png"),
            _ => return Ok(None),
        })))
    }
    fn list(&self, _: &str) -> Result<Vec<SharedString>> {
        Ok(vec![])
    }
}
fn icon(name: &'static str, color: u32, size: f32) -> Svg {
    svg().path(name).size(px(size)).text_color(rgb(color))
}
fn asset(name: &str) -> PathBuf {
    let installed = PathBuf::from("/usr/share/crowbaros-shell").join(name);
    if installed.exists() {
        installed
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(name)
    }
}
const BAR_HEIGHT: f32 = 24.;
const DOCK_WIDTH: f32 = 260.;
const DOCK_HEIGHT: f32 = 48.;
const DOCK_LAYER_HEIGHT: f32 = 76.;

fn bar_mark_asset(preferences: &Preferences) -> &'static str {
    if preferences.studio {
        "mark-frost"
    } else if matches!(preferences.theme, theme::Theme::Dark) {
        "mark-micro"
    } else {
        "mark-dark"
    }
}

fn background_asset(preferences: &Preferences) -> Option<&'static str> {
    if preferences.studio {
        return None;
    }
    Some(match preferences.wallpaper {
        theme::Wallpaper::SignalFeather => "wallpaper.png",
        theme::Wallpaper::Nightwatch => "nightwatch.png",
        theme::Wallpaper::QuietSignal => "quiet-signal.png",
    })
}

fn occupied_workspaces(clients: &[session::Client]) -> Vec<i32> {
    let mut ids: Vec<i32> = clients
        .iter()
        .map(|c| c.workspace.id)
        .filter(|id| *id > 0)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn bar_workspaces(active: i32, clients: &[session::Client]) -> Vec<i32> {
    let mut workspaces = vec![1, 2, 3, 4];
    for id in std::iter::once(active).chain(clients.iter().map(|c| c.workspace.id)) {
        if id > 4 && !workspaces.contains(&id) {
            workspaces.push(id);
        }
    }
    workspaces.sort();
    workspaces
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DockPinState {
    Idle,
    Running { attention: bool },
    Focused { attention: bool },
}

fn dock_pin_state(app: Option<&AppEntry>, snapshot: &session::Snapshot) -> DockPinState {
    let Some(app) = app else {
        return DockPinState::Idle;
    };
    let running = snapshot.clients.iter().any(|c| app.matches_class(&c.class));
    let focused = app.matches_class(&snapshot.active.class);
    if !running && !focused {
        return DockPinState::Idle;
    }
    let attention = snapshot
        .clients
        .iter()
        .any(|c| c.attention && app.matches_class(&c.class));
    if focused {
        DockPinState::Focused { attention }
    } else {
        DockPinState::Running { attention }
    }
}

struct ClockDisplay {
    bar: String,
    lock_time: String,
    lock_date: String,
}
impl ClockDisplay {
    fn now() -> Self {
        let now = chrono::Local::now();
        Self {
            bar: now.format("%a, %b %-d · %H:%M").to_string(),
            lock_time: now.format("%H:%M").to_string(),
            lock_date: now.format("%A, %B %-d").to_string(),
        }
    }
}
struct Model {
    snapshot: session::Snapshot,
    preferences: Preferences,
    launcher: Option<WindowHandle<Launcher>>,
    quick_settings: Option<WindowHandle<QuickSettingsPopover>>,
    calendar: Option<WindowHandle<CalendarPopover>>,
    osd: Option<WindowHandle<OsdOverlay>>,
    quick_status: quick_settings::QuickStatus,
    osd_state: Option<quick_settings::OsdState>,
    osd_seq: u64,
    timer: calendar::QuickTimer,
    lock: Option<WindowHandle<LockScreen>>,
    /// The Agent's surface, while it is up.
    agent: Option<WindowHandle<AgentSurface>>,
    /// The floating Agent mark shown over a fullscreen window.
    mark_overlay: Option<(WindowHandle<MarkOverlay>, String)>,
    agent_state: agent::AgentState,
    agent_tx: Option<agent::CommandSender>,
    agent_view: Arc<Mutex<agent::AgentViewSnapshot>>,
    apps: Vec<AppEntry>,
    tx: session::Sender,
    clock: String,
    lock_time: String,
    lock_date: String,
    person: String,
}
impl Model {
    fn palette(&self) -> Palette {
        Palette::for_preferences(&self.preferences)
    }
}
fn layer(
    name: &str,
    kind: Layer,
    anchor: Anchor,
    width: f32,
    height: f32,
    zone: Option<f32>,
    keyboard: KeyboardInteractivity,
) -> WindowOptions {
    layer_with_margin(name, kind, anchor, width, height, zone, None, keyboard)
}
fn layer_with_margin(
    name: &str,
    kind: Layer,
    anchor: Anchor,
    width: f32,
    height: f32,
    zone: Option<f32>,
    margin: Option<(Pixels, Pixels, Pixels, Pixels)>,
    keyboard: KeyboardInteractivity,
) -> WindowOptions {
    WindowOptions {
        titlebar: None,
        app_id: Some("com.crowbaros.Shell".into()),
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(width), px(height)),
        ))),
        window_background: WindowBackgroundAppearance::Transparent,
        kind: WindowKind::LayerShell(LayerShellOptions {
            namespace: format!("crowbaros-{name}"),
            layer: kind,
            anchor,
            exclusive_zone: zone.map(px),
            margin,
            keyboard_interactivity: keyboard,
            ..Default::default()
        }),
        ..Default::default()
    }
}
fn render_mark_pill(mark: agent::Mark, p: Palette) -> Option<Stateful<Div>> {
    match mark {
        agent::Mark::Idle => None,
        agent::Mark::Working => Some(
            div()
                .id("agent-mark")
                .h(px(18.))
                .px(px(7.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(4.))
                .bg(rgb(p.surface))
                .border_1()
                .border_color(rgb(p.focus))
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(p.text))
                .child(div().size(px(6.)).rounded_full().bg(rgb(p.focus)))
                .child("Working"),
        ),
        agent::Mark::Done => Some(
            div()
                .id("agent-mark")
                .h(px(18.))
                .px(px(7.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded(px(4.))
                .bg(rgb(p.accent))
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(p.on_accent))
                .child(div().size(px(6.)).rounded_full().bg(rgb(p.on_accent)))
                .child("Done"),
        ),
    }
}
struct MarkOverlay {
    model: Entity<Model>,
}
impl Render for MarkOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let pill = render_mark_pill(m.agent_state.mark, p);
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_end()
            .font_family("Manrope")
            .when_some(pill, |d, mark| {
                d.child(
                    div()
                        .px(px(4.))
                        .py(px(3.))
                        .rounded(px(7.))
                        .bg(rgb(p.canvas))
                        .border_1()
                        .border_color(rgb(p.line))
                        .child(mark),
                )
            })
    }
}
struct Background {
    model: Entity<Model>,
}
impl Render for Background {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let Some(file) = background_asset(&m.preferences) else {
            return div().size_full().bg(rgb(p.canvas));
        };
        match m.preferences.wallpaper {
            theme::Wallpaper::Nightwatch => {
                let vp = window.viewport_size();
                let vp_w: f32 = vp.width.into();
                let vp_h: f32 = vp.height.into();
                let artwork = match nightwatch_size(vp_w, vp_h) {
                    Some((bg_w, bg_h)) => img(asset(file))
                        .absolute()
                        .top_0()
                        .right_0()
                        .w(px(bg_w))
                        .h(px(bg_h))
                        .object_fit(ObjectFit::Fill),
                    None => img(asset(file))
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                };
                div()
                    .relative()
                    .size_full()
                    .overflow_hidden()
                    .bg(rgb(LOCK_GROUND))
                    .child(artwork)
            }
            theme::Wallpaper::SignalFeather | theme::Wallpaper::QuietSignal => div()
                .size_full()
                .bg(rgb(LOCK_VOID))
                .child(img(asset(file)).size_full().object_fit(ObjectFit::Cover)),
        }
    }
}
struct Bar {
    model: Entity<Model>,
}
impl Render for Bar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let mark = ImageSource::Resource(Resource::Embedded(
            bar_mark_asset(&m.preferences).into(),
        ));
        let active = m.snapshot.workspace;
        let title = if m.snapshot.connected {
            m.snapshot.active.title.clone()
        } else {
            "Connecting to the compositor…".into()
        };
        let clock = m.clock.clone();
        let occupied = occupied_workspaces(&m.snapshot.clients);
        // A workspace with a window that wants attention is marked, and
        // occupied or active workspaces are shown past the first four.
        let wanting: Vec<i32> = m
            .snapshot
            .clients
            .iter()
            .filter(|c| c.attention)
            .map(|c| c.workspace.id)
            .collect();
        let workspaces = bar_workspaces(active, &m.snapshot.clients);
        div()
            .size_full()
            .flex()
            .items_center()
            .px(px(10.))
            .gap(px(12.))
            .font_family("Manrope")
            .text_size(px(11.5))
            .text_color(rgb(p.text))
            .bg(rgb(p.canvas))
            .border_b_1()
            .border_color(rgb(p.line))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("applications")
                            .w(px(24.))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(img(mark).size(px(15.)).object_fit(ObjectFit::Contain))
                            .on_click(cx.listener(|this, _, _, cx| {
                                toggle_launcher(this.model.clone(), cx)
                            })),
                    )
                    .child(div().w(px(1.)).h(px(10.)).bg(rgb(p.line)))
                    .child(div().flex().items_center().gap(px(2.)).children(
                        workspaces.into_iter().map(|id| {
                            let has_windows = occupied.contains(&id);
                            let attention = wanting.contains(&id);
                            let selected = active == id;
                            div()
                                .id(("workspace", id as usize))
                                .relative()
                                .border_b_2()
                                .border_color(if attention || selected {
                                    rgb(p.accent)
                                } else {
                                    rgba(0x00000000)
                                })
                                .w(px(18.))
                                .h(px(17.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(4.))
                                .cursor_pointer()
                                .text_size(px(11.))
                                .when(selected, |d| d.font_weight(FontWeight::SEMIBOLD))
                                .bg(rgb(if selected { p.accent } else { p.canvas }))
                                .text_color(rgb(if selected {
                                    p.on_accent
                                } else if has_windows {
                                    p.text
                                } else {
                                    p.muted
                                }))
                                .hover(move |s| {
                                    s.bg(rgb(if selected { p.accent } else { p.surface }))
                                })
                                .child(id.to_string())
                                .when(has_windows && !selected && !attention, |d| {
                                    d.child(
                                        div()
                                            .absolute()
                                            .left_0()
                                            .right_0()
                                            .bottom(px(0.))
                                            .flex()
                                            .justify_center()
                                            .child(
                                                div()
                                                    .size(px(3.))
                                                    .rounded_full()
                                                    .bg(rgb(p.muted)),
                                            ),
                                    )
                                })
                                .on_click(move |_, _, _| session::workspace(id))
                        }),
                    ))
                    .when(!title.is_empty(), |d| {
                        d.child(div().w(px(1.)).h(px(10.)).bg(rgb(p.line))).child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.5))
                                .text_color(rgb(p.muted))
                                .text_ellipsis()
                                .child(title),
                        )
                    }),
            )
            .child(
                div()
                    .id("clock")
                    .flex_none()
                    .h(px(18.))
                    .px(px(6.))
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .when(m.calendar.is_some(), |d| d.bg(rgb(p.surface)))
                    .hover(move |s| s.bg(rgb(p.surface)))
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(p.text))
                    .child(clock)
                    .on_click(cx.listener(|this, _, _, cx| {
                        toggle_calendar(this.model.clone(), cx)
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(4.))
                    .when_some(render_mark_pill(m.agent_state.mark, p), |d, pill| {
                        d.child(pill)
                    })
                    .child(
                        div()
                            .id("lock")
                            .w(px(22.))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(icon("lock", p.muted, 13.5))
                            .on_click(|_, _, _| session::lock(true)),
                    )
                    .child(
                        div()
                            .id("quick-settings")
                            .w(px(22.))
                            .h(px(18.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .when(m.quick_settings.is_some(), |d| d.bg(rgb(p.surface)))
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(icon(
                                "settings",
                                if m.quick_settings.is_some() { p.accent } else { p.muted },
                                13.5,
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                toggle_quick_settings(this.model.clone(), cx)
                            })),
                    ),
            )
    }
}
struct Dock {
    model: Entity<Model>,
    hovered: Option<&'static str>,
}
fn activate_pin(model: &Entity<Model>, id: &str, cx: &mut App) {
    let m = model.read(cx);
    if let Some(app) = m.apps.iter().find(|a| a.id == id) {
        if let Some(client) = m
            .snapshot
            .clients
            .iter()
            .find(|c| app.matches_class(&c.class))
        {
            session::focus(client.address.clone())
        } else {
            session::launch(app.path.clone(), m.tx.clone())
        }
    }
}
impl Render for Dock {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let apps = m.apps.clone();
        let snapshot = m.snapshot.clone();
        let hovered = self.hovered;
        let mut pins: Vec<(&'static str, &'static str, Option<&'static str>)> =
            vec![("apps", "Applications", None)];
        if apps.iter().any(|a| a.id == "com.crowbaros.Files.desktop") {
            pins.push(("files", "Files", Some("com.crowbaros.Files.desktop")))
        }
        if apps.iter().any(|a| a.id == "chromium.desktop") {
            pins.push(("browser", "Chromium", Some("chromium.desktop")))
        }
        if apps.iter().any(|a| a.id == "foot.desktop") {
            pins.push(("terminal", "Terminal", Some("foot.desktop")))
        }
        if apps
            .iter()
            .any(|a| a.id == "com.crowbaros.StudioSettings.desktop")
        {
            pins.push((
                "settings",
                "Settings",
                Some("com.crowbaros.StudioSettings.desktop"),
            ))
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_end()
            .gap(px(5.))
            .pb(px(6.))
            .font_family("Manrope")
            .text_color(rgb(p.text))
            .when_some(hovered, |d, label| {
                d.child(
                    div()
                        .px(px(8.))
                        .py(px(3.))
                        .rounded(px(6.))
                        .bg(rgb(p.canvas))
                        .border_1()
                        .border_color(rgb(p.line))
                        .text_size(px(11.))
                        .text_color(rgb(p.text))
                        .child(label),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .px(px(5.))
                    .py(px(4.))
                    .rounded(px(11.))
                    .bg(rgb(p.canvas))
                    .border_1()
                    .border_color(rgb(p.line))
                    .children(pins.into_iter().map(|(ic, label, id)| {
                        let app = id.and_then(|id| apps.iter().find(|a| a.id == id));
                        let state = dock_pin_state(app, &snapshot);
                        let is_hovered = hovered == Some(label);
                        let is_focused = matches!(state, DockPinState::Focused { .. });
                        let icon_color = match state {
                            DockPinState::Focused { .. } => p.focus,
                            DockPinState::Running { .. } => p.text,
                            DockPinState::Idle => {
                                if is_hovered {
                                    p.text
                                } else {
                                    p.muted
                                }
                            }
                        };
                        let indicator = div()
                            .h(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .map(|slot| match state {
                                DockPinState::Focused { attention } => slot.child(
                                    div()
                                        .w(px(12.))
                                        .h(px(2.))
                                        .rounded(px(1.))
                                        .bg(rgb(if attention { p.accent } else { p.focus })),
                                ),
                                DockPinState::Running { attention: true } => slot.child(
                                    div()
                                        .w(px(12.))
                                        .h(px(2.))
                                        .rounded(px(1.))
                                        .bg(rgb(p.accent)),
                                ),
                                DockPinState::Running { attention: false } => slot.child(
                                    div()
                                        .size(px(4.))
                                        .rounded_full()
                                        .bg(rgb(p.muted)),
                                ),
                                DockPinState::Idle => slot,
                            });
                        div()
                            .id(ic)
                            .w(px(36.))
                            .h(px(28.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(2.))
                            .rounded(px(7.))
                            .cursor_pointer()
                            .when(is_focused, |d| d.bg(rgb(p.surface)))
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(icon(ic, icon_color, 17.))
                            .child(indicator)
                            .on_hover(cx.listener(move |this, entered: &bool, _, cx| {
                                let next = if *entered {
                                    Some(label)
                                } else if this.hovered == Some(label) {
                                    None
                                } else {
                                    this.hovered
                                };
                                if this.hovered != next {
                                    this.hovered = next;
                                    cx.notify();
                                }
                            }))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(id) = id {
                                    activate_pin(&this.model, id, cx)
                                } else {
                                    toggle_launcher(this.model.clone(), cx)
                                }
                            }))
                    })),
            )
    }
}
struct Launcher {
    model: Entity<Model>,
    input: Entity<input::TextInput>,
    apps: Vec<AppEntry>,
    query: String,
    selected: usize,
    error: Option<String>,
    busy: bool,
    _subscription: Subscription,
}
impl Launcher {
    fn results(&self) -> Vec<AppEntry> {
        catalog::ranked(&self.apps, &self.query)
            .into_iter()
            .map(|i| self.apps[i].clone())
            .collect()
    }
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model.update(cx, |m, _| m.launcher = None);
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
        if let Some(app) = self.results().get(self.selected) {
            let m = self.model.read(cx);
            if let Some(client) = m
                .snapshot
                .clients
                .iter()
                .find(|c| app.matches_class(&c.class))
            {
                let address = client.address.clone();
                self.close(window, cx);
                session::focus(address);
            } else {
                self.busy = true;
                self.error = None;
                session::launch(app.path.clone(), m.tx.clone());
                cx.notify();
            }
        }
    }
}
fn palette_keycap(label: &'static str, p: Palette) -> Div {
    div()
        .px(px(6.))
        .py(px(1.))
        .rounded(px(4.))
        .bg(rgb(p.surface))
        .border_1()
        .border_color(rgb(p.line))
        .text_size(px(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(p.text))
        .child(label)
}
impl Render for Launcher {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.model.read(cx).palette();
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
                            .when(total == 0, |d| {
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
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(4.))
                                            .child(palette_keycap("↑ ↓", p))
                                            .child("choose"),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(4.))
                                            .child(palette_keycap("Enter", p))
                                            .child("open"),
                                    )
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
fn close_launcher(model: &Entity<Model>, cx: &mut App) {
    if let Some(w) = model.read(cx).launcher {
        let _ = w.update(cx, |view, w, cx| view.close(w, cx));
    }
}
fn toggle_launcher(model: Entity<Model>, cx: &mut App) {
    if model.read(cx).launcher.is_some() {
        log::info!("SHELL launcher open=false");
        close_launcher(&model, cx);
        return;
    }
    close_quick_settings(&model, cx);
    close_calendar(&model, cx);
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
        Ok(w) => model.update(cx, |m, _| m.launcher = Some(w)),
        Err(e) => log::error!("Opening launcher: {e}"),
    }
}

struct QuickSettingsPopover {
    model: Entity<Model>,
    focus: FocusHandle,
    confirm_power: Option<quick_settings::PowerConfirm>,
    busy: bool,
    error: Option<String>,
}

impl QuickSettingsPopover {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.read(cx).quick_settings.is_some() {
            log::info!("SHELL quick-settings open=false");
        }
        self.model.update(cx, |m, _| m.quick_settings = None);
        window.remove_window();
        cx.refresh_windows();
    }

    fn run_audio_action(
        &mut self,
        op: impl FnOnce() -> Result<quick_settings::AudioQuickState, String> + Send + 'static,
        kind: quick_settings::OsdKind,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let tx = self.model.read(cx).tx.clone();
        let task = cx.background_executor().spawn(async move { op() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(audio) => {
                        let (percent, muted) = match kind {
                            quick_settings::OsdKind::Microphone => (audio.volume, audio.mic_muted),
                            _ => (audio.volume, audio.muted),
                        };
                        this.model.update(cx, |m, _| m.quick_status.audio = audio);
                        let _ = tx.send(session::Event::Osd {
                            kind,
                            percent,
                            muted,
                        });
                    }
                    Err(err) => {
                        this.error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn run_brightness_action(
        &mut self,
        percent: u32,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let tx = self.model.read(cx).tx.clone();
        let task = cx
            .background_executor()
            .spawn(async move { quick_settings::set_brightness(percent) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(brightness) => {
                        let pct = brightness.percent;
                        this.model.update(cx, |m, _| m.quick_status.brightness = brightness);
                        let _ = tx.send(session::Event::Osd {
                            kind: quick_settings::OsdKind::Brightness,
                            percent: pct,
                            muted: false,
                        });
                    }
                    Err(err) => {
                        this.error = Some(err);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for QuickSettingsPopover {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let qs = m.quick_status.clone();
        let audio = &qs.audio;
        let wifi = &qs.wifi;
        let bt = &qs.bluetooth;
        let brightness = &qs.brightness;
        let confirm_power = self.confirm_power;
        let error = self.error.clone();

        let wifi_label = if !wifi.available {
            "Unavailable".to_string()
        } else if let Some(ssid) = &wifi.ssid {
            ssid.clone()
        } else if wifi.powered {
            "On".to_string()
        } else {
            "Off".to_string()
        };
        let bt_label = if !bt.available {
            "Unavailable"
        } else if bt.powered {
            "On"
        } else {
            "Off"
        };

        let vol_pct = audio.volume;
        let vol_muted = audio.muted;
        let vol_steps = ((vol_pct.min(100) as f32) / 5.0).round() as usize;
        let mut vol_track = div()
            .flex_1()
            .h(px(10.))
            .rounded_full()
            .overflow_hidden()
            .bg(rgb(p.surface))
            .border_1()
            .border_color(rgb(p.line))
            .flex()
            .items_center();
        for step in 1..=20usize {
            let target = (step * 5) as u32;
            let active = audio.available && !vol_muted && step <= vol_steps;
            vol_track = vol_track.child(
                div()
                    .id(("qs-vol-step", step))
                    .flex_1()
                    .h_full()
                    .bg(rgb(if active { p.accent } else { p.surface }))
                    .when(audio.available, |d| {
                        d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                            this.run_audio_action(
                                move || quick_settings::set_volume(target),
                                quick_settings::OsdKind::Volume,
                                cx,
                            );
                        }))
                    }),
            );
        }

        let mut panel = div()
            .id("quick-settings-panel")
            .w(px(344.))
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
            // Header
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .font_weight(FontWeight::BOLD)
                            .child("Quick Settings"),
                    )
                    .child(
                        div()
                            .id("qs-open-studio-settings")
                            .h(px(26.))
                            .px(px(8.))
                            .rounded(px(6.))
                            .bg(rgb(p.surface))
                            .border_1()
                            .border_color(rgb(p.line))
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.86))
                            .text_size(px(11.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(icon("settings", p.muted, 12.5))
                            .child("Studio Settings")
                            .on_click(cx.listener(|this, _, w, cx| {
                                let model = this.model.clone();
                                this.close(w, cx);
                                activate_pin(&model, "com.crowbaros.StudioSettings.desktop", cx);
                            })),
                    ),
            )
            // Connectivity row: Wi-Fi + Bluetooth
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child({
                        let wifi_avail = wifi.available;
                        let wifi_next = !wifi.powered;
                        let wifi_active = wifi_avail && wifi.powered;
                        div()
                            .id("qs-wifi-tile")
                            .flex_1()
                            .min_w_0()
                            .p(px(10.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(rgb(if wifi_active { p.accent } else { p.line }))
                            .bg(rgb(if wifi_active { p.accent } else { p.surface }))
                            .text_color(rgb(if wifi_active { p.on_accent } else { p.text }))
                            .when(!wifi_avail, |d| d.opacity(0.65))
                            .when(wifi_avail, |d| d.cursor_pointer().hover(|s| s.opacity(0.88)))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Wi-Fi"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(if wifi_active { p.on_accent } else { p.muted }))
                                    .text_ellipsis()
                                    .child(wifi_label),
                            )
                            .when(wifi_avail, |d| {
                                d.on_click(cx.listener(move |this, _, _, cx| {
                                    if this.busy {
                                        return;
                                    }
                                    this.busy = true;
                                    this.error = None;
                                    cx.notify();
                                    let task = cx.background_executor().spawn(async move {
                                        quick_settings::set_wifi_power(wifi_next)
                                    });
                                    cx.spawn(async move |this, cx| {
                                        let result = task.await;
                                        let _ = this.update(cx, |this, cx| {
                                            this.busy = false;
                                            match result {
                                                Ok(w) => this.model.update(cx, |m, _| m.quick_status.wifi = w),
                                                Err(e) => this.error = Some(e),
                                            }
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }))
                            })
                    })
                    .child({
                        let bt_avail = bt.available;
                        let bt_next = !bt.powered;
                        let bt_active = bt_avail && bt.powered;
                        div()
                            .id("qs-bt-tile")
                            .flex_1()
                            .min_w_0()
                            .p(px(10.))
                            .rounded(px(8.))
                            .border_1()
                            .border_color(rgb(if bt_active { p.accent } else { p.line }))
                            .bg(rgb(if bt_active { p.accent } else { p.surface }))
                            .text_color(rgb(if bt_active { p.on_accent } else { p.text }))
                            .when(!bt_avail, |d| d.opacity(0.65))
                            .when(bt_avail, |d| d.cursor_pointer().hover(|s| s.opacity(0.88)))
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Bluetooth"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(rgb(if bt_active { p.on_accent } else { p.muted }))
                                    .child(bt_label),
                            )
                            .when(bt_avail, |d| {
                                d.on_click(cx.listener(move |this, _, _, cx| {
                                    if this.busy {
                                        return;
                                    }
                                    this.busy = true;
                                    this.error = None;
                                    cx.notify();
                                    let task = cx.background_executor().spawn(async move {
                                        quick_settings::set_bluetooth_power(bt_next)
                                    });
                                    cx.spawn(async move |this, cx| {
                                        let result = task.await;
                                        let _ = this.update(cx, |this, cx| {
                                            this.busy = false;
                                            match result {
                                                Ok(b) => this.model.update(cx, |m, _| m.quick_status.bluetooth = b),
                                                Err(e) => this.error = Some(e),
                                            }
                                            cx.notify();
                                        });
                                    })
                                    .detach();
                                }))
                            })
                    }),
            )
            // Volume slider + mute + mic mute
            .child(
                div()
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(rgb(p.surface))
                    .border_1()
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
                                    .child(if audio.available {
                                        audio.sink_name.clone()
                                    } else {
                                        "Audio unavailable".into()
                                    }),
                            )
                            .child(
                                div()
                                    .font_family("IBM Plex Mono")
                                    .text_size(px(11.))
                                    .text_color(rgb(p.muted))
                                    .child(if !audio.available {
                                        "—".into()
                                    } else if vol_muted {
                                        "Muted".into()
                                    } else {
                                        format!("{vol_pct}%")
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .id("qs-vol-down")
                                    .w(px(26.))
                                    .h(px(24.))
                                    .rounded(px(6.))
                                    .bg(rgb(p.canvas))
                                    .border_1()
                                    .border_color(rgb(p.line))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(audio.available, |d| {
                                        d.cursor_pointer()
                                            .hover(|s| s.opacity(0.85))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.run_audio_action(
                                                    || quick_settings::step_volume(-5),
                                                    quick_settings::OsdKind::Volume,
                                                    cx,
                                                );
                                            }))
                                    })
                                    .child("−"),
                            )
                            .child(vol_track)
                            .child(
                                div()
                                    .id("qs-vol-up")
                                    .w(px(26.))
                                    .h(px(24.))
                                    .rounded(px(6.))
                                    .bg(rgb(p.canvas))
                                    .border_1()
                                    .border_color(rgb(p.line))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .when(audio.available, |d| {
                                        d.cursor_pointer()
                                            .hover(|s| s.opacity(0.85))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.run_audio_action(
                                                    || quick_settings::step_volume(5),
                                                    quick_settings::OsdKind::Volume,
                                                    cx,
                                                );
                                            }))
                                    })
                                    .child("+"),
                            )
                            .child(
                                div()
                                    .id("qs-vol-mute")
                                    .h(px(24.))
                                    .px(px(8.))
                                    .rounded(px(6.))
                                    .bg(rgb(if vol_muted { p.accent } else { p.canvas }))
                                    .border_1()
                                    .border_color(rgb(if vol_muted { p.accent } else { p.line }))
                                    .text_color(rgb(if vol_muted { p.on_accent } else { p.text }))
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .flex()
                                    .items_center()
                                    .when(audio.available, |d| {
                                        d.cursor_pointer()
                                            .hover(|s| s.opacity(0.85))
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.run_audio_action(
                                                    quick_settings::toggle_mute,
                                                    quick_settings::OsdKind::Volume,
                                                    cx,
                                                );
                                            }))
                                    })
                                    .child(if vol_muted { "Unmute" } else { "Mute" }),
                            ),
                    )
                    .child(
                        div()
                            .pt(px(4.))
                            .border_t_1()
                            .border_color(rgb(p.line))
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .text_color(rgb(p.muted))
                                    .child(if audio.mic_available {
                                        "Microphone"
                                    } else {
                                        "No microphone connected"
                                    }),
                            )
                            .when(audio.mic_available, |d| {
                                let mic_muted = audio.mic_muted;
                                d.child(
                                    div()
                                        .id("qs-mic-mute")
                                        .h(px(22.))
                                        .px(px(8.))
                                        .rounded(px(6.))
                                        .bg(rgb(if mic_muted { p.accent } else { p.canvas }))
                                        .border_1()
                                        .border_color(rgb(if mic_muted { p.accent } else { p.line }))
                                        .text_color(rgb(if mic_muted { p.on_accent } else { p.text }))
                                        .text_size(px(11.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .flex()
                                        .items_center()
                                        .cursor_pointer()
                                        .hover(|s| s.opacity(0.85))
                                        .child(if mic_muted { "Mic muted" } else { "Mute mic" })
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.run_audio_action(
                                                quick_settings::toggle_mic_mute,
                                                quick_settings::OsdKind::Microphone,
                                                cx,
                                            );
                                        })),
                                )
                            }),
                    ),
            );

        // Brightness section (shown only when /sys/class/backlight/* exists)
        if brightness.available {
            let b_pct = brightness.percent;
            let b_steps = ((b_pct.min(100) as f32) / 5.0).round() as usize;
            let mut b_track = div()
                .flex_1()
                .h(px(10.))
                .rounded_full()
                .overflow_hidden()
                .bg(rgb(p.canvas))
                .border_1()
                .border_color(rgb(p.line))
                .flex()
                .items_center();
            for step in 1..=20usize {
                let target = ((step * 5) as u32).max(5);
                let active = step <= b_steps;
                b_track = b_track.child(
                    div()
                        .id(("qs-bright-step", step))
                        .flex_1()
                        .h_full()
                        .bg(rgb(if active { p.accent } else { p.canvas }))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.run_brightness_action(target, cx);
                        })),
                );
            }
            panel = panel.child(
                div()
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(rgb(p.surface))
                    .border_1()
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
                                    .child("Display brightness"),
                            )
                            .child(
                                div()
                                    .font_family("IBM Plex Mono")
                                    .text_size(px(11.))
                                    .text_color(rgb(p.muted))
                                    .child(format!("{b_pct}%")),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(b_track),
                    ),
            );
        }

        if let Some(err) = error {
            panel = panel.child(
                div()
                    .text_size(px(11.))
                    .text_color(rgb(p.muted))
                    .child(err),
            );
        }

        // Power row (with inline confirmation on Reboot / Power Off)
        if let Some(confirm) = confirm_power {
            panel = panel.child(
                div()
                    .p(px(10.))
                    .rounded(px(8.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.focus))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(confirm.prompt()),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(6.))
                            .child(
                                div()
                                    .id("qs-power-cancel")
                                    .h(px(26.))
                                    .px(px(8.))
                                    .rounded(px(6.))
                                    .bg(rgb(p.canvas))
                                    .border_1()
                                    .border_color(rgb(p.line))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .text_size(px(11.5))
                                    .child("Cancel")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_power = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                div()
                                    .id("qs-power-confirm")
                                    .h(px(26.))
                                    .px(px(10.))
                                    .rounded(px(6.))
                                    .bg(rgb(p.accent))
                                    .text_color(rgb(p.on_accent))
                                    .flex()
                                    .items_center()
                                    .cursor_pointer()
                                    .text_size(px(11.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(confirm.confirm_label())
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        let action = confirm.action_name();
                                        this.confirm_power = None;
                                        this.close(w, cx);
                                        std::thread::spawn(move || {
                                            if let Err(e) = quick_settings::run_power_action(action) {
                                                log::warn!("SHELL power action={action} failed: {e}");
                                            }
                                        });
                                    })),
                            ),
                    ),
            );
        } else {
            let power_btn = |id: &'static str, label: &'static str| {
                div()
                    .id(id)
                    .flex_1()
                    .h(px(30.))
                    .rounded(px(7.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.85))
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(label)
            };
            panel = panel.child(
                div()
                    .flex()
                    .gap(px(6.))
                    .child(power_btn("qs-lock", "Lock").on_click(cx.listener(|this, _, w, cx| {
                        this.close(w, cx);
                        session::lock(true);
                    })))
                    .child(power_btn("qs-suspend", "Suspend").on_click(cx.listener(
                        |this, _, w, cx| {
                            this.close(w, cx);
                            std::thread::spawn(|| {
                                if let Err(e) = quick_settings::run_power_action("suspend") {
                                    log::warn!("SHELL power action=suspend failed: {e}");
                                }
                            });
                        },
                    )))
                    .child(power_btn("qs-reboot", "Reboot").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.confirm_power = Some(quick_settings::PowerConfirm::Reboot);
                            cx.notify();
                        },
                    )))
                    .child(power_btn("qs-poweroff", "Power Off").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.confirm_power = Some(quick_settings::PowerConfirm::PowerOff);
                            cx.notify();
                        },
                    ))),
            );
        }

        div()
            .id("quick-settings-backdrop")
            .key_context("QuickSettings")
            .track_focus(&self.focus)
            .size_full()
            .pt(px(BAR_HEIGHT + 8.))
            .pr(px(12.))
            .flex()
            .items_start()
            .justify_end()
            .font_family("Manrope")
            .on_action(cx.listener(|this, _: &Close, w, cx| this.close(w, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, w, cx| this.close(w, cx)),
            )
            .child(panel)
    }
}

fn close_quick_settings(model: &Entity<Model>, cx: &mut App) {
    if let Some(w) = model.read(cx).quick_settings {
        let _ = w.update(cx, |view, w, cx| view.close(w, cx));
    }
}

fn toggle_quick_settings(model: Entity<Model>, cx: &mut App) {
    if model.read(cx).quick_settings.is_some() {
        close_quick_settings(&model, cx);
        return;
    }
    close_launcher(&model, cx);
    close_calendar(&model, cx);
    log::info!("SHELL quick-settings open=true");
    let tx = model.read(cx).tx.clone();
    std::thread::spawn(move || {
        let status = quick_settings::fetch_status();
        let _ = tx.send(session::Event::QuickStatus(status));
    });
    let model2 = model.clone();
    let result = cx.open_window(
        layer(
            "quick-settings",
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
            cx.new(|_| QuickSettingsPopover {
                model: model2,
                focus,
                confirm_power: None,
                busy: false,
                error: None,
            })
        },
    );
    match result {
        Ok(w) => {
            model.update(cx, |m, _| m.quick_settings = Some(w));
            cx.refresh_windows();
        }
        Err(e) => log::error!("Opening quick settings: {e}"),
    }
}

struct CalendarPopover {
    model: Entity<Model>,
    focus: FocusHandle,
    month: calendar::CalendarMonth,
}

impl CalendarPopover {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.read(cx).calendar.is_some() {
            log::info!("SHELL calendar open=false");
        }
        self.model.update(cx, |m, _| m.calendar = None);
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
        let cells = calendar::month_grid(self.month.year, self.month.month, today);

        let mut grid = div().flex().flex_col().gap(px(4.));
        // Weekday header row
        grid = grid.child(
            div().flex().children(calendar::WEEKDAY_HEADERS.into_iter().map(|day| {
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
            })),
        );
        for week in cells.chunks(7) {
            grid = grid.child(
                div().flex().gap(px(2.)).children(week.iter().map(|cell| {
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
                })),
            );
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
            // Month header + navigation
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
                                this.month = calendar::CalendarMonth::current();
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
            // Compact quick timer
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
                            .child(
                                div()
                                    .flex()
                                    .gap(px(4.))
                                    .children(calendar::TIMER_PRESETS_SECS.into_iter().map(
                                        |secs| {
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
                                                    this.model.update(cx, |m, _| {
                                                        m.timer.select_preset(secs)
                                                    });
                                                    cx.notify();
                                                }))
                                        },
                                    )),
                            )
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

fn close_calendar(model: &Entity<Model>, cx: &mut App) {
    if let Some(w) = model.read(cx).calendar {
        let _ = w.update(cx, |view, w, cx| view.close(w, cx));
    }
}

fn toggle_calendar(model: Entity<Model>, cx: &mut App) {
    if model.read(cx).calendar.is_some() {
        close_calendar(&model, cx);
        return;
    }
    close_launcher(&model, cx);
    close_quick_settings(&model, cx);
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
                month: calendar::CalendarMonth::current(),
            })
        },
    );
    match result {
        Ok(w) => {
            model.update(cx, |m, _| m.calendar = Some(w));
            cx.refresh_windows();
        }
        Err(e) => log::error!("Opening calendar: {e}"),
    }
}

struct OsdOverlay {
    model: Entity<Model>,
}

impl Render for OsdOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let Some(osd) = m.osd_state.clone() else {
            return div().size_full();
        };
        let fill_pct = if osd.muted { 0 } else { osd.percent.min(100) };
        let bar_w = 104.0f32;
        let fill_w = (bar_w * (fill_pct as f32 / 100.0)).round();

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .font_family("Manrope")
            .child(
                div()
                    .w(px(244.))
                    .h(px(40.))
                    .px(px(12.))
                    .rounded(px(10.))
                    .bg(rgb(p.canvas))
                    .border_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(
                        div()
                            .w(px(66.))
                            .text_size(px(11.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(p.text))
                            .child(osd.kind.label(osd.muted)),
                    )
                    .child(
                        div()
                            .w(px(bar_w))
                            .h(px(7.))
                            .rounded_full()
                            .overflow_hidden()
                            .bg(rgb(p.surface))
                            .border_1()
                            .border_color(rgb(p.line))
                            .child(div().h_full().w(px(fill_w)).bg(rgb(p.accent))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .justify_end()
                            .font_family("IBM Plex Mono")
                            .text_size(px(11.))
                            .text_color(rgb(p.muted))
                            .child(osd.value_text()),
                    ),
            )
    }
}

fn close_osd(model: &Entity<Model>, cx: &mut App) {
    if let Some(w) = model.update(cx, |m, _| {
        m.osd_state = None;
        m.osd.take()
    }) {
        log::info!("SHELL osd show=false");
        let _ = w.update(cx, |_, w, _| w.remove_window());
    }
}

fn show_osd(
    model: &Entity<Model>,
    kind: quick_settings::OsdKind,
    percent: u32,
    muted: bool,
    cx: &mut App,
) {
    let seq = model.update(cx, |m, _| {
        m.osd_seq = m.osd_seq.wrapping_add(1);
        m.osd_state = Some(quick_settings::OsdState::new(
            kind,
            percent,
            muted,
            Instant::now(),
            m.osd_seq,
        ));
        m.osd_seq
    });
    log::info!(
        "SHELL osd show=true kind={} percent={} muted={}",
        kind.log_name(),
        percent,
        muted
    );
    if let Some(w) = model.read(cx).osd {
        let _ = w.update(cx, |_, _, cx| cx.notify());
    } else {
        let model2 = model.clone();
        match cx.open_window(
            layer_with_margin(
                "osd",
                Layer::Overlay,
                Anchor::BOTTOM,
                248.,
                44.,
                Some(-1.),
                Some((px(0.), px(0.), px(DOCK_HEIGHT + 20.), px(0.))),
                KeyboardInteractivity::None,
            ),
            move |_, cx| cx.new(|_| OsdOverlay { model: model2 }),
        ) {
            Ok(w) => model.update(cx, |m, _| m.osd = Some(w)),
            Err(e) => log::error!("Opening OSD overlay: {e}"),
        }
    }
    let dismiss_model = model.clone();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(quick_settings::OSD_DURATION)
            .await;
        let _ = cx.update(|cx| {
            let should_close = dismiss_model
                .read(cx)
                .osd_state
                .as_ref()
                .is_some_and(|osd| osd.sequence == seq && osd.is_expired(Instant::now()));
            if should_close || dismiss_model.read(cx).osd_seq == seq {
                close_osd(&dismiss_model, cx);
            }
        });
    })
    .detach();
}

fn handle_media_key(model: &Entity<Model>, key: session::MediaKey, cx: &mut App) {
    let tx = model.read(cx).tx.clone();
    std::thread::spawn(move || match key {
        session::MediaKey::VolumeUp => {
            if let Ok(audio) = quick_settings::step_volume(5) {
                let _ = tx.send(session::Event::Osd {
                    kind: quick_settings::OsdKind::Volume,
                    percent: audio.volume,
                    muted: audio.muted,
                });
                let _ = tx.send(session::Event::QuickStatus(quick_settings::fetch_status()));
            }
        }
        session::MediaKey::VolumeDown => {
            if let Ok(audio) = quick_settings::step_volume(-5) {
                let _ = tx.send(session::Event::Osd {
                    kind: quick_settings::OsdKind::Volume,
                    percent: audio.volume,
                    muted: audio.muted,
                });
                let _ = tx.send(session::Event::QuickStatus(quick_settings::fetch_status()));
            }
        }
        session::MediaKey::VolumeMute => {
            if let Ok(audio) = quick_settings::toggle_mute() {
                let _ = tx.send(session::Event::Osd {
                    kind: quick_settings::OsdKind::Volume,
                    percent: audio.volume,
                    muted: audio.muted,
                });
                let _ = tx.send(session::Event::QuickStatus(quick_settings::fetch_status()));
            }
        }
        session::MediaKey::MicMute => {
            if let Ok(audio) = quick_settings::toggle_mic_mute() {
                let _ = tx.send(session::Event::Osd {
                    kind: quick_settings::OsdKind::Microphone,
                    percent: audio.volume,
                    muted: audio.mic_muted,
                });
                let _ = tx.send(session::Event::QuickStatus(quick_settings::fetch_status()));
            }
        }
        session::MediaKey::BrightnessUp => {
            if let Ok(b) = quick_settings::step_brightness(10) {
                let _ = tx.send(session::Event::Osd {
                    kind: quick_settings::OsdKind::Brightness,
                    percent: b.percent,
                    muted: false,
                });
                let _ = tx.send(session::Event::QuickStatus(quick_settings::fetch_status()));
            }
        }
        session::MediaKey::BrightnessDown => {
            if let Ok(b) = quick_settings::step_brightness(-10) {
                let _ = tx.send(session::Event::Osd {
                    kind: quick_settings::OsdKind::Brightness,
                    percent: b.percent,
                    muted: false,
                });
                let _ = tx.send(session::Event::QuickStatus(quick_settings::fetch_status()));
            }
        }
    });
}
/// What the summon gesture, Super+Space, does (ADR 0035). The shell holds
/// whether the Agent's surface is up, and the compositor says with each
/// gesture whether the keyboard is in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Gesture {
    /// Bring the surface up, with the keyboard.
    Show,
    /// The surface is up and the keyboard elsewhere: bring it back.
    TakeKeyboard,
    /// The keyboard is in the surface: put it away, and give the keyboard
    /// back to the window that had it.
    PutAway,
}
fn gesture_does(up: bool, keyboard_in_surface: bool) -> Gesture {
    match (up, keyboard_in_surface) {
        (false, _) => Gesture::Show,
        (true, false) => Gesture::TakeKeyboard,
        (true, true) => Gesture::PutAway,
    }
}
/// How wide the Agent's surface is on a display: about a third of it.
fn agent_surface_width(display_width: f32) -> f32 {
    (display_width / 3.).round()
}
const AGENT_HEADER_HEIGHT: f32 = 48.;
/// Keeps the `crowbar-shell`-only control-socket snapshot of the drawer and
/// mark up to date.
fn sync_agent_view(model: &Entity<Model>, cx: &App) {
    let m = model.read(cx);
    if let Ok(mut view) = m.agent_view.lock() {
        view.drawer_up = m.agent.is_some();
        view.mark_overlay = m.mark_overlay.is_some();
        view.mark_overlay_output = m.mark_overlay.as_ref().map(|(_, out)| out.clone());
        view.state = m.agent_state.clone();
    }
}
/// Opens, moves, or removes the floating fullscreen mark overlay to match
/// whether a fullscreen window is covering the bar while the mark is active.
/// Returns `true` when the overlay was opened, moved, or closed.
fn sync_mark_overlay(model: &Entity<Model>, cx: &mut App) -> bool {
    let (want_output, existing_output, mark) = {
        let m = model.read(cx);
        let want = agent::fullscreen_mark_overlay_output(
            m.agent_state.mark,
            &m.snapshot,
            m.agent.is_some(),
        );
        let existing = m.mark_overlay.as_ref().map(|(_, out)| out.clone());
        (want, existing, m.agent_state.mark)
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
                move |_, cx| cx.new(|_| MarkOverlay { model: overlay_model }),
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
/// The Agent's surface (ADR 0035): a drawer on the right of the display the
/// pointer was on at the summon, the display's full height, above every
/// window and taking no room from any. The summon gesture or its close
/// control puts it away; Escape does not. Putting the drawer away stops
/// nothing: a working turn finishes behind it and the mark shows `done`.
struct AgentSurface {
    model: Entity<Model>,
    input: Entity<input::TextInput>,
    scroll: ScrollHandle,
    submitted_draft: Option<String>,
    _input_sub: Subscription,
}
impl AgentSurface {
    /// Puts the surface away. The keyboard goes back to the window that had
    /// it, if the surface holds it. A working turn keeps going behind it.
    fn put_away(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    fn send(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).query();
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return;
        }
        let (tx, conversation) = self.model.update(cx, |m, _| {
            if !m.agent_state.can_send() {
                return (None, None);
            }
            if let Some(tx) = m.agent_tx.clone() {
                m.agent_state.sending = true;
                m.agent_state.error = None;
                (Some(tx), m.agent_state.conversation.clone())
            } else {
                (None, None)
            }
        });
        if let Some(tx) = tx {
            let message = trimmed.to_string();
            self.submitted_draft = Some(query);
            sync_agent_view(&self.model, cx);
            tx.send_message(conversation, message);
            cx.notify();
        }
    }
    fn stop_turn(&mut self, cx: &mut Context<Self>) {
        let (tx, conversation, turn) = {
            let m = self.model.read(cx);
            (
                m.agent_tx.clone(),
                m.agent_state.conversation.clone(),
                m.agent_state.current_turn(),
            )
        };
        if let (Some(tx), Some(conversation)) = (tx, conversation) {
            tx.stop_turn(conversation, turn);
        }
    }
}
impl Render for AgentSurface {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let mark = ImageSource::Resource(Resource::Embedded(
            bar_mark_asset(&m.preferences).into(),
        ));
        let state = m.agent_state.clone();
        let working = state.working();
        let draft = self.input.read(cx).query();
        let can_send = state.can_send() && !draft.trim().is_empty();
        self.input.update(cx, |input, _| input.palette = p);

        let header_sub = if let Some(model) = &state.model {
            model.name.clone()
        } else if !state.configured || !state.connected {
            "Offline".into()
        } else if state.no_harness {
            "Connected · No harness".into()
        } else {
            "No Model".into()
        };

        let mut body = div()
            .id("agent-conversation")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(px(16.))
            .py(px(16.))
            .flex()
            .flex_col()
            .gap(px(14.))
            .bg(rgb(p.canvas));

        if !state.turns.is_empty() {
            for turn in &state.turns {
                let mut turn_col = div().flex().flex_col().gap(px(8.));
                // Person's message.
                turn_col = turn_col.child(
                    div()
                        .w_full()
                        .px(px(12.))
                        .py(px(10.))
                        .rounded(px(8.))
                        .bg(rgb(p.surface))
                        .border_1()
                        .border_color(rgb(p.line))
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .child(
                            div()
                                .text_size(px(10.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(p.muted))
                                .child("You"),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .line_height(px(19.))
                                .text_color(rgb(p.text))
                                .child(turn.message.clone()),
                        ),
                );
                // Desktop context carried with the message.
                if let Some(ctx) = &turn.context {
                    let lines = agent::format_context_summary(ctx);
                    let mut ctx_box = div()
                        .w_full()
                        .pl(px(10.))
                        .py(px(4.))
                        .border_l_2()
                        .border_color(rgb(p.line))
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_size(px(10.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(p.muted))
                                .child("Desktop context"),
                        );
                    for line in lines {
                        ctx_box = ctx_box.child(
                            div()
                                .font_family("IBM Plex Mono")
                                .text_size(px(10.5))
                                .line_height(px(15.))
                                .text_color(rgb(p.muted))
                                .child(line),
                        );
                    }
                    turn_col = turn_col.child(ctx_box);
                } else {
                    turn_col = turn_col.child(
                        div()
                            .pl(px(10.))
                            .py(px(2.))
                            .border_l_2()
                            .border_color(rgb(p.line))
                            .text_size(px(10.5))
                            .text_color(rgb(p.muted))
                            .child("No desktop context available"),
                    );
                }
                // Agent's answer and turn status.
                let state_label = match &turn.state {
                    agent::TurnState::Working => Some("Working…"),
                    agent::TurnState::Done => None,
                    agent::TurnState::Stopped => Some("Stopped"),
                    agent::TurnState::Failed(_) => Some("Could not answer"),
                };
                let mut answer_box = div().w_full().flex().flex_col().gap(px(4.)).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(px(10.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(p.focus))
                                .child("Agent"),
                        )
                        .when_some(state_label, |row, label| {
                            row.child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(rgb(p.muted))
                                    .child(label),
                            )
                        }),
                );
                if !turn.answer.is_empty() {
                    answer_box = answer_box.child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(19.5))
                            .text_color(rgb(p.text))
                            .child(turn.answer.clone()),
                    );
                } else if matches!(turn.state, agent::TurnState::Working) {
                    answer_box = answer_box.child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(p.muted))
                            .child("Thinking…"),
                    );
                }
                if let Some(retry) = &turn.retry {
                    answer_box = answer_box.child(
                        div()
                            .text_size(px(11.5))
                            .line_height(px(16.))
                            .text_color(rgb(p.muted))
                            .child(retry.clone()),
                    );
                }
                if let agent::TurnState::Failed(err) = &turn.state {
                    answer_box = answer_box.child(
                        div()
                            .mt(px(2.))
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
                turn_col = turn_col.child(answer_box);
                body = body.child(turn_col);
            }
        } else if !state.configured || !state.connected {
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
                            .child("Agent is not reachable"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(rgb(p.muted))
                            .child(
                                "The Agent isn't running right now. The drawer will \
                                 reconnect automatically once it is.",
                            ),
                    ),
            );
        } else if state.no_harness {
            let detail = match &state.lever_version {
                Some(ver) => format!(
                    "Reached Lever {ver}, but no harness is started to answer yet."
                ),
                None => "Reached the Agent, but no harness is started to answer yet.".into(),
            };
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
                            .child("Agent is reachable — no harness yet"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(rgb(p.muted))
                            .child(detail),
                    ),
            );
        } else if state.model.is_none() {
            let reason = state.missing_model.clone().unwrap_or_else(|| {
                "Sign in to a Model from a terminal to talk with the Agent.".into()
            });
            body = body.child(
                div()
                    .pt(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("No Model signed in"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(rgb(p.muted))
                            .child(reason),
                    )
                    .child(
                        div()
                            .px(px(10.))
                            .py(px(7.))
                            .rounded(px(6.))
                            .bg(rgb(p.surface))
                            .border_1()
                            .border_color(rgb(p.line))
                            .font_family("IBM Plex Mono")
                            .text_size(px(11.5))
                            .text_color(rgb(p.text))
                            .child("crowbar-agent login"),
                    ),
            );
        } else {
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
                            .child("Start a conversation"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(rgb(p.muted))
                            .child(
                                "Each message carries your current desktop context: active window, \
                                 open windows by workspace, and displays — never window contents.",
                            ),
                    ),
            );
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context("AgentSurface")
            .on_action(cx.listener(|this, _: &SendMessage, _, cx| this.send(cx)))
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
                            .size(px(30.))
                            .rounded(px(10.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(icon("close", p.muted, 14.))
                            .on_click(cx.listener(|this, _, w, cx| this.put_away(w, cx))),
                    ),
            )
            .child(body)
            .when_some(
                state.error.clone().or_else(|| {
                    if state.turns.is_empty() {
                        None
                    } else if !state.configured || !state.connected {
                        Some("The Agent isn't running right now. Reconnecting automatically.".into())
                    } else if state.no_harness {
                        None
                    } else if state.model.is_none() {
                        Some(state.missing_model.clone().unwrap_or_else(|| {
                            "No Model is signed in. Run `crowbar-agent login` in a terminal.".into()
                        }))
                    } else {
                        None
                    }
                }),
                |d, err| {
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
            .child(
                div()
                    .flex_none()
                    .px(px(12.))
                    .py(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(rgb(p.line))
                    .child(
                        div()
                            .w_full()
                            .pl(px(10.))
                            .pr(px(6.))
                            .py(px(6.))
                            .rounded(px(8.))
                            .bg(rgb(p.surface))
                            .border_1()
                            .border_color(rgb(p.line))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().flex_1().min_w_0().child(self.input.clone()))
                            .when(working, |row| {
                                row.child(
                                    div()
                                        .id("stop-agent-turn")
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
                                        .on_click(cx.listener(|this, _, _, cx| this.stop_turn(cx))),
                                )
                            })
                            .when(!working, |row| {
                                row.child(
                                    div()
                                        .id("send-agent-message")
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
                                        .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .text_size(px(11.))
                            .text_color(rgb(p.muted))
                            .child(palette_keycap("Enter", p))
                            .child("send")
                            .child(div().ml(px(6.)).child(palette_keycap("Super", p)))
                            .child(palette_keycap("Space", p))
                            .child("put away"),
                    ),
            )
    }
}
/// Brings the Agent's surface up on the display the gesture named, a third
/// of it wide, and asks the compositor to give it the keyboard. The
/// compositor puts a surface of this namespace on the display the pointer
/// was on at the gesture.
fn show_agent_surface(model: &Entity<Model>, summon: &session::Summon, cx: &mut App) {
    let width = agent_surface_width(summon.width);
    log::info!(
        "SHELL agent-surface up=true output={} width={width}",
        summon.output
    );
    let surface = model.clone();
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
        move |w, cx| {
            let input = cx.new(|cx| input::TextInput::with_placeholder("Ask the Agent…", 16384, cx));
            w.focus(&input.focus_handle(cx));
            let scroll = ScrollHandle::new();
            cx.new(|cx| {
                let input_sub = cx.observe(&input, |_, _, cx| cx.notify());
                AgentSurface {
                    model: surface,
                    input,
                    scroll,
                    submitted_draft: None,
                    _input_sub: input_sub,
                }
            })
        },
    );
    match result {
        Ok(w) => {
            let prev_mark = model.read(cx).agent_state.mark;
            let (tx, cleared) = model.update(cx, |m, _| {
                m.agent = Some(w);
                let cleared = m.agent_state.on_drawer_opened();
                (m.agent_tx.clone(), cleared)
            });
            if cleared {
                log::info!(
                    "SHELL agent-mark state={} prev={}",
                    model.read(cx).agent_state.mark.label(),
                    prev_mark.label()
                );
            }
            if let Some(tx) = tx {
                tx.refresh_status();
            }
            sync_mark_overlay(model, cx);
            session::agent_keyboard(true);
            cx.refresh_windows();
        }
        Err(e) => log::error!("Summoning the Agent's surface: {e}"),
    }
}
/// The summon gesture: the surface comes up, takes the keyboard back, or is
/// put away.
fn summoned(model: &Entity<Model>, summon: session::Summon, cx: &mut App) {
    // The launcher holds the keyboard while it is open, over whatever the
    // gesture gives it to.
    close_launcher(model, cx);
    // A surface the compositor closed, as one whose display went away, is
    // no longer up.
    let agent = model.read(cx).agent;
    let up = agent.is_some_and(|w| w.update(cx, |_, _, _| ()).is_ok());
    if !up {
        model.update(cx, |m, _| m.agent = None);
    }
    match gesture_does(up, summon.keyboard) {
        Gesture::Show => show_agent_surface(model, &summon, cx),
        Gesture::TakeKeyboard => {
            log::info!("SHELL agent-surface keyboard=true");
            if let Some(w) = agent {
                let _ = w.update(cx, |view, w, cx| {
                    w.focus(&view.input.focus_handle(cx));
                });
            }
            if let Some(tx) = model.read(cx).agent_tx.clone() {
                tx.refresh_status();
            }
            session::agent_keyboard(true)
        }
        Gesture::PutAway => {
            if let Some(w) = agent {
                let _ = w.update(cx, |view, w, cx| view.put_away(w, cx));
            }
        }
    }
}
/// The lock screen: shown while the compositor has the session locked, and
/// all it shows (ADR 0032). Where an unlock helper checks the person's
/// password, its entry takes it at once, and Enter sends it to the helper,
/// which tells the compositor to unlock: the shell's own word is not enough.
/// Without one, Enter asks the compositor to unlock.
struct LockScreen {
    model: Entity<Model>,
    focus: FocusHandle,
    /// What the person typed, shown only as dots, and dropped once sent.
    password: String,
    /// Why the last password did not unlock.
    error: Option<String>,
    /// A password is with the helper.
    busy: bool,
}
impl LockScreen {
    const MAX_PASSWORD: usize = 1024;
    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if !session::unlocks_with_password() {
            session::lock(false);
            return;
        }
        if self.password.is_empty() {
            return;
        }
        let tx = self.model.read(cx).tx.clone();
        self.busy = true;
        self.error = None;
        session::unlock_with_password(std::mem::take(&mut self.password), tx);
        cx.notify();
    }
    fn typed(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if self.busy || !session::unlocks_with_password() {
            return;
        }
        let k = &event.keystroke;
        if k.modifiers.control || k.modifiers.alt || k.modifiers.platform || k.modifiers.function {
            return;
        }
        match k.key.as_str() {
            "backspace" => {
                self.password.pop();
            }
            "escape" => self.password.clear(),
            _ => match &k.key_char {
                Some(c)
                    if !c.chars().any(char::is_control)
                        && self.password.len() + c.len() <= Self::MAX_PASSWORD =>
                {
                    self.password.push_str(c)
                }
                _ => return,
            },
        }
        self.error = None;
        cx.notify();
    }
    /// The helper answered: a wrong password is said, and the entry is
    /// ready for another.
    fn answered(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        self.busy = false;
        self.error = result.err();
        cx.notify();
    }
}
const LOCK_GROUND: u32 = 0x030b13;
const LOCK_VOID: u32 = 0x080d12;
const LOCK_ABYSS: u32 = 0x17212b;
const LOCK_STEEL: u32 = 0x6d8191;
const LOCK_LINE: u32 = 0x364957;
const LOCK_FROST: u32 = 0xd8e5ec;
const LOCK_MUTED: u32 = 0xa8bac7;
const LOCK_ERROR: u32 = 0xff8a80;
const NIGHTWATCH_RATIO: f32 = 1672.0 / 941.0;
const LOGO_RATIO: f32 = 180.0 / 850.0;
const MAX_VISIBLE_DOTS: usize = 22;

#[derive(Clone, Copy, Debug, PartialEq)]
struct LockLayout {
    left_pad: f32,
    top_pad: f32,
    bottom_pad: f32,
    clock_size: f32,
    date_size: f32,
    login_gap: f32,
    field_width: f32,
    logo_width: f32,
    logo_height: f32,
}

impl LockLayout {
    fn for_viewport(width: f32, height: f32) -> Self {
        let w = if width > 0. { width } else { 1600. };
        let h = if height > 0. { height } else { 900. };
        let left_pad = (w * 0.082).clamp(48., 156.);
        let top_pad = (h * 0.145).clamp(40., 160.);
        let bottom_pad = (h * 0.042).clamp(24., 48.);
        let clock_size = (h * 0.138).clamp(64., 140.);
        let date_size = (h * 0.025).clamp(16., 25.);
        let login_gap = (h * 0.115).clamp(36., 120.);
        let field_width = (w - left_pad * 2.0).min(370.0).max(260.0);
        let logo_width = (w * 0.145).clamp(186., 236.);
        Self {
            left_pad,
            top_pad,
            bottom_pad,
            clock_size,
            date_size,
            login_gap,
            field_width,
            logo_width,
            logo_height: logo_width * LOGO_RATIO,
        }
    }
}

/// Keeps Nightwatch at full viewport height anchored to the right edge so the
/// crow and slate perch stay unclipped on 4:3, 16:9, and ultrawide displays.
fn nightwatch_size(viewport_w: f32, viewport_h: f32) -> Option<(f32, f32)> {
    if viewport_w <= 0. || viewport_h <= 0. {
        return None;
    }
    Some((viewport_h * NIGHTWATCH_RATIO, viewport_h))
}

fn visible_password_dots(password: &str) -> usize {
    password.chars().count().min(MAX_VISIBLE_DOTS)
}

fn is_service_account(name: &str) -> bool {
    name.is_empty() || name == "root" || name == "nobody" || name.starts_with("crowbar-")
}

fn gecos_display_name(gecos: &str) -> Option<String> {
    let first = gecos.split(',').next()?.trim();
    (!first.is_empty()).then(|| first.to_string())
}

fn person_label_from(
    env_person: Option<&str>,
    env_user: Option<&str>,
    passwd: Option<&str>,
) -> String {
    if let Some(name) = env_person.map(str::trim).filter(|s| !s.is_empty()) {
        return name.to_string();
    }
    let entries: Vec<(&str, u32, &str)> = passwd
        .unwrap_or("")
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let _pw = fields.next()?;
            let uid = fields.next()?.parse::<u32>().ok()?;
            let _gid = fields.next()?;
            let gecos = fields.next().unwrap_or("");
            Some((name, uid, gecos))
        })
        .collect();
    if let Some(user) = env_user.map(str::trim).filter(|u| !is_service_account(u)) {
        if let Some((_, _, gecos)) = entries.iter().find(|(n, _, _)| *n == user) {
            if let Some(display) = gecos_display_name(gecos) {
                return display;
            }
        }
        return user.to_string();
    }
    if let Some((name, _, gecos)) = entries
        .iter()
        .find(|(n, uid, _)| *uid >= 1000 && *uid < 65534 && !is_service_account(n))
    {
        return gecos_display_name(gecos).unwrap_or_else(|| (*name).to_string());
    }
    "Person".into()
}

fn person_label() -> String {
    let env_person = std::env::var("CROWBAR_PERSON").ok();
    let env_user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok();
    let passwd = std::fs::read_to_string("/etc/passwd").ok();
    person_label_from(env_person.as_deref(), env_user.as_deref(), passwd.as_deref())
}

impl Render for LockScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let studio = m.preferences.studio;
        let accent = p.accent;
        let lock_time = m.lock_time.clone();
        let lock_date = m.lock_date.clone();
        let person = m.person.clone();
        let with_password = session::unlocks_with_password();
        let has_password = !self.password.is_empty();
        let dot_count = visible_password_dots(&self.password);
        let busy = self.busy;
        let error = self.error.clone();
        let vp = window.viewport_size();
        let vp_w: f32 = vp.width.into();
        let vp_h: f32 = vp.height.into();
        let layout = LockLayout::for_viewport(vp_w, vp_h);
        let background = match nightwatch_size(vp_w, vp_h) {
            Some((bg_w, bg_h)) => img(asset("nightwatch.png"))
                .absolute()
                .top_0()
                .right_0()
                .w(px(bg_w))
                .h(px(bg_h))
                .object_fit(ObjectFit::Fill),
            None => img(asset("nightwatch.png"))
                .size_full()
                .object_fit(ObjectFit::Cover),
        };
        let logo = ImageSource::Resource(Resource::Embedded(
            if studio {
                "logo-horizontal-frost"
            } else {
                "logo-horizontal-primary"
            }
            .into(),
        ));
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .track_focus(&self.focus)
            .key_context("Lock")
            .on_action(cx.listener(|this, _: &Unlock, _, cx| this.submit(cx)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.typed(event, cx)))
            .bg(rgb(LOCK_GROUND))
            .text_color(rgb(LOCK_FROST))
            .font_family("Manrope")
            .child(div().absolute().inset_0().overflow_hidden().child(background))
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .flex_col()
                    .justify_between()
                    .pl(px(layout.left_pad))
                    .pr(px(layout.left_pad))
                    .pt(px(layout.top_pad))
                    .pb(px(layout.bottom_pad))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_size(px(layout.clock_size))
                                            .font_weight(FontWeight::NORMAL)
                                            .line_height(relative(1.0))
                                            .text_color(rgb(LOCK_FROST))
                                            .child(lock_time),
                                    )
                                    .child(
                                        div()
                                            .mt(px(16.))
                                            .text_size(px(layout.date_size))
                                            .text_color(rgb(LOCK_MUTED))
                                            .child(lock_date),
                                    ),
                            )
                            .child(
                                div()
                                    .mt(px(layout.login_gap))
                                    .w(px(layout.field_width))
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(15.))
                                            .mb(px(22.))
                                            .child(
                                                div()
                                                    .size(px(44.))
                                                    .rounded_full()
                                                    .bg(rgb(LOCK_ABYSS))
                                                    .border_1()
                                                    .border_color(rgb(LOCK_LINE))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .child(icon("user", LOCK_FROST, 20.)),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(20.))
                                                    .text_color(rgb(LOCK_FROST))
                                                    .child(person),
                                            ),
                                    )
                                    .when(with_password, |d| {
                                        let border = if error.is_some() {
                                            LOCK_ERROR
                                        } else if busy {
                                            LOCK_LINE
                                        } else {
                                            accent
                                        };
                                        let can_submit = has_password && !busy;
                                        d.child(
                                            div()
                                                .id("password")
                                                .w_full()
                                                .h(px(52.))
                                                .pl(px(17.))
                                                .pr(px(8.))
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .gap(px(10.))
                                                .rounded(px(10.))
                                                .bg(rgb(LOCK_VOID))
                                                .border_1()
                                                .border_color(rgb(border))
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .overflow_hidden()
                                                        .flex()
                                                        .items_center()
                                                        .when(!has_password, |field| {
                                                            field
                                                                .when(!busy, |f| {
                                                                    f.child(
                                                                        div()
                                                                            .w(px(2.))
                                                                            .h(px(19.))
                                                                            .mr(px(6.))
                                                                            .rounded(px(1.))
                                                                            .bg(rgb(accent)),
                                                                    )
                                                                })
                                                                .child(
                                                                    div()
                                                                        .text_size(px(14.))
                                                                        .text_color(rgb(LOCK_MUTED))
                                                                        .child(if busy {
                                                                            "Checking…"
                                                                        } else {
                                                                            "Password"
                                                                        }),
                                                                )
                                                        })
                                                        .when(has_password, |field| {
                                                            field
                                                                .gap(px(6.))
                                                                .children((0..dot_count).map(|_| {
                                                                    div()
                                                                        .size(px(7.))
                                                                        .rounded_full()
                                                                        .bg(rgb(LOCK_FROST))
                                                                }))
                                                                .when(!busy, |f| {
                                                                    f.child(
                                                                        div()
                                                                            .w(px(2.))
                                                                            .h(px(19.))
                                                                            .ml(px(2.))
                                                                            .rounded(px(1.))
                                                                            .bg(rgb(accent)),
                                                                    )
                                                                })
                                                        }),
                                                )
                                                .child(
                                                    div()
                                                        .id("unlock")
                                                        .size(px(36.))
                                                        .rounded(px(8.))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .when(can_submit, |btn| {
                                                            btn.bg(rgb(accent))
                                                                .cursor_pointer()
                                                                .hover(|s| s.opacity(0.85))
                                                                .child(icon("arrow", p.on_accent, 18.))
                                                        })
                                                        .when(!can_submit, |btn| {
                                                            btn.child(icon("arrow", LOCK_STEEL, 18.))
                                                        })
                                                        .on_click(cx.listener(|this, _, _, cx| {
                                                            this.submit(cx)
                                                        })),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .mt(px(14.))
                                                .min_h(px(20.))
                                                .flex()
                                                .items_center()
                                                .gap(px(8.))
                                                .when_some(error.clone(), |row, message| {
                                                    row.child(
                                                        div()
                                                            .size(px(6.))
                                                            .rounded_full()
                                                            .bg(rgb(LOCK_ERROR)),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(13.))
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .text_color(rgb(LOCK_ERROR))
                                                            .child(message),
                                                    )
                                                })
                                                .when(error.is_none() && busy, |row| {
                                                    row.child(
                                                        div()
                                                            .size(px(6.))
                                                            .rounded_full()
                                                            .bg(rgb(accent)),
                                                    )
                                                    .child(
                                                        div()
                                                            .text_size(px(12.))
                                                            .text_color(rgb(LOCK_MUTED))
                                                            .child("Checking password…"),
                                                    )
                                                })
                                                .when(error.is_none() && !busy, |row| {
                                                    row.child(
                                                        div()
                                                            .text_size(px(12.))
                                                            .text_color(rgb(LOCK_MUTED))
                                                            .child("Sign in to your workspace"),
                                                    )
                                                }),
                                        )
                                    })
                                    .when(!with_password, |d| {
                                        d.child(
                                            div()
                                                .id("unlock")
                                                .w_full()
                                                .h(px(48.))
                                                .px(px(18.))
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .rounded(px(10.))
                                                .bg(rgb(accent))
                                                .text_color(rgb(p.on_accent))
                                                .cursor_pointer()
                                                .hover(|s| s.opacity(0.85))
                                                .child(
                                                    div()
                                                        .text_size(px(14.))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .child("Unlock"),
                                                )
                                                .child(icon("arrow", p.on_accent, 18.))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.submit(cx)
                                                })),
                                        )
                                        .child(
                                            div()
                                                .mt(px(14.))
                                                .text_size(px(12.))
                                                .text_color(rgb(LOCK_MUTED))
                                                .child("Press Enter to unlock"),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        div().child(
                            img(logo)
                                .w(px(layout.logo_width))
                                .h(px(layout.logo_height))
                                .object_fit(ObjectFit::Contain),
                        ),
                    ),
            )
    }
}
/// Whether a snapshot puts the lock screen up or takes it down. One that did
/// not reach the compositor says nothing about the lock: taken as unlocked,
/// it would take the lock screen down while the compositor stays locked,
/// and leave nothing on the screen.
fn lock_screen_changes(snapshot: &session::Snapshot, showing: bool) -> bool {
    snapshot.connected && snapshot.locked != showing
}

/// Shows the lock screen over every display, or takes it down.
fn set_lock_screen(model: &Entity<Model>, locked: bool, cx: &mut App) {
    if !locked {
        if let Some(w) = model.update(cx, |m, _| m.lock.take()) {
            log::info!("SHELL lock open=false");
            let _ = w.update(cx, |_, w, _| w.remove_window());
        }
        return;
    }
    if model.read(cx).lock.is_some() {
        return;
    }
    close_launcher(model, cx);
    close_quick_settings(model, cx);
    close_calendar(model, cx);
    close_osd(model, cx);
    log::info!("SHELL lock open=true");
    let screen = model.clone();
    let result = cx.open_window(
        layer(
            "lock",
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
            cx.new(|_| LockScreen {
                model: screen,
                focus,
                password: String::new(),
                error: None,
                busy: false,
            })
        },
    );
    match result {
        Ok(w) => model.update(cx, |m, _| m.lock = Some(w)),
        Err(e) => log::error!("Opening the lock screen: {e}"),
    }
}
/// Handles the shell's events. True if the windows must be drawn again: a
/// snapshot that is the same as the last draws nothing.
fn handle_events(model: &Entity<Model>, events: Vec<session::Event>, cx: &mut App) -> bool {
    let mut refresh = false;
    for e in events {
        match e {
            session::Event::Toggle => toggle_launcher(model.clone(), cx),
            session::Event::Summon(summon) => summoned(model, summon, cx),
            session::Event::MediaKey(key) => handle_media_key(model, key, cx),
            session::Event::QuickStatus(status) => {
                model.update(cx, |m, _| m.quick_status = status);
                if let Some(w) = model.read(cx).quick_settings {
                    let _ = w.update(cx, |_, _, cx| cx.notify());
                }
            }
            session::Event::Osd {
                kind,
                percent,
                muted,
            } => show_osd(model, kind, percent, muted, cx),
            session::Event::Quit => cx.quit(),
            session::Event::Snapshot(s) => {
                if model.read(cx).snapshot == s {
                    continue;
                }
                // A shell that starts while locked shows the lock screen too.
                let locked = s.locked;
                let lock_changes = lock_screen_changes(&s, model.read(cx).lock.is_some());
                model.update(cx, |m, _| m.snapshot = s);
                if lock_changes {
                    set_lock_screen(model, locked, cx);
                }
                sync_mark_overlay(model, cx);
                refresh = true
            }
            session::Event::Lock(locked) => {
                set_lock_screen(model, locked, cx);
                model.update(cx, |m, _| m.snapshot.locked = locked);
                sync_mark_overlay(model, cx);
            }
            session::Event::Agent(agent_event) => {
                let is_sent = matches!(agent_event, agent::AgentEvent::Sent { .. });
                let is_message = matches!(agent_event, agent::AgentEvent::Message { .. });
                let is_no_model = matches!(
                    &agent_event,
                    agent::AgentEvent::Error { code, .. } if code == "no-model"
                );
                let prev_mark = model.read(cx).agent_state.mark;
                let (changed, needs_status_refresh) = model.update(cx, |m, _| {
                    let up = m.agent.is_some();
                    let changed = m.agent_state.apply(agent_event, up);
                    let need_status = (is_sent && m.agent_state.model.is_none()) || is_no_model;
                    (changed, need_status)
                });
                if needs_status_refresh {
                    if let Some(tx) = model.read(cx).agent_tx.clone() {
                        tx.refresh_status();
                    }
                }
                let next_mark = model.read(cx).agent_state.mark;
                let mark_changed = next_mark != prev_mark;
                if mark_changed {
                    log::info!(
                        "SHELL agent-mark state={} prev={}",
                        next_mark.label(),
                        prev_mark.label()
                    );
                }
                let overlay_changed = sync_mark_overlay(model, cx);
                if is_sent {
                    if let Some(w) = model.read(cx).agent {
                        let _ = w.update(cx, |view, _, cx| {
                            if let Some(submitted) = view.submitted_draft.take() {
                                let current = view.input.read(cx).query();
                                if let Some(next) = agent::draft_after_send(&current, &submitted) {
                                    view.input.update(cx, |input, cx| input.set_text(next, cx));
                                }
                            }
                            view.scroll.scroll_to_bottom();
                            cx.notify();
                        });
                    }
                } else if is_message {
                    if let Some(w) = model.read(cx).agent {
                        let _ = w.update(cx, |view, _, cx| {
                            view.scroll.scroll_to_bottom();
                            cx.notify();
                        });
                    }
                }
                if mark_changed || overlay_changed {
                    refresh = true;
                } else if changed {
                    if let Some(w) = model.read(cx).agent {
                        let _ = w.update(cx, |_, _, cx| cx.notify());
                    }
                }
            }
            session::Event::Unlock(result) => {
                if let Some(w) = model.read(cx).lock {
                    let _ = w.update(cx, |v, _, cx| v.answered(result, cx));
                }
            }
            session::Event::Launch(result) => {
                if let Some(w) = model.read(cx).launcher {
                    match result {
                        Ok(()) => close_launcher(model, cx),
                        Err(error) => {
                            let _ = w.update(cx, |v, _, cx| {
                                v.busy = false;
                                v.error = Some(error);
                                cx.notify();
                            });
                        }
                    }
                } else if let Err(error) = result {
                    toggle_launcher(model.clone(), cx);
                    if let Some(w) = model.read(cx).launcher {
                        let _ = w.update(cx, |v, _, cx| {
                            v.error = Some(error);
                            cx.notify();
                        });
                    }
                }
            }
        }
    }
    refresh
}
/// Looks at the clock and the saved appearance. True if either changed what
/// the shell shows.
fn tick(model: &Entity<Model>, cx: &mut App) -> bool {
    let now = ClockDisplay::now();
    let prefs = Preferences::load();
    let (refresh, timer_changed, calendar_handle) = model.update(cx, |m, _| {
        let mut refresh = false;
        if m.clock != now.bar || m.lock_time != now.lock_time || m.lock_date != now.lock_date {
            m.clock = now.bar;
            m.lock_time = now.lock_time;
            m.lock_date = now.lock_date;
            refresh = true
        }
        if let Some(prefs) = prefs {
            if prefs != m.preferences {
                log::info!(
                    "SHELL appearance theme={:?} wallpaper={:?}",
                    prefs.theme,
                    prefs.wallpaper
                );
                m.preferences = prefs;
                refresh = true
            }
        }
        let timer_changed = m.timer.tick_one_second();
        (refresh, timer_changed, m.calendar)
    });
    if timer_changed {
        if let Some(w) = calendar_handle {
            let _ = w.update(cx, |_, _, cx| cx.notify());
        }
    }
    refresh
}
fn main() -> anyhow::Result<()> {
    env_logger::init();
    match std::env::args().nth(1).as_deref() {
        Some("--version") => {
            println!("crowbaros-shell {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--list-apps") => {
            println!("{}", serde_json::to_string_pretty(&catalog::discover())?);
            return Ok(());
        }
        Some("--launcher") => return session::send("launcher"),
        Some("--quit") => return session::send("quit"),
        Some("--start-session") => return session::start_session(),
        Some(_) => {
            anyhow::bail!("Use --version, --list-apps, --launcher, --quit or --start-session")
        }
        None => {}
    }
    session::connect();
    let (tx, rx) = session::channel();
    session::deliver_events_to(tx.clone());
    let agent_tx = agent::start_from_env(tx.clone());
    let agent_configured = agent_tx.is_some();
    let agent_view = Arc::new(Mutex::new(agent::AgentViewSnapshot {
        drawer_up: false,
        mark_overlay: false,
        mark_overlay_output: None,
        state: agent::AgentState::new(agent_configured),
    }));
    let _lock = session::server(tx.clone(), agent_view.clone())?;
    session::start_display_bridge();
    let watcher = tx.clone();
    std::thread::spawn(move || {
        let mut last = None;
        loop {
            let snapshot = match session::snapshot() {
                Ok(snapshot) => snapshot,
                Err(e) if session::closed() => {
                    // The wrapper that runs the shell connects again.
                    log::error!("SHELL exits: the compositor's control channel is closed: {e}");
                    std::process::exit(1);
                }
                Err(_) => session::Snapshot::default(),
            };
            if last.as_ref() != Some(&snapshot) {
                last = Some(snapshot.clone());
                if watcher.send(session::Event::Snapshot(snapshot)).is_err() {
                    break;
                }
            }
            // Events, as Super tapped alone or the compositor's snapshot, are
            // read as they come. Asking again is for a compositor that sends
            // no snapshots, and finds a closed channel.
            session::read_events_for(Duration::from_millis(700));
        }
    });
    // Before GPUI starts and makes the shell's layers.
    if let Err(e) = session::wait_for_a_display() {
        log::error!("SHELL exits: the compositor's control channel is closed: {e}");
        std::process::exit(1);
    }
    Application::new().with_assets(Assets).run(move |cx| {
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../fonts/Manrope-Regular.ttf")),
                Cow::Borrowed(include_bytes!("../fonts/Manrope-SemiBold.ttf")),
                Cow::Borrowed(include_bytes!("../fonts/Manrope-Bold.ttf")),
                Cow::Borrowed(include_bytes!("../fonts/IBMPlexMono-Regular.ttf")),
            ])
            .expect("Bundled fonts");
        input::bind(cx);
        cx.bind_keys([
            KeyBinding::new("down", Next, Some("Launcher")),
            KeyBinding::new("up", Previous, Some("Launcher")),
            KeyBinding::new("tab", Next, Some("Launcher")),
            KeyBinding::new("shift-tab", Previous, Some("Launcher")),
            KeyBinding::new("enter", Open, Some("Launcher")),
            KeyBinding::new("escape", Close, Some("Launcher")),
            KeyBinding::new("escape", Close, Some("QuickSettings")),
            KeyBinding::new("escape", Close, Some("Calendar")),
            KeyBinding::new("left", PrevMonth, Some("Calendar")),
            KeyBinding::new("right", NextMonth, Some("Calendar")),
            KeyBinding::new("enter", Unlock, Some("Lock")),
            KeyBinding::new("enter", SendMessage, Some("AgentSurface")),
        ]);
        let now = ClockDisplay::now();
        let model = cx.new(|_| Model {
            snapshot: Default::default(),
            preferences: {
                let preferences = Preferences::load().unwrap_or_default();
                log::info!(
                    "SHELL appearance theme={:?} wallpaper={:?}",
                    preferences.theme,
                    preferences.wallpaper
                );
                preferences
            },
            launcher: None,
            quick_settings: None,
            calendar: None,
            osd: None,
            quick_status: quick_settings::QuickStatus::default(),
            osd_state: None,
            osd_seq: 0,
            timer: calendar::QuickTimer::default(),
            lock: None,
            agent: None,
            mark_overlay: None,
            agent_state: agent::AgentState::new(agent_configured),
            agent_tx,
            agent_view,
            apps: catalog::discover(),
            tx,
            clock: now.bar,
            lock_time: now.lock_time,
            lock_date: now.lock_date,
            person: person_label(),
        });
        cx.open_window(
            layer(
                "wallpaper",
                Layer::Background,
                Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                0.,
                0.,
                Some(-1.),
                KeyboardInteractivity::None,
            ),
            |_, cx| {
                cx.new(|_| Background {
                    model: model.clone(),
                })
            },
        )
        .expect("Wallpaper layer");
        cx.open_window(
            layer(
                "bar",
                Layer::Top,
                Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
                0.,
                BAR_HEIGHT,
                Some(BAR_HEIGHT),
                KeyboardInteractivity::None,
            ),
            |_, cx| {
                cx.new(|_| Bar {
                    model: model.clone(),
                })
            },
        )
        .expect("Bar layer");
        cx.open_window(
            layer(
                "dock",
                Layer::Top,
                Anchor::BOTTOM,
                DOCK_WIDTH,
                DOCK_LAYER_HEIGHT,
                Some(DOCK_HEIGHT),
                KeyboardInteractivity::None,
            ),
            |_, cx| {
                cx.new(|_| Dock {
                    model: model.clone(),
                    hovered: None,
                })
            },
        )
        .expect("Dock layer");
        // The shell's events wake the interface as they come, so the bar
        // shows a workspace in the frame it changes or the next (#273).
        let events = model.clone();
        cx.spawn(async move |cx| {
            let mut rx = rx;
            while let Some(first) = rx.next().await {
                let mut waiting = vec![first];
                while let Ok(e) = rx.try_recv() {
                    waiting.push(e);
                }
                if cx
                    .update(|cx| {
                        if handle_events(&events, waiting, cx) {
                            cx.refresh_windows();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        // The clock and the saved appearance are looked at once a second.
        // Neither redraws anything until it changes.
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if cx
                    .update(|cx| {
                        if tick(&model, cx) {
                            cx.refresh_windows();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    });
    // GPUI stops when the shell quits, or once it has no layer left, as when
    // the compositor closed them all because the last display went away. The
    // shell's service starts it again, and it waits for a display (#271).
    log::info!("SHELL exits: it quit, or has no layer left to draw");
    Ok(())
}

#[cfg(test)]
mod lock_tests {
    use super::{
        AppEntry, Assets, DockPinState, Gesture, LockLayout, MAX_VISIBLE_DOTS, Preferences,
        agent_surface_width, asset, gesture_does,
        background_asset, bar_mark_asset, bar_workspaces, dock_pin_state, nightwatch_size,
        lock_screen_changes, occupied_workspaces, person_label_from, session,
        theme::{Theme, Wallpaper},
        visible_password_dots,
    };
    use gpui::AssetSource;
    use std::path::PathBuf;

    #[test]
    fn a_snapshot_that_did_not_reach_the_compositor_leaves_the_lock_screen_alone() {
        let failed = session::Snapshot::default();
        assert!(!lock_screen_changes(&failed, true));
        assert!(!lock_screen_changes(&failed, false));

        let unlocked = session::Snapshot {
            connected: true,
            ..Default::default()
        };
        assert!(lock_screen_changes(&unlocked, true));
        let locked = session::Snapshot {
            connected: true,
            locked: true,
            ..Default::default()
        };
        assert!(lock_screen_changes(&locked, false));
        assert!(!lock_screen_changes(&locked, true));
    }

    /// The summon gesture brings the Agent's surface up, brings the keyboard
    /// back to it from a window, and puts it away only when the keyboard is
    /// in it (ADR 0035).
    #[test]
    fn the_summon_gesture_shows_the_agents_surface_returns_the_keyboard_or_puts_it_away() {
        assert_eq!(gesture_does(false, false), Gesture::Show);
        assert_eq!(gesture_does(true, false), Gesture::TakeKeyboard);
        assert_eq!(gesture_does(true, true), Gesture::PutAway);
        // The compositor says the keyboard is in a surface the shell no
        // longer has, as one whose display went away: it is shown again.
        assert_eq!(gesture_does(false, true), Gesture::Show);
    }

    #[test]
    fn the_agents_surface_is_a_third_of_its_display() {
        assert_eq!(agent_surface_width(1280.), 427.);
        assert_eq!(agent_surface_width(2560.), 853.);
        assert_eq!(agent_surface_width(1024.), 341.);
    }

    #[test]
    fn lock_layout_matches_brand_proportions_at_1600x900() {
        let layout = LockLayout::for_viewport(1600., 900.);
        assert!((layout.left_pad - 131.2).abs() < 1.0);
        assert!((layout.clock_size - 124.2).abs() < 1.0);
        assert_eq!(layout.field_width, 370.0);
        assert!((layout.logo_width - 232.0).abs() < 1.0);
        assert!((layout.logo_height - 232.0 * (180.0 / 850.0)).abs() < 0.1);
    }

    #[test]
    fn lock_layout_keeps_login_clear_of_crow_on_1280x960() {
        let layout = LockLayout::for_viewport(1280., 960.);
        let (bg_w, bg_h) = nightwatch_size(1280., 960.).unwrap();
        assert_eq!(bg_h, 960.0);
        assert!(bg_w > 1280.0);
        // The crow starts 709/1672 of Nightwatch's width from the right edge.
        let crow_left_x = 1280.0 - bg_w * (709.0 / 1672.0);
        assert!(
            layout.left_pad + layout.field_width + 48.0 < crow_left_x,
            "login right {} too close to crow left {}",
            layout.left_pad + layout.field_width,
            crow_left_x
        );
    }

    #[test]
    fn nightwatch_size_handles_zero_viewport() {
        assert_eq!(nightwatch_size(0., 900.), None);
        assert_eq!(nightwatch_size(1600., 0.), None);
    }

    #[test]
    fn person_label_prefers_explicit_env_then_human_account_then_fallback() {
        let passwd = concat!(
            "root:x:0:0:root:/root:/bin/sh\n",
            "crowbar-compositor:x:1000:1000::/home/crowbar-compositor:/bin/sh\n",
            "crowbar-shell:x:1001:1001::/home/crowbar-shell:/bin/sh\n",
        );
        assert_eq!(
            person_label_from(Some("Alex"), Some("crowbar-shell"), Some(passwd)),
            "Alex"
        );
        assert_eq!(
            person_label_from(None, Some("crowbar-shell"), Some(passwd)),
            "Person"
        );
        // The lab account is a person's account, not a service account.
        let with_lab = format!("{passwd}crowbar:x:1003:1003::/home/crowbar:/bin/bash\n");
        assert_eq!(
            person_label_from(None, Some("crowbar-shell"), Some(&with_lab)),
            "crowbar"
        );
        let with_human = format!("{passwd}jason:x:1003:1003:Jason Alexander,,,:/home/jason:/bin/bash\n");
        assert_eq!(
            person_label_from(None, Some("crowbar-shell"), Some(&with_human)),
            "Jason Alexander"
        );
        assert_eq!(
            person_label_from(None, Some("jason"), Some(&with_human)),
            "Jason Alexander"
        );
    }

    #[test]
    fn password_dots_cap_at_field_capacity() {
        assert_eq!(visible_password_dots(""), 0);
        assert_eq!(visible_password_dots("crowbar"), 7);
        assert_eq!(visible_password_dots(&"a".repeat(64)), MAX_VISIBLE_DOTS);
    }

    #[test]
    fn embedded_lock_assets_load() {
        let assets = Assets;
        for name in [
            "user",
            "mark-micro",
            "mark-dark",
            "mark-frost",
            "logo-horizontal-primary",
            "logo-horizontal-frost",
        ] {
            let bytes = assets.load(name).unwrap();
            assert!(bytes.is_some_and(|b| !b.is_empty()), "missing asset {name}");
        }
    }

    #[test]
    fn bar_mark_matches_theme_and_studio_override() {
        assert_eq!(bar_mark_asset(&Preferences::default()), "mark-micro");
        assert_eq!(
            bar_mark_asset(&Preferences {
                theme: Theme::Light,
                ..Default::default()
            }),
            "mark-dark"
        );
        assert_eq!(
            bar_mark_asset(&Preferences {
                theme: Theme::WarmPaper,
                ..Default::default()
            }),
            "mark-dark"
        );
        assert_eq!(
            bar_mark_asset(&Preferences {
                theme: Theme::Light,
                studio: true,
                ..Default::default()
            }),
            "mark-frost"
        );
    }

    #[test]
    fn background_asset_follows_wallpaper_and_studio_neutral_canvas() {
        assert_eq!(
            background_asset(&Preferences::default()),
            Some("wallpaper.png")
        );
        assert_eq!(
            background_asset(&Preferences {
                wallpaper: Wallpaper::Nightwatch,
                ..Default::default()
            }),
            Some("nightwatch.png")
        );
        assert_eq!(
            background_asset(&Preferences {
                wallpaper: Wallpaper::QuietSignal,
                ..Default::default()
            }),
            Some("quiet-signal.png")
        );
        assert_eq!(
            background_asset(&Preferences {
                wallpaper: Wallpaper::QuietSignal,
                studio: true,
                ..Default::default()
            }),
            None
        );
        assert!(asset("quiet-signal.png").is_file());
    }

    #[test]
    fn occupied_workspaces_deduplicates_and_bar_includes_extra_workspaces() {
        let client = |addr: &str, ws: i32, attention: bool| session::Client {
            address: addr.into(),
            class: "foot".into(),
            title: "Terminal".into(),
            workspace: session::Workspace { id: ws },
            attention,
            fullscreen: false,
            output: String::new(),
        };
        assert!(occupied_workspaces(&[]).is_empty());
        assert_eq!(bar_workspaces(1, &[]), vec![1, 2, 3, 4]);

        let clients = vec![
            client("0x1", 3, false),
            client("0x2", 1, false),
            client("0x3", 3, false),
            client("0x4", 6, false),
            client("0x5", 0, false),
        ];
        assert_eq!(occupied_workspaces(&clients), vec![1, 3, 6]);
        assert_eq!(bar_workspaces(2, &clients), vec![1, 2, 3, 4, 6]);
        assert_eq!(bar_workspaces(5, &clients), vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn dock_pin_state_distinguishes_idle_backgrounded_focused_and_attention() {
        let foot = AppEntry {
            id: "foot.desktop".into(),
            name: "Terminal".into(),
            description: "Wayland terminal".into(),
            path: PathBuf::from("/usr/share/applications/foot.desktop"),
            wm_class: "foot".into(),
            keywords: String::new(),
        };
        let settings = AppEntry {
            id: "com.crowbaros.StudioSettings.desktop".into(),
            name: "Settings".into(),
            description: "Appearance".into(),
            path: PathBuf::from("/usr/share/applications/com.crowbaros.StudioSettings.desktop"),
            wm_class: "com.crowbaros.StudioSettings".into(),
            keywords: String::new(),
        };
        let foot_client = session::Client {
            address: "0x1".into(),
            class: "foot".into(),
            title: "Terminal".into(),
            workspace: session::Workspace { id: 1 },
            attention: false,
            fullscreen: false,
            output: String::new(),
        };
        let settings_client = session::Client {
            address: "0x2".into(),
            class: "com.crowbaros.StudioSettings".into(),
            title: "Settings".into(),
            workspace: session::Workspace { id: 1 },
            attention: false,
            fullscreen: false,
            output: String::new(),
        };

        let empty = session::Snapshot::default();
        assert_eq!(dock_pin_state(None, &empty), DockPinState::Idle);
        assert_eq!(dock_pin_state(Some(&foot), &empty), DockPinState::Idle);

        let focused_foot = session::Snapshot {
            workspace: 1,
            clients: vec![foot_client.clone(), settings_client.clone()],
            active: foot_client.clone(),
            connected: true,
            locked: false,
        };
        assert_eq!(
            dock_pin_state(Some(&foot), &focused_foot),
            DockPinState::Focused { attention: false }
        );
        assert_eq!(
            dock_pin_state(Some(&settings), &focused_foot),
            DockPinState::Running { attention: false }
        );

        let mut urgent_settings = settings_client.clone();
        urgent_settings.attention = true;
        let urgent_snapshot = session::Snapshot {
            workspace: 1,
            clients: vec![foot_client.clone(), urgent_settings],
            active: foot_client,
            connected: true,
            locked: false,
        };
        assert_eq!(
            dock_pin_state(Some(&settings), &urgent_snapshot),
            DockPinState::Running { attention: true }
        );

        let no_active = session::Snapshot {
            workspace: 2,
            clients: vec![settings_client],
            active: session::Client::default(),
            connected: true,
            locked: false,
        };
        assert_eq!(
            dock_pin_state(Some(&settings), &no_active),
            DockPinState::Running { attention: false }
        );
        assert_eq!(dock_pin_state(Some(&foot), &no_active), DockPinState::Idle);
    }
}
