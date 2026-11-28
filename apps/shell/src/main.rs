// SPDX-License-Identifier: MIT
//! CrowbarOS desktop shell bootstrap: CLI flags, IPC wiring, keybindings,
//! persistent layer windows, and the shell event loop.

mod agent;
mod appearance;
mod calendar;
mod catalog;
mod chrome;
mod clipboard_picker;
mod composer;
mod compositor;
mod control_server;
mod drawer;
mod events;
mod input;
mod launcher;
mod launcher_client;
mod lock;
mod markdown;
mod picker_history;
mod quick_settings;
mod slash;
mod symbols;
mod ui;
mod unlock;

use futures::StreamExt;
use gpui::{layer_shell::*, *};
use std::{
    borrow::Cow,
    sync::{Arc, Mutex},
    time::Duration,
};

use agent::{AgentController, AgentIntent, AgentViewSnapshot};
use compositor::CompositorClient;
use events::Event;
use launcher_client::LauncherClient;
use ui::{
    ActivePopover, Assets, BAR_HEIGHT, ClockDisplay, Close, DOCK_HEIGHT, DOCK_LAYER_HEIGHT,
    DOCK_WIDTH, Model, Next, NextMonth, Open, PrevMonth, Previous, SendMessage, Unlock,
    close_active_popover, layer,
};
use unlock::UnlockClient;

fn sync_drawer_palette(model: &Entity<Model>, cx: &mut App) {
    if let Some(w) = model.read(cx).agent_window {
        let _ = w.update(cx, |view, _, cx| view.sync_palette(cx));
    }
}

