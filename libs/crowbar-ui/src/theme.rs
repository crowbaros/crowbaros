// SPDX-License-Identifier: MIT
//! Live GPUI theme palette and filesystem change watcher backed by `crowbar-theme`.

use crowbar_theme::{ActiveTheme, Theme, ThemeColors, ThemeStore, contrast_ratio};
use std::time::Duration;

/// Default polling interval used by [`spawn_theme_watcher`].
pub const DEFAULT_WATCH_INTERVAL: Duration = Duration::from_millis(600);

/// Semantic color palette consumed by all CrowbarOS GPUI UI components.
///
/// Colors are 24-bit `0xRRGGBB` integers ready for `gpui::rgb(...)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiPalette {
    pub canvas: u32,
    pub surface: u32,
    pub surface_elevated: u32,
    pub text: u32,
    pub muted: u32,
    pub line: u32,
    pub accent: u32,
    pub on_accent: u32,
    pub focus: u32,
    pub danger: u32,
    pub warning: u32,
    pub success: u32,
    pub is_dark: bool,
}

impl UiPalette {
    /// Derive a [`UiPalette`] directly from a [`ThemeColors`] struct and dark/light flag.
    pub const fn from_colors(colors: &ThemeColors, dark: bool) -> Self {
        Self {
            canvas: colors.canvas,
            surface: colors.surface,
            surface_elevated: colors.surface_elevated,
            text: colors.text,
            muted: colors.muted,
            line: colors.line,
            accent: colors.accent,
            on_accent: colors.on_accent,
            focus: colors.focus,
            danger: colors.danger,
            warning: colors.warning,
            success: colors.success,
            is_dark: dark,
        }
    }

    /// Derive a [`UiPalette`] from a live [`ActiveTheme`].
    ///
    /// Respects `active.studio` by applying [`ThemeColors::studio_neutral()`] whenever
    /// Studio Mode is active.
    pub fn from_active(active: &ActiveTheme) -> Self {
        if active.studio {
            Self::studio_neutral()
        } else {
            Self::from_colors(&active.colors, active.appearance.is_dark())
        }
    }

    /// Derive a [`UiPalette`] from a [`Theme`] bundle.
    pub fn from_theme(theme: &Theme) -> Self {
        Self::from_colors(&theme.colors, theme.appearance.is_dark())
    }

    /// CrowbarOS neutral monochrome palette used when Studio Mode is enabled.
    pub const fn studio_neutral() -> Self {
        let neutral = ThemeColors::studio_neutral();
        Self::from_colors(&neutral, true)
    }

    /// High-contrast foreground color for text/borders drawn on top of [`Self::danger`].
    pub fn on_danger(&self) -> u32 {
        const DARK_INK: u32 = 0x080d12;
        const LIGHT_INK: u32 = 0xffffff;
        if contrast_ratio(DARK_INK, self.danger) >= contrast_ratio(LIGHT_INK, self.danger) {
            DARK_INK
        } else {
            LIGHT_INK
        }
    }

    /// Soft translucent fill (`0xRRGGBBAA`) for active/selected list and menu rows.
    pub const fn selection_bg(&self) -> u32 {
        ((self.focus & 0x00ff_ffff) << 8) | 0x24
    }

    /// High-contrast signal accent for icons and section labels on canvas/surface backgrounds.
    pub const fn signal_fg(&self) -> u32 {
        self.focus
    }
}

impl Default for UiPalette {
    fn default() -> Self {
        Self::from_theme(&crowbar_theme::builtin::nullkeel_dark())
    }
}

/// Lightweight change detector around [`ThemeStore`] using [`ThemeStore::active_fingerprint`].
#[derive(Clone, Debug)]
pub struct ThemeWatcher {
    store: ThemeStore,
    last_fingerprint: Option<(u64, u64)>,
}

impl ThemeWatcher {
    /// Construct a watcher around an explicit [`ThemeStore`], snapshotting its current
    /// active theme file fingerprint.
    pub fn new(store: ThemeStore) -> Self {
        let last_fingerprint = store.active_fingerprint();
        Self {
            store,
            last_fingerprint,
        }
    }

