// SPDX-License-Identifier: MIT
mod catalog;
mod input;
mod session;
mod theme;
use catalog::AppEntry;
use gpui::{layer_shell::*, prelude::*, *};
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::mpsc::{self, Sender},
    time::Duration,
};
use theme::{Palette, Preferences};
actions!(shell, [Next, Previous, Open, Close, Unlock]);
struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Some(Cow::Borrowed(match path {
            "apps" => include_bytes!("../assets/apps.svg"),
            "terminal" => include_bytes!("../assets/terminal.svg"),
            "settings" => include_bytes!("../assets/settings.svg"),
            "lock" => include_bytes!("../assets/lock.svg"),
            "application" => include_bytes!("../assets/application.svg"),
            "search" => include_bytes!("../assets/search.svg"),
            "close" => include_bytes!("../assets/close.svg"),
            "arrow" => include_bytes!("../assets/arrow.svg"),
            "user" => include_bytes!("../assets/user.svg"),
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
struct ClockDisplay {
    bar: String,
    lock_time: String,
    lock_date: String,
}
impl ClockDisplay {
    fn now() -> Self {
        let now = chrono::Local::now();
        Self {
            bar: now.format("%a %b %-d   %H:%M").to_string(),
            lock_time: now.format("%H:%M").to_string(),
            lock_date: now.format("%A, %B %-d").to_string(),
        }
    }
}
struct Model {
    snapshot: session::Snapshot,
    preferences: Preferences,
    launcher: Option<WindowHandle<Launcher>>,
    lock: Option<WindowHandle<LockScreen>>,
    /// The confirm chord opened the lock screen's unlock entry.
    unlock_entry: bool,
    apps: Vec<AppEntry>,
    tx: Sender<session::Event>,
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
            keyboard_interactivity: keyboard,
            ..Default::default()
        }),
        ..Default::default()
    }
}
struct Background;
impl Render for Background {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(rgb(0x080d12)).child(
            img(asset("wallpaper.png"))
                .size_full()
                .object_fit(ObjectFit::Cover),
        )
    }
}
struct Bar {
    model: Entity<Model>,
}
impl Render for Bar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let active = m.snapshot.workspace;
        let title = if m.snapshot.connected {
            if m.snapshot.active.title.is_empty() {
                "Desktop".into()
            } else {
                m.snapshot.active.title.clone()
            }
        } else {
            "Connecting to the compositor…".into()
        };
        let clock = m.clock.clone();
        // A workspace with a window that wants attention is marked, and
        // shown past the first four.
        let wanting: Vec<i32> = m
            .snapshot
            .clients
            .iter()
            .filter(|c| c.attention)
            .map(|c| c.workspace.id)
            .collect();
        let mut workspaces = vec![1, 2, 3, 4];
        for id in std::iter::once(active).chain(wanting.iter().copied()) {
            if id > 4 && !workspaces.contains(&id) {
                workspaces.push(id)
            }
        }
        workspaces.sort();
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_between()
            .px(px(16.))
            .gap(px(24.))
            .font_family("Manrope")
            .text_size(px(13.))
            .text_color(rgb(p.text))
            .bg(rgb(p.canvas))
            .border_b_1()
            .border_color(rgb(p.line))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(20.))
                    .child(
                        div()
                            .id("applications")
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .px(px(8.))
                            .h(px(36.))
                            .rounded(px(8.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(img(asset("mark.png")).size(px(23.)))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Applications"),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                toggle_launcher(this.model.clone(), cx)
                            })),
                    )
                    .child(div().flex().items_center().gap(px(4.)).children(
                        workspaces.into_iter().map(|id| {
                            let attention = wanting.contains(&id);
                            div()
                                .id(("workspace", id as usize))
                                .border_b_2()
                                .border_color(rgb(if attention { p.accent } else { p.canvas }))
                                .w(px(28.))
                                .h(px(28.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(px(6.))
                                .cursor_pointer()
                                .bg(rgb(if active == id { p.accent } else { p.canvas }))
                                .text_color(rgb(if active == id { p.on_accent } else { p.muted }))
                                .hover(move |s| {
                                    s.bg(rgb(if active == id { p.accent } else { p.surface }))
                                })
                                .child(id.to_string())
                                .on_click(move |_, _, _| session::workspace(id))
                        }),
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(rgb(p.muted))
                    .text_ellipsis()
                    .child(title),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(18.))
                    .child(clock)
                    .child(
                        div()
                            .id("lock")
                            .size(px(32.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(8.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(icon("lock", p.muted, 19.))
                            .on_click(|_, _, _| session::lock(true)),
                    )
                    .child(
                    div()
                        .id("settings")
                        .size(px(32.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(8.))
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgb(p.surface)))
                        .child(icon("settings", p.muted, 19.))
                        .on_click(cx.listener(|this, _, _, cx| {
                            activate_pin(&this.model, "com.crowbaros.StudioSettings.desktop", cx)
                        })),
                ),
            )
    }
}
struct Dock {
    model: Entity<Model>,
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
        let clients = m.snapshot.clients.clone();
        let mut pins = vec![("apps", "Applications", None)];
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
            .items_start()
            .justify_center()
            .pt(px(4.))
            .font_family("Manrope")
            .text_size(px(11.))
            .text_color(rgb(p.text))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .p(px(7.))
                    .rounded(px(16.))
                    .bg(rgb(p.canvas))
                    .border_1()
                    .border_color(rgb(p.line))
                    .children(pins.into_iter().map(|(ic, label, id)| {
                        let app = id.and_then(|id| apps.iter().find(|a| a.id == id));
                        let running =
                            app.is_some_and(|a| clients.iter().any(|c| a.matches_class(&c.class)));
                        let attention = app.is_some_and(|a| {
                            clients
                                .iter()
                                .any(|c| c.attention && a.matches_class(&c.class))
                        });
                        div()
                            .id(ic)
                            .w(px(84.))
                            .h(px(64.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap(px(4.))
                            .rounded(px(10.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(icon(ic, if running { p.focus } else { p.text }, 25.))
                            .child(label)
                            .child(
                                div()
                                    .w(px(if attention {
                                        20.
                                    } else if running {
                                        12.
                                    } else {
                                        3.
                                    }))
                                    .h(px(2.))
                                    .rounded(px(1.))
                                    .bg(rgb(if attention {
                                        p.accent
                                    } else if running {
                                        p.focus
                                    } else {
                                        p.canvas
                                    })),
                            )
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
            .justify_center()
            .pt(px(72.))
            .bg(rgba(0x080d1280))
            .font_family("Manrope")
            .text_color(rgb(p.text))
            .text_size(px(14.))
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
                    .w(px(640.))
                    .h(px(578.))
                    .mx(px(24.))
                    .flex()
                    .flex_col()
                    .rounded(px(16.))
                    .bg(rgb(p.canvas))
                    .border_1()
                    .border_color(rgb(p.line))
                    .overflow_hidden()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .px(px(24.))
                            .pt(px(20.))
                            .pb(px(12.))
                            .child(
                                div()
                                    .text_size(px(24.))
                                    .font_weight(FontWeight::BOLD)
                                    .child("Applications"),
                            )
                            .child(
                                div()
                                    .id("close-launcher")
                                    .size(px(32.))
                                    .rounded(px(8.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(rgb(p.surface)))
                                    .child(icon("close", p.muted, 18.))
                                    .on_click(cx.listener(|this, _, w, cx| this.close(w, cx))),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .mx(px(24.))
                            .mb(px(16.))
                            .px(px(12.))
                            .h(px(52.))
                            .rounded(px(10.))
                            .bg(rgb(p.surface))
                            .border_1()
                            .border_color(rgb(p.focus))
                            .child(icon("search", p.muted, 22.))
                            .child(div().flex_1().min_w_0().child(self.input.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .px(px(12.))
                            .when(total == 0, |d| {
                                d.child(
                                    div()
                                        .px(px(20.))
                                        .pt(px(42.))
                                        .child(
                                            div()
                                                .text_size(px(18.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .child("No applications found"),
                                        )
                                        .child(
                                            div()
                                                .mt(px(8.))
                                                .text_color(rgb(p.muted))
                                                .child("Try a shorter name or clear your search."),
                                        ),
                                )
                            })
                            .children(results.into_iter().enumerate().skip(offset).take(6).map(
                                |(index, app)| {
                                    div()
                                        .id(("result", index))
                                        .h(px(56.))
                                        .px(px(12.))
                                        .flex()
                                        .items_center()
                                        .gap(px(14.))
                                        .rounded(px(8.))
                                        .bg(rgb(if index == selected {
                                            p.surface
                                        } else {
                                            p.canvas
                                        }))
                                        .cursor_pointer()
                                        .hover(move |s| s.bg(rgb(p.surface)))
                                        .child(
                                            div()
                                                .size(px(36.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .child(icon(
                                                    app.icon(),
                                                    if index == selected {
                                                        p.focus
                                                    } else {
                                                        p.muted
                                                    },
                                                    24.,
                                                )),
                                        )
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .child(
                                                    div()
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .text_ellipsis()
                                                        .child(app.name),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(12.))
                                                        .text_color(rgb(p.muted))
                                                        .text_ellipsis()
                                                        .child(app.description),
                                                ),
                                        )
                                        .when(index == selected, |d| {
                                            d.child(icon("arrow", p.focus, 18.))
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
                                .px(px(24.))
                                .py(px(8.))
                                .text_size(px(12.))
                                .text_color(rgb(p.text))
                                .child(self.error.clone().unwrap_or_default()),
                        )
                    })
                    .child(
                        div()
                            .h(px(44.))
                            .px(px(24.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_t_1()
                            .border_color(rgb(p.line))
                            .text_size(px(12.))
                            .text_color(rgb(p.muted))
                            .child(if busy {
                                "Opening application…".into()
                            } else {
                                if total > 6 {
                                    format!("{}–{} of {total}", offset + 1, (offset + 6).min(total))
                                } else {
                                    format!("{total} applications")
                                }
                            })
                            .when(total > 6, |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .gap(px(10.))
                                        .child(
                                            div()
                                                .id("previous-page")
                                                .cursor_pointer()
                                                .px(px(4.))
                                                .py(px(8.))
                                                .child("Previous")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.selected = this.selected.saturating_sub(6);
                                                    cx.notify();
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("next-page")
                                                .cursor_pointer()
                                                .px(px(4.))
                                                .py(px(8.))
                                                .child("Next")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.selected =
                                                        (this.selected + 6).min(total - 1);
                                                    cx.notify();
                                                })),
                                        ),
                                )
                            })
                            .child("↑ ↓  choose     Enter  open     Esc  close"),
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
/// The lock screen: shown while the compositor has the session locked, and
/// all it shows (ADR 0020). Unlock begins with the confirm chord,
/// Ctrl+Alt+Enter, which opens its entry. Where an unlock helper checks the
/// person's password, the entry takes it, and Enter sends it to the helper,
/// which tells the compositor to unlock: the shell's own word is not enough.
/// Without one, Enter asks the compositor to unlock. Both stand in for the
/// broker, as Fulcrum's proposal until a record decides (issues 87 and 100).
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
        let m = self.model.read(cx);
        if !m.unlock_entry || self.busy {
            return;
        }
        if !session::unlocks_with_password() {
            session::lock(false);
            return;
        }
        if self.password.is_empty() {
            return;
        }
        let tx = m.tx.clone();
        self.busy = true;
        self.error = None;
        session::unlock_with_password(std::mem::take(&mut self.password), tx);
        cx.notify();
    }
    fn typed(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if !self.model.read(cx).unlock_entry || self.busy || !session::unlocks_with_password() {
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
    if let Some((_, _, gecos)) = entries.iter().find(|(n, _, _)| *n == "crowbar-person") {
        if let Some(display) = gecos_display_name(gecos) {
            return display;
        }
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

fn keycap(label: &'static str) -> Div {
    div()
        .px(px(8.))
        .py(px(3.))
        .rounded(px(6.))
        .bg(rgb(LOCK_ABYSS))
        .border_1()
        .border_color(rgb(LOCK_LINE))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(LOCK_FROST))
        .child(label)
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
        let entry = m.unlock_entry;
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
                                    .when(!entry, |d| {
                                        d.child(
                                            div()
                                                .id("lock-chord")
                                                .w_full()
                                                .h(px(52.))
                                                .px(px(17.))
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .rounded(px(10.))
                                                .bg(rgb(LOCK_VOID))
                                                .border_1()
                                                .border_color(rgb(LOCK_STEEL))
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap(px(10.))
                                                        .child(icon("lock", LOCK_MUTED, 16.))
                                                        .child(
                                                            div()
                                                                .text_size(px(14.))
                                                                .text_color(rgb(LOCK_MUTED))
                                                                .child("Locked"),
                                                        ),
                                                )
                                                .child(
                                                    div()
                                                        .flex()
                                                        .items_center()
                                                        .gap(px(5.))
                                                        .child(keycap("Ctrl"))
                                                        .child(
                                                            div()
                                                                .text_size(px(11.))
                                                                .text_color(rgb(LOCK_STEEL))
                                                                .child("+"),
                                                        )
                                                        .child(keycap("Alt"))
                                                        .child(
                                                            div()
                                                                .text_size(px(11.))
                                                                .text_color(rgb(LOCK_STEEL))
                                                                .child("+"),
                                                        )
                                                        .child(keycap("Enter")),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .mt(px(14.))
                                                .text_size(px(12.))
                                                .text_color(rgb(LOCK_MUTED))
                                                .child("Press Ctrl+Alt+Enter to unlock"),
                                        )
                                    })
                                    .when(entry && with_password, |d| {
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
                                    .when(entry && !with_password, |d| {
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
    let (tx, rx) = mpsc::channel();
    session::deliver_events_to(tx.clone());
    let _lock = session::server(tx.clone())?;
    let watcher = tx.clone();
    std::thread::spawn(move || {
        let mut last = None;
        loop {
            let snapshot = session::snapshot().unwrap_or_default();
            if last.as_ref() != Some(&snapshot) {
                last = Some(snapshot.clone());
                if watcher.send(session::Event::Snapshot(snapshot)).is_err() {
                    break;
                }
            }
            // Events, as Super tapped alone, are read sooner than snapshots.
            for _ in 0..7 {
                session::pump();
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    });
    Application::new().with_assets(Assets).run(move |cx| {
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../fonts/Manrope-Regular.ttf")),
                Cow::Borrowed(include_bytes!("../fonts/Manrope-SemiBold.ttf")),
                Cow::Borrowed(include_bytes!("../fonts/Manrope-Bold.ttf")),
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
            KeyBinding::new("enter", Unlock, Some("Lock")),
        ]);
        let now = ClockDisplay::now();
        let model = cx.new(|_| Model {
            snapshot: Default::default(),
            preferences: {
                let preferences = Preferences::load().unwrap_or_default();
                log::info!("SHELL appearance theme={:?}", preferences.theme);
                preferences
            },
            launcher: None,
            lock: None,
            unlock_entry: false,
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
            |_, cx| cx.new(|_| Background),
        )
        .expect("Wallpaper layer");
        cx.open_window(
            layer(
                "bar",
                Layer::Top,
                Anchor::TOP | Anchor::LEFT | Anchor::RIGHT,
                0.,
                44.,
                Some(44.),
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
                286.,
                96.,
                Some(96.),
                KeyboardInteractivity::None,
            ),
            |_, cx| {
                cx.new(|_| Dock {
                    model: model.clone(),
                })
            },
        )
        .expect("Dock layer");
        cx.spawn(async move |cx| {
            let mut ticks = 0u8;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(150))
                    .await;
                if cx
                    .update(|cx| {
                        let mut refresh = false;
                        for e in rx.try_iter() {
                            match e {
                                session::Event::Toggle => toggle_launcher(model.clone(), cx),
                                session::Event::Quit => cx.quit(),
                                session::Event::Snapshot(s) => {
                                    // A shell that starts while locked shows
                                    // the lock screen too.
                                    let locked = s.locked;
                                    let entry = s.unlock_entry;
                                    model.update(cx, |m, _| {
                                        m.snapshot = s;
                                        m.unlock_entry = entry;
                                    });
                                    if locked != model.read(cx).lock.is_some() {
                                        set_lock_screen(&model, locked, cx);
                                    }
                                    refresh = true
                                }
                                session::Event::Lock(locked) => {
                                    model.update(cx, |m, _| m.unlock_entry = false);
                                    set_lock_screen(&model, locked, cx)
                                }
                                session::Event::UnlockEntry => {
                                    log::info!("SHELL lock entry=open");
                                    model.update(cx, |m, _| m.unlock_entry = true);
                                    refresh = true
                                }
                                session::Event::Unlock(result) => {
                                    if let Some(w) = model.read(cx).lock {
                                        let _ = w.update(cx, |v, _, cx| v.answered(result, cx));
                                    }
                                }
                                session::Event::Launch(result) => {
                                    if let Some(w) = model.read(cx).launcher {
                                        match result {
                                            Ok(()) => close_launcher(&model, cx),
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
                        ticks = (ticks + 1) % 7;
                        if ticks == 0 {
                            let now = ClockDisplay::now();
                            let prefs = Preferences::load();
                            model.update(cx, |m, _| {
                                if m.clock != now.bar
                                    || m.lock_time != now.lock_time
                                    || m.lock_date != now.lock_date
                                {
                                    m.clock = now.bar;
                                    m.lock_time = now.lock_time;
                                    m.lock_date = now.lock_date;
                                    refresh = true
                                }
                                if let Some(prefs) = prefs {
                                    if prefs != m.preferences {
                                        log::info!("SHELL appearance theme={:?}", prefs.theme);
                                        m.preferences = prefs;
                                        refresh = true
                                    }
                                }
                            });
                        }
                        if refresh {
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
    Ok(())
}

#[cfg(test)]
mod lock_tests {
    use super::{
        Assets, LockLayout, MAX_VISIBLE_DOTS, nightwatch_size, person_label_from,
        visible_password_dots,
    };
    use gpui::AssetSource;

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
            "crowbar-app:x:1002:1002::/home/crowbar-app:/bin/sh\n",
            "crowbar-person:x:1003:1003::/home/crowbar-person:/bin/false\n",
        );
        assert_eq!(
            person_label_from(Some("Alex"), Some("crowbar-shell"), Some(passwd)),
            "Alex"
        );
        assert_eq!(
            person_label_from(None, Some("crowbar-shell"), Some(passwd)),
            "Person"
        );
        let with_human = format!("{passwd}jason:x:1004:1004:Jason Alexander,,,:/home/jason:/bin/bash\n");
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
        for name in ["user", "logo-horizontal-primary", "logo-horizontal-frost"] {
            let bytes = assets.load(name).unwrap();
            assert!(bytes.is_some_and(|b| !b.is_empty()), "missing asset {name}");
        }
    }
}
