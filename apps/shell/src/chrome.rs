// SPDX-License-Identifier: MIT
//! Persistent desktop chrome: Bar, Dock, Background, and the fullscreen
//! Agent MarkOverlay.

use gpui::{prelude::*, *};

use crate::{
    agent, appearance, calendar, catalog::AppEntry, compositor, launcher, lock, quick_settings,
    ui::{Model, asset, icon},
};

pub fn bar_mark_asset(preferences: &appearance::Preferences) -> &'static str {
    if preferences.studio {
        "mark-frost"
    } else if matches!(preferences.theme, appearance::Theme::Dark) {
        "mark-micro"
    } else {
        "mark-dark"
    }
}

pub fn background_asset(preferences: &appearance::Preferences) -> Option<&'static str> {
    if preferences.studio {
        return None;
    }
    Some(match preferences.wallpaper {
        appearance::Wallpaper::SignalFeather => "wallpaper.png",
        appearance::Wallpaper::Nightwatch => "nightwatch.png",
        appearance::Wallpaper::QuietSignal => "quiet-signal.png",
    })
}

pub fn occupied_workspaces(clients: &[compositor::Client]) -> Vec<i32> {
    let mut ids: Vec<i32> = clients
        .iter()
        .map(|c| c.workspace.id)
        .filter(|id| *id > 0)
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

pub fn bar_workspaces(active: i32, clients: &[compositor::Client]) -> Vec<i32> {
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
pub enum DockPinState {
    Idle,
    Running { attention: bool },
    Focused { attention: bool },
}

pub fn dock_pin_state(app: Option<&AppEntry>, snapshot: &compositor::Snapshot) -> DockPinState {
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

pub fn render_mark_pill(mark: agent::Mark, p: appearance::Palette) -> Option<Stateful<Div>> {
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
        // A request waits for the person: a ring, not a dot, in the accent.
        agent::Mark::Waiting => Some(
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
                .border_color(rgb(p.accent))
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(p.text))
                .child(
                    div()
                        .size(px(7.))
                        .rounded_full()
                        .border_2()
                        .border_color(rgb(p.accent)),
                )
                .child("Waiting"),
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

pub struct MarkOverlay {
    pub model: Entity<Model>,
}

impl Render for MarkOverlay {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let pill = render_mark_pill(m.agent.state.mark, p);
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

pub struct Background {
    pub model: Entity<Model>,
}

impl Render for Background {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let Some(file) = background_asset(&m.preferences) else {
            return div().size_full().bg(rgb(p.canvas));
        };
        if let Some(image) = &m.wallpaper {
            return div()
                .size_full()
                .bg(rgb(lock::LOCK_VOID))
                .child(img(image.clone()).size_full().object_fit(ObjectFit::Cover));
        }
        match m.preferences.wallpaper {
            appearance::Wallpaper::Nightwatch => {
                let vp = window.viewport_size();
                let vp_w: f32 = vp.width.into();
                let vp_h: f32 = vp.height.into();
                let artwork = match lock::nightwatch_size(vp_w, vp_h) {
                    Some((bg_w, bg_h)) => img(asset(file))
                        .absolute()
                        .top_0()
                        .right_0()
                        .w(px(bg_w))
                        .h(px(bg_h))
                        .object_fit(ObjectFit::Fill),
                    None => img(asset(file)).size_full().object_fit(ObjectFit::Cover),
                };
                div()
                    .relative()
                    .size_full()
                    .overflow_hidden()
                    .bg(rgb(lock::LOCK_GROUND))
                    .child(artwork)
            }
            appearance::Wallpaper::SignalFeather | appearance::Wallpaper::QuietSignal => div()
                .size_full()
                .bg(rgb(lock::LOCK_VOID))
                .child(img(asset(file)).size_full().object_fit(ObjectFit::Cover)),
        }
    }
}

pub struct Bar {
    pub model: Entity<Model>,
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
        let wanting: Vec<i32> = m
            .snapshot
            .clients
            .iter()
            .filter(|c| c.attention)
            .map(|c| c.workspace.id)
            .collect();
        let workspaces = bar_workspaces(active, &m.snapshot.clients);
        let calendar_open = m.is_calendar_open();
        let quick_settings_open = m.is_quick_settings_open();
        let compositor = m.compositor.clone();
        let lock_compositor = compositor.clone();
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
                                launcher::toggle(this.model.clone(), cx)
                            })),
                    )
                    .child(div().w(px(1.)).h(px(10.)).bg(rgb(p.line)))
                    .child(div().flex().items_center().gap(px(2.)).children(
                        workspaces.into_iter().map(|id| {
                            let has_windows = occupied.contains(&id);
                            let attention = wanting.contains(&id);
                            let selected = active == id;
                            let ws_compositor = compositor.clone();
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
                                .on_click(move |_, _, _| ws_compositor.focus_workspace(id))
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
                    .when(calendar_open, |d| d.bg(rgb(p.surface)))
                    .hover(move |s| s.bg(rgb(p.surface)))
                    .text_size(px(11.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(rgb(p.text))
                    .child(clock)
                    .on_click(cx.listener(|this, _, _, cx| {
                        calendar::toggle(this.model.clone(), cx)
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
                    .when_some(render_mark_pill(m.agent.state.mark, p), |d, pill| {
                        d.child(pill)
                    })
                    .child(
                        div()
                            .id("theme-switcher")
                            .w(px(24.))
                            .h(px(20.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(icon("palette", p.muted, 15.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                appearance::toggle_theme_switcher(this.model.clone(), cx)
                            })),
                    )
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
                            .on_click(move |_, _, _| lock_compositor.lock(true)),
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
                            .when(quick_settings_open, |d| d.bg(rgb(p.surface)))
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(icon(
                                "settings",
                                if quick_settings_open {
                                    p.accent
                                } else {
                                    p.muted
                                },
                                13.5,
                            ))
                            .on_click(cx.listener(|this, _, _, cx| {
                                quick_settings::toggle(this.model.clone(), cx)
                            })),
                    ),
            )
    }
}

pub struct Dock {
    pub model: Entity<Model>,
    pub hovered: Option<&'static str>,
}

pub fn activate_pin(model: &Entity<Model>, id: &str, cx: &mut App) {
    let m = model.read(cx);
    if let Some(app) = m.apps.iter().find(|a| a.id == id) {
        if let Some(client) = m
            .snapshot
            .clients
            .iter()
            .find(|c| app.matches_class(&c.class))
        {
            m.compositor.focus_window(client.address.clone());
        } else {
            m.launcher_client
                .launch(app.path.clone(), m.compositor.clone(), m.tx.clone());
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
                                    div().size(px(4.)).rounded_full().bg(rgb(p.muted)),
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
                                    launcher::toggle(this.model.clone(), cx)
                                }
                            }))
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DockPinState, background_asset, bar_mark_asset, bar_workspaces, dock_pin_state,
        occupied_workspaces,
    };
    use crate::appearance::{Preferences, Theme, Wallpaper};
    use crate::catalog::AppEntry;
    use crate::compositor;
    use crate::ui::asset;
    use std::path::PathBuf;

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
        let client = |addr: &str, ws: i32, attention: bool| compositor::Client {
            address: addr.into(),
            class: "foot".into(),
            title: "Terminal".into(),
            workspace: compositor::Workspace { id: ws },
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
        let foot_client = compositor::Client {
            address: "0x1".into(),
            class: "foot".into(),
            title: "Terminal".into(),
            workspace: compositor::Workspace { id: 1 },
            attention: false,
            fullscreen: false,
            output: String::new(),
        };
        let settings_client = compositor::Client {
            address: "0x2".into(),
            class: "com.crowbaros.StudioSettings".into(),
            title: "Settings".into(),
            workspace: compositor::Workspace { id: 1 },
            attention: false,
            fullscreen: false,
            output: String::new(),
        };

        let empty = compositor::Snapshot::default();
        assert_eq!(dock_pin_state(None, &empty), DockPinState::Idle);
        assert_eq!(dock_pin_state(Some(&foot), &empty), DockPinState::Idle);

        let focused_foot = compositor::Snapshot {
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
        let urgent_snapshot = compositor::Snapshot {
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

        let no_active = compositor::Snapshot {
            workspace: 2,
            clients: vec![settings_client],
            active: compositor::Client::default(),
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