    /// Construct a watcher for the current user's [`ThemeStore::for_user`].
    pub fn for_user() -> Result<Self, String> {
        ThemeStore::for_user().map(Self::new)
    }

    /// Access the underlying [`ThemeStore`].
    pub fn store(&self) -> &ThemeStore {
        &self.store
    }

    /// Load the currently active theme from disk (or built-in fallback).
    pub fn current(&self) -> ActiveTheme {
        self.store.load_active()
    }

    /// Return the last observed `(mtime_nanos, byte_len)` fingerprint.
    pub fn last_fingerprint(&self) -> Option<(u64, u64)> {
        self.last_fingerprint
    }

    /// Synchronize the recorded fingerprint with disk without emitting a change event.
    pub fn sync_fingerprint(&mut self) {
        self.last_fingerprint = self.store.active_fingerprint();
    }

    /// Check whether the active theme file changed on disk since construction or the last
    /// [`Self::poll`], returning the newly loaded [`ActiveTheme`] when it changed.
    pub fn poll(&mut self) -> Option<ActiveTheme> {
        let fingerprint = self.store.active_fingerprint();
        if fingerprint != self.last_fingerprint {
            self.last_fingerprint = fingerprint;
            Some(self.store.load_active())
        } else {
            None
        }
    }
}

/// Spawn a background GPUI task that polls the user's active theme every 600ms and invokes
/// `on_change` whenever the active theme changes on disk.
///
/// The watcher stops automatically when the owning GPUI entity is dropped.
pub fn spawn_theme_watcher<T, F>(cx: &mut gpui::Context<T>, on_change: F)
where
    T: 'static,
    F: FnMut(&mut T, ActiveTheme, &mut gpui::Context<T>) + 'static,
{
    let Ok(watcher) = ThemeWatcher::for_user() else {
        return;
    };
    spawn_theme_watcher_with(watcher, DEFAULT_WATCH_INTERVAL, cx, on_change);
}

