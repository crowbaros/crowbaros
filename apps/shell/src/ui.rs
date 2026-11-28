// SPDX-License-Identifier: MIT
//! Shared GPUI shell primitives, global `Model`, and the `ActivePopover`
//! coordinator that replaces cross-calling `close_*` paths between overlays.

use gpui::{layer_shell::*, prelude::*, *};
use std::{
    borrow::Cow,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use crate::{
    agent, appearance, calendar, catalog, chrome, clipboard_picker, compositor, drawer, events,
    launcher, launcher_client, lock, quick_settings, unlock,
};

actions!(
    shell,
    [
        Next,
        Previous,
        Open,
        Close,
        Unlock,
        SendMessage,
        PrevMonth,
        NextMonth
    ]
);

pub const BAR_HEIGHT: f32 = 24.;
pub const DOCK_WIDTH: f32 = 260.;
pub const DOCK_HEIGHT: f32 = 48.;
pub const DOCK_LAYER_HEIGHT: f32 = 76.;

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(Some(Cow::Borrowed(match path {
            "apps" => include_bytes!("../assets/apps.svg"),
            "files" => include_bytes!("../assets/files.svg"),
            "browser" => include_bytes!("../assets/browser.svg"),
            "terminal" => include_bytes!("../assets/terminal.svg"),
            "palette" => include_bytes!("../assets/palette.svg"),
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

pub fn icon(name: &'static str, color: u32, size: f32) -> Svg {
    svg().path(name).size(px(size)).text_color(rgb(color))
}

pub fn asset(name: &str) -> PathBuf {
    let installed = PathBuf::from("/usr/share/crowbaros-shell").join(name);
    if installed.exists() {
        installed
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets")
            .join(name)
    }
}

pub fn palette_keycap(label: &'static str, p: appearance::Palette) -> Div {
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

pub fn layer(
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

#[allow(clippy::too_many_arguments)]
pub fn layer_with_margin(
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

pub struct ClockDisplay {
    pub bar: String,
    pub lock_time: String,
    pub lock_date: String,
}

impl ClockDisplay {
    pub fn now() -> Self {
        let now = chrono::Local::now();
        Self {
            bar: now.format("%a, %b %-d · %H:%M").to_string(),
            lock_time: now.format("%H:%M").to_string(),
            lock_date: now.format("%A, %B %-d").to_string(),
        }
    }
}

/// Exclusive shell modal popovers coordinated so at most one is open at a time.
#[derive(Clone, Copy)]
pub enum ActivePopover {
    Launcher(WindowHandle<launcher::Launcher>),
    QuickSettings(WindowHandle<quick_settings::QuickSettingsPopover>),
    Calendar(WindowHandle<calendar::CalendarPopover>),
    ThemeSwitcher(WindowHandle<appearance::ThemeSwitcher>),
    Picker(WindowHandle<clipboard_picker::Picker>),
}

impl ActivePopover {
    /// Closes the popover window through its own view `close` method, returning
    /// `true` if the window handle was still live.
    pub fn close(self, cx: &mut App) -> bool {
        match self {
            ActivePopover::Launcher(w) => w.update(cx, |view, w, cx| view.close(w, cx)).is_ok(),
            ActivePopover::QuickSettings(w) => {
                w.update(cx, |view, w, cx| view.close(w, cx)).is_ok()
            }
            ActivePopover::Calendar(w) => w.update(cx, |view, w, cx| view.close(w, cx)).is_ok(),
            ActivePopover::ThemeSwitcher(w) => {
                w.update(cx, |view, w, cx| view.close(w, cx)).is_ok()
            }
            ActivePopover::Picker(w) => w.update(cx, |view, w, cx| view.close(w, cx)).is_ok(),
        }
    }

    fn same_kind(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }
}

/// Closes an active popover slot without clearing it before the view's own
/// `close` runs (so views that guard their `open=false` log on `is_*_open()`
/// still observe the open handle). If the window handle was already gone,
/// clears the stale slot.
fn close_popover_slot<C, T: Copy>(
    cx: &mut C,
    read: impl FnOnce(&C) -> Option<T>,
    close: impl FnOnce(T, &mut C) -> bool,
    clear_if: impl FnOnce(T, &mut C),
) {
    let Some(popover) = read(cx) else {
        return;
    };
    if !close(popover, cx) {
        clear_if(popover, cx);
    }
}

/// Closes whichever modal popover is currently open, if any.
pub fn close_active_popover(model: &Entity<Model>, cx: &mut App) {
    close_popover_slot(
        cx,
        |cx| model.read(cx).active_popover,
        |popover, cx| popover.close(cx),
        |popover, cx| clear_active_popover_if(model, |p| p.same_kind(popover), cx),
    );
}

/// Clears `active_popover` only when the stored handle still matches `predicate`.
pub fn clear_active_popover_if(
    model: &Entity<Model>,
    predicate: impl FnOnce(ActivePopover) -> bool,
    cx: &mut App,
) {
    model.update(cx, |m, _| {
        if m.active_popover.is_some_and(predicate) {
            m.active_popover = None;
        }
    });
}

pub struct Model {
    pub snapshot: compositor::Snapshot,
    pub preferences: appearance::Preferences,
    pub themes: Option<appearance::ThemeCatalog>,
    pub wallpaper: Option<Arc<RenderImage>>,
    pub active_popover: Option<ActivePopover>,
    pub osd: Option<WindowHandle<quick_settings::OsdOverlay>>,
    pub quick_status: quick_settings::QuickStatus,
    pub osd_state: Option<quick_settings::OsdState>,
    pub osd_seq: u64,
    pub timer: calendar::QuickTimer,
    pub lock: Option<WindowHandle<lock::LockScreen>>,
    /// The Agent's surface, while it is up.
    pub agent_window: Option<WindowHandle<drawer::AgentSurface>>,
    /// The floating Agent mark shown over a fullscreen window.
    pub mark_overlay: Option<(WindowHandle<chrome::MarkOverlay>, String)>,
    /// Unified MVU controller for the Agent's surface and mark.
    pub agent: agent::AgentController,
    pub agent_tx: Option<agent::CommandSender>,
    pub agent_view: Arc<Mutex<agent::AgentViewSnapshot>>,
    pub apps: Vec<catalog::AppEntry>,
    pub tx: events::Sender,
    pub compositor: compositor::CompositorClient,
    pub launcher_client: launcher_client::LauncherClient,
    pub unlock_client: unlock::UnlockClient,
    pub clock: String,
    pub lock_time: String,
    pub lock_date: String,
    pub person: String,
}

impl Model {
    pub fn palette(&self) -> appearance::Palette {
        self.themes
            .as_ref()
            .map(|t| appearance::Palette::from_colors(&t.active.colors))
            .unwrap_or_else(|| appearance::Palette::for_preferences(&self.preferences))
    }

    pub fn is_launcher_open(&self) -> bool {
        matches!(self.active_popover, Some(ActivePopover::Launcher(_)))
    }

    pub fn is_quick_settings_open(&self) -> bool {
        matches!(self.active_popover, Some(ActivePopover::QuickSettings(_)))
    }

    pub fn is_calendar_open(&self) -> bool {
        matches!(self.active_popover, Some(ActivePopover::Calendar(_)))
    }

    pub fn is_theme_switcher_open(&self) -> bool {
        matches!(self.active_popover, Some(ActivePopover::ThemeSwitcher(_)))
    }

    pub fn is_picker_open(&self) -> bool {
        matches!(self.active_popover, Some(ActivePopover::Picker(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::close_popover_slot;

    #[test]
    fn close_popover_slot_keeps_slot_visible_during_view_close_and_clears_stale_handles() {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Kind {
            Calendar,
            QuickSettings,
        }

        // Live popover: view's close sees the slot still open, logs once, and clears it.
        let mut slot = Some(Kind::Calendar);
        let mut logged = 0usize;
        close_popover_slot(
            &mut slot,
            |s| *s,
            |kind, s| {
                if *s == Some(kind) {
                    logged += 1;
                }
                *s = None;
                true
            },
            |_, _| panic!("live close should not fall back to stale clear"),
        );
        assert_eq!(logged, 1);
        assert_eq!(slot, None);

        // Stale handle (view already gone): close returns false, coordinator clears slot.
        let mut slot = Some(Kind::QuickSettings);
        close_popover_slot(
            &mut slot,
            |s| *s,
            |_, _| false,
            |kind, s| {
                if *s == Some(kind) {
                    *s = None;
                }
            },
        );
        assert_eq!(slot, None);
    }
}