/// Handles the shell's events. True if the windows must be drawn again: a
/// snapshot that is the same as the last draws nothing.
fn handle_events(model: &Entity<Model>, incoming: Vec<Event>, cx: &mut App) -> bool {
    let mut refresh = false;
    for e in incoming {
        match e {
            Event::Themes(catalog) => {
                log::info!("SHELL theme={}", catalog.active.theme_id);
                model.update(cx, |m, _| m.themes = Some(catalog));
                sync_drawer_palette(model, cx);
                refresh = true;
            }
            Event::Wallpaper(wallpaper) => {
                let image = wallpaper.map(appearance::CustomWallpaper::into_render_image);
                log::info!(
                    "SHELL wallpaper={}",
                    if image.is_some() { "custom" } else { "builtin" }
                );
                let old = model.update(cx, |m, _| std::mem::replace(&mut m.wallpaper, image));
                if let Some(old) = old {
                    cx.drop_image(old, None);
                }
                refresh = true;
            }
            Event::ClipboardChanged => {
                if let Some(ActivePopover::Picker(w)) = model.read(cx).active_popover {
                    let _ = w.update(cx, |view, w, cx| view.load(false, w, cx));
                }
            }
            Event::Picker(kind) => clipboard_picker::toggle(model.clone(), kind, cx),
            Event::Toggle => launcher::toggle(model.clone(), cx),
            Event::Summon(summon) => {
                drawer::summoned(model, summon, cx);
            }
            Event::MediaKey(key) => quick_settings::handle_media_key(model, key, cx),
            Event::QuickStatus(status) => {
                model.update(cx, |m, _| m.quick_status = status);
                if let Some(ActivePopover::QuickSettings(w)) = model.read(cx).active_popover {
                    let _ = w.update(cx, |_, _, cx| cx.notify());
                }
            }
            Event::Osd {
                kind,
                percent,
                muted,
            } => quick_settings::show_osd(model, kind, percent, muted, cx),
            Event::Quit => cx.quit(),
            Event::Snapshot(s) => {
                if model.read(cx).snapshot == s {
                    continue;
                }
                let locked = s.locked;
                let lock_changes = lock::lock_screen_changes(&s, model.read(cx).lock.is_some());
                model.update(cx, |m, _| m.snapshot = s);
                if lock_changes {
                    lock::set_lock_screen(model, locked, cx);
                }
                drawer::sync_mark_overlay(model, cx);
                refresh = true;
            }
            Event::Lock(locked) => {
                lock::set_lock_screen(model, locked, cx);
                model.update(cx, |m, _| m.snapshot.locked = locked);
                drawer::sync_mark_overlay(model, cx);
            }
            Event::Agent(agent_event) => {
                let effects = model.update(cx, |m, _| m.agent.handle_event(agent_event));
                if drawer::apply_effects_from_app(model, effects, cx) {
                    refresh = true;
                }
            }
            Event::Unlock(result) => {
                if let Some(w) = model.read(cx).lock {
                    let _ = w.update(cx, |v, _, cx| v.answered(result, cx));
                }
            }
            Event::AgentLaunch(result) => {
                match &result {
                    Ok(()) => log::info!("SHELL agent-sign-in launched=true"),
                    Err(error) => log::warn!("SHELL agent-sign-in launched=false error={error}"),
                }
                let effects = model.update(cx, |m, _| {
                    m.agent.handle_intent(AgentIntent::SignInLaunchResult(result))
                });
                if drawer::apply_effects_from_app(model, effects, cx) {
                    refresh = true;
                }
            }
            Event::Launch(result) => {
                if let Some(ActivePopover::Launcher(w)) = model.read(cx).active_popover {
                    match result {
                        Ok(()) => close_active_popover(model, cx),
                        Err(error) => {
                            let _ = w.update(cx, |v, _, cx| {
                                v.busy = false;
                                v.error = Some(error);
                                cx.notify();
                            });
                        }
                    }
                } else if let Err(error) = result {
                    launcher::toggle(model.clone(), cx);
                    if let Some(ActivePopover::Launcher(w)) = model.read(cx).active_popover {
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
    let launcher = model.read(cx).launcher_client.clone();
    let prefs = launcher.appearance();
    let (refresh, appearance_changed, timer_changed, calendar_handle) = model.update(cx, |m, _| {
        let mut refresh = false;
        let mut appearance_changed = false;
        if m.clock != now.bar || m.lock_time != now.lock_time || m.lock_date != now.lock_date {
            m.clock = now.bar;
            m.lock_time = now.lock_time;
            m.lock_date = now.lock_date;
            refresh = true;
        }
        if let Some(prefs) = prefs {
            if prefs != m.preferences {
                log::info!(
                    "SHELL appearance theme={:?} wallpaper={:?}",
                    prefs.theme,
                    prefs.wallpaper
                );
                m.preferences = prefs;
                refresh = true;
                appearance_changed = true;
            }
        }
        let timer_changed = m.timer.tick_one_second();
        let calendar_handle = match m.active_popover {
            Some(ActivePopover::Calendar(w)) => Some(w),
            _ => None,
        };
        (refresh, appearance_changed, timer_changed, calendar_handle)
    });
    if appearance_changed {
        sync_drawer_palette(model, cx);
    }
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
        Some("--launcher") => return control_server::send("launcher"),
        Some("--quit") => return control_server::send("quit"),
        Some("--start-session") => return control_server::start_session(),
        Some(_) => {
            anyhow::bail!("Use --version, --list-apps, --launcher, --quit or --start-session")
        }
        None => {}
    }

    let (tx, rx) = events::channel();
    let compositor = CompositorClient::connect(tx.clone());
    let launcher_client = LauncherClient::from_env();
    let unlock_client = UnlockClient::from_env();

    appearance::watch_wallpaper(tx.clone(), launcher_client.clone());
    appearance::watch_themes(tx.clone(), compositor.clone(), launcher_client.clone());

    let agent_tx = agent::start_from_env(tx.clone());
    let agent_configured = agent_tx.is_some();
    let (last_model, last_effort) = agent::load_picks();
    let agent_controller = AgentController::new(agent_configured, last_model, last_effort);
    let agent_view = Arc::new(Mutex::new(AgentViewSnapshot {
        state: agent_controller.state.clone(),
        ..Default::default()
    }));

    let _lock = control_server::server(tx.clone(), compositor.clone(), agent_view.clone())?;
    control_server::start_display_bridge(compositor.clone(), launcher_client.clone());

    let watcher = tx.clone();
    let watcher_compositor = compositor.clone();
    std::thread::spawn(move || {
        let mut last = None;
        loop {
            let snapshot = match watcher_compositor.snapshot() {
                Ok(snapshot) => snapshot,
                Err(e) if watcher_compositor.closed() => {
                    log::error!("SHELL exits: the compositor's control channel is closed: {e}");
                    std::process::exit(1);
                }
                Err(_) => compositor::Snapshot::default(),
            };
            if last.as_ref() != Some(&snapshot) {
                last = Some(snapshot.clone());
                if watcher.send(Event::Snapshot(snapshot)).is_err() {
                    break;
                }
            }
            watcher_compositor.read_events_for(Duration::from_millis(700));
        }
    });

    if let Err(e) = compositor.wait_for_a_display() {
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
        composer::bind(cx);
        cx.bind_keys([
            KeyBinding::new("down", Next, Some("ClipboardPicker")),
            KeyBinding::new("up", Previous, Some("ClipboardPicker")),
            KeyBinding::new("enter", Open, Some("ClipboardPicker")),
            KeyBinding::new("escape", Close, Some("ClipboardPicker")),
            KeyBinding::new("down", Next, Some("Launcher")),
            KeyBinding::new("up", Previous, Some("Launcher")),
            KeyBinding::new("tab", Next, Some("Launcher")),
            KeyBinding::new("shift-tab", Previous, Some("Launcher")),
            KeyBinding::new("enter", Open, Some("Launcher")),
            KeyBinding::new("escape", Close, Some("Launcher")),
            KeyBinding::new("escape", Close, Some("ThemeSwitcher")),
            KeyBinding::new("escape", Close, Some("QuickSettings")),
            KeyBinding::new("escape", Close, Some("Calendar")),
            KeyBinding::new("left", PrevMonth, Some("Calendar")),
            KeyBinding::new("right", NextMonth, Some("Calendar")),
            KeyBinding::new("enter", Unlock, Some("Lock")),
            KeyBinding::new("enter", SendMessage, Some("AgentSurface && !Composer")),
            KeyBinding::new("tab", Next, Some("AgentSurface")),
            KeyBinding::new("shift-tab", Previous, Some("AgentSurface")),
        ]);

        let now = ClockDisplay::now();
        let preferences = launcher_client.appearance().unwrap_or_default();
        log::info!(
            "SHELL appearance theme={:?} wallpaper={:?}",
            preferences.theme,
            preferences.wallpaper
        );

        let model = cx.new(|_| Model {
            snapshot: Default::default(),
            wallpaper: None,
            preferences,
            themes: None,
            active_popover: None,
            osd: None,
            quick_status: quick_settings::QuickStatus::default(),
            osd_state: None,
            osd_seq: 0,
            timer: calendar::QuickTimer::default(),
            lock: None,
            agent_window: None,
            mark_overlay: None,
            agent: agent_controller,
            agent_tx,
            agent_view,
            apps: catalog::discover(),
            tx,
            compositor,
            launcher_client,
            unlock_client,
            clock: now.bar,
            lock_time: now.lock_time,
            lock_date: now.lock_date,
            person: lock::person_label(),
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
                cx.new(|_| chrome::Background {
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
                cx.new(|_| chrome::Bar {
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
                cx.new(|_| chrome::Dock {
                    model: model.clone(),
                    hovered: None,
                })
            },
        )
        .expect("Dock layer");

        let events_model = model.clone();
        cx.spawn(async move |cx| {
            let mut rx = rx;
            while let Some(first) = rx.next().await {
                let mut waiting = vec![first];
                while let Ok(e) = rx.try_recv() {
                    waiting.push(e);
                }
                if cx
                    .update(|cx| {
                        if handle_events(&events_model, waiting, cx) {
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

    log::info!("SHELL exits: it quit, or has no layer left to draw");
    Ok(())
}