/// Spawn a GPUI theme watcher task with a custom [`ThemeWatcher`] and poll interval.
pub fn spawn_theme_watcher_with<T, F>(
    mut watcher: ThemeWatcher,
    interval: Duration,
    cx: &mut gpui::Context<T>,
    mut on_change: F,
) where
    T: 'static,
    F: FnMut(&mut T, ActiveTheme, &mut gpui::Context<T>) + 'static,
{
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(interval).await;
            if this.upgrade().is_none() {
                break;
            }
            let (next_watcher, changed) = cx
                .background_executor()
                .spawn(async move {
                    let changed = watcher.poll();
                    (watcher, changed)
                })
                .await;
            watcher = next_watcher;
            if let Some(active) = changed
                && this
                    .update(cx, |entity, cx| {
                        on_change(entity, active, cx);
                    })
                    .is_err()
            {
                break;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crowbar_theme::{Appearance, SCHEMA_VERSION, builtin};
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempStore {
        root: PathBuf,
        store: ThemeStore,
    }

    impl TempStore {
        fn new(label: &str) -> Self {
            let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let seq = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("crowbar-ui-test-{label}-{}-{stamp}-{seq}", std::process::id()));
            let config_dir = root.join("xdg/crowbaros");
            let system_dir = root.join("system-themes");
            fs::create_dir_all(&config_dir).unwrap();
            fs::create_dir_all(&system_dir).unwrap();
            let store = ThemeStore::new(config_dir, system_dir);
            Self { root, store }
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn make_active(theme: Theme, studio: bool) -> ActiveTheme {
        ActiveTheme {
            schema_version: SCHEMA_VERSION,
            theme_id: theme.id,
            theme_name: theme.name,
            appearance: theme.appearance,
            studio,
            background: theme.backgrounds.first().cloned(),
            background_path: None,
            colors: theme.colors,
            generation: 1,
        }
    }

    #[test]
    fn palette_from_active_handles_dark_light_warm_paper_and_studio_override() {
        let dark_theme = builtin::nullkeel_dark();
        let dark = UiPalette::from_active(&make_active(dark_theme.clone(), false));
        assert!(dark.is_dark);
        assert_eq!(dark.canvas, 0x080d12);
        assert_eq!(dark.surface, 0x17212b);
        assert_eq!(dark.surface_elevated, dark_theme.colors.surface_elevated);
        assert_eq!(dark.text, 0xd8e5ec);
        assert_eq!(dark.muted, 0xa8bac7);
        assert_eq!(dark.line, 0x364957);
        assert_eq!(dark.accent, 0x49d7e8);
        assert_eq!(dark.on_accent, 0x080d12);
        assert_eq!(dark.focus, 0x49d7e8);
        assert_eq!(dark.danger, dark_theme.colors.danger);
        assert_eq!(dark.warning, dark_theme.colors.warning);
        assert_eq!(dark.success, dark_theme.colors.success);

        let light = UiPalette::from_active(&make_active(builtin::nullkeel_light(), false));
        assert!(!light.is_dark);
        assert_eq!(light.canvas, 0xf4f7f9);
        assert_eq!(light.surface, 0xeaf0f3);
        assert_eq!(light.text, 0x080d12);
        assert_eq!(light.focus, 0x086a78);

        let paper = UiPalette::from_active(&make_active(builtin::warm_paper(), false));
        assert!(!paper.is_dark);
        assert_eq!(paper.canvas, 0xf7f2e8);
        assert_eq!(paper.surface, 0xeae3d7);
        assert_eq!(paper.focus, 0x526c6c);

        // Even if ActiveTheme carries light colors, `studio = true` MUST override
        // to ThemeColors::studio_neutral() and dark=true.
        let studio = UiPalette::from_active(&make_active(builtin::nullkeel_light(), true));
        let neutral = ThemeColors::studio_neutral();
        assert!(studio.is_dark);
        assert_eq!(studio, UiPalette::from_colors(&neutral, true));
        assert_eq!(studio.canvas, 0x111111);
        assert_eq!(studio.surface, 0x212121);
        assert_eq!(studio.accent, 0xe5e5e5);
        assert_eq!(studio.on_accent, 0x111111);
        assert_eq!(studio.focus, 0xe5e5e5);
    }

    #[test]
    fn palette_on_danger_maintains_wcag_contrast_across_builtins() {
        for theme in builtin::builtin_themes() {
            let p = UiPalette::from_theme(&theme);
            let fg = p.on_danger();
            assert!(
                contrast_ratio(fg, p.danger) >= 4.5,
                "on_danger contrast too low for {}: {:.2}",
                theme.id,
                contrast_ratio(fg, p.danger)
            );
        }
    }

    #[test]
    fn theme_watcher_detects_creation_switch_and_studio_changes() {
        let fx = TempStore::new("watcher");
        let mut watcher = ThemeWatcher::new(fx.store.clone());

        // Initially no theme file exists; poll reports no change.
        assert_eq!(watcher.last_fingerprint(), None);
        assert!(watcher.poll().is_none());
        assert_eq!(watcher.current().theme_id, "nullkeel-dark");

        // Applying a theme creates current-theme.json and triggers poll().
        fx.store.apply_theme("tokyo-night", None, Some(false)).unwrap();
        let changed = watcher.poll().expect("watcher must detect initial theme write");
        assert_eq!(changed.theme_id, "tokyo-night");
        assert_eq!(changed.appearance, Appearance::Dark);
        assert!(!changed.studio);
        assert!(watcher.last_fingerprint().is_some());

        // Polling again without disk writes returns None.
        assert!(watcher.poll().is_none());

        // Switching to a different theme updates fingerprint and returns the new theme.
        std::thread::sleep(Duration::from_millis(10));
        fx.store.apply_theme("catppuccin-mocha", None, Some(false)).unwrap();
        let next = watcher.poll().expect("watcher must detect theme switch");
        assert_eq!(next.theme_id, "catppuccin-mocha");
        assert_eq!(UiPalette::from_active(&next).canvas, 0x1e1e2e);
        assert!(watcher.poll().is_none());

        // Enabling Studio Mode triggers a change and yields studio neutral palette.
        std::thread::sleep(Duration::from_millis(10));
        fx.store.apply_theme("catppuccin-mocha", None, Some(true)).unwrap();
        let studio = watcher.poll().expect("watcher must detect studio toggle");
        assert!(studio.studio);
        assert_eq!(UiPalette::from_active(&studio), UiPalette::studio_neutral());

        // Removing current-theme.json + appearance.json triggers fallback change.
        fs::remove_file(fx.store.current_theme_path()).unwrap();
        fs::remove_file(fx.store.appearance_path()).unwrap();
        let fallback = watcher.poll().expect("watcher must detect theme file removal");
        assert_eq!(fallback.theme_id, "nullkeel-dark");
        assert_eq!(watcher.last_fingerprint(), None);
        assert!(watcher.poll().is_none());

        // sync_fingerprint suppresses a change event for self-initiated writes.
        fx.store.apply_theme("nord", None, Some(false)).unwrap();
        watcher.sync_fingerprint();
        assert!(watcher.poll().is_none());
        assert_eq!(watcher.current().theme_id, "nord");
    }

    #[test]
    fn palette_default_matches_nullkeel_dark() {
        let def = UiPalette::default();
        assert!(def.is_dark);
        assert_eq!(def.canvas, 0x080d12);
        assert_eq!(def.accent, 0x49d7e8);
        assert_eq!(def.signal_fg(), 0x49d7e8);
        assert_eq!(def.selection_bg(), 0x49d7e824);

        let light = UiPalette::from_theme(&builtin::nullkeel_light());
        assert_eq!(light.signal_fg(), 0x086a78);
        assert_eq!(light.selection_bg(), 0x086a7824);
    }

    #[test]
    fn typography_tokens_match_design_system() {
        assert_eq!(crate::FONT_UI, "Manrope");
        assert_eq!(crate::FONT_MONO, "IBM Plex Mono");
        assert_eq!(crate::TEXT_TITLE, 28.0);
        assert_eq!(crate::TEXT_SECTION, 14.0);
        assert_eq!(crate::TEXT_BODY, 13.0);
        assert_eq!(crate::TEXT_LABEL, 13.0);
        assert_eq!(crate::TEXT_SUPPORTING, 12.0);
        assert_eq!(crate::TEXT_TECHNICAL, 11.0);
        assert_eq!(crate::TEXT_SELECTION_LABEL, 10.0);
    }

    #[test]
    fn theme_watcher_detects_direct_appearance_json_updates() {
        let fx = TempStore::new("appearance-watch");
        let mut watcher = ThemeWatcher::new(fx.store.clone());
        assert!(watcher.poll().is_none());

        fs::write(
            fx.store.appearance_path(),
            r#"{"schema_version":1,"theme":"light","wallpaper":"signal-feather","studio":false,"reduced_motion":true}"#,
        )
        .unwrap();
        let light = watcher.poll().expect("watcher detects direct appearance.json write");
        assert_eq!(light.theme_id, "nullkeel-light");
        assert_eq!(UiPalette::from_active(&light).canvas, 0xf4f7f9);
        assert!(watcher.poll().is_none());

        // Even after current-theme.json exists, a newer appearance.json still triggers a refresh.
        fx.store.apply_theme("nullkeel-dark", None, Some(false)).unwrap();
        let dark = watcher.poll().expect("watcher detects apply_theme");
        assert_eq!(dark.theme_id, "nullkeel-dark");

        std::thread::sleep(Duration::from_millis(15));
        fs::write(
            fx.store.appearance_path(),
            r#"{"schema_version":1,"theme":"warm-paper","wallpaper":"quiet-signal","studio":false,"reduced_motion":true}"#,
        )
        .unwrap();
        let paper = watcher
            .poll()
            .expect("watcher detects newer appearance.json over current-theme.json");
        assert_eq!(paper.theme_id, "warm-paper");
        assert_eq!(UiPalette::from_active(&paper).canvas, 0xf7f2e8);
    }
}
