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
actions!(shell, [Next, Previous, Open, Close]);
struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Some(Cow::Borrowed(match path {
            "apps" => include_bytes!("../assets/apps.svg"),
            "terminal" => include_bytes!("../assets/terminal.svg"),
            "settings" => include_bytes!("../assets/settings.svg"),
            "application" => include_bytes!("../assets/application.svg"),
            "search" => include_bytes!("../assets/search.svg"),
            "close" => include_bytes!("../assets/close.svg"),
            "arrow" => include_bytes!("../assets/arrow.svg"),
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
    PathBuf::from("/usr/share/crowbaros-shell").join(name)
}
struct Model {
    snapshot: session::Snapshot,
    preferences: Preferences,
    launcher: Option<WindowHandle<Launcher>>,
    apps: Vec<AppEntry>,
    tx: Sender<session::Event>,
    clock: String,
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
                div().flex().items_center().gap(px(18.)).child(clock).child(
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
        ]);
        let model = cx.new(|_| Model {
            snapshot: Default::default(),
            preferences: {
                let preferences = Preferences::load().unwrap_or_default();
                log::info!("SHELL appearance theme={:?}", preferences.theme);
                preferences
            },
            launcher: None,
            apps: catalog::discover(),
            tx,
            clock: chrono::Local::now().format("%a %b %-d   %H:%M").to_string(),
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
                                    model.update(cx, |m, _| m.snapshot = s);
                                    refresh = true
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
                            let clock =
                                chrono::Local::now().format("%a %b %-d   %H:%M").to_string();
                            let prefs = Preferences::load();
                            model.update(cx, |m, _| {
                                if m.clock != clock {
                                    m.clock = clock;
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
