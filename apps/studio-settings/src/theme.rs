// SPDX-License-Identifier: MIT
//! Studio Settings palette resolution and `crowbar-theme` store synchronization.

use crate::settings::{Preferences, Theme, Wallpaper};
use crowbar_theme::{ActiveTheme, Appearance, SCHEMA_VERSION, ThemeColors, ThemeStore, builtin};
pub use crowbar_ui::UiPalette as Palette;

/// Map a Studio Settings [`Theme`] choice to its canonical built-in `crowbar-theme` ID.
pub fn theme_id_for_choice(theme: Theme) -> &'static str {
    match theme {
        Theme::Dark => "nullkeel-dark",
        Theme::Light => "nullkeel-light",
        Theme::WarmPaper => "warm-paper",
    }
}

/// Map a Studio Settings [`Wallpaper`] choice to its canonical background filename.
pub fn background_for_wallpaper(wallpaper: Wallpaper) -> &'static str {
    match wallpaper {
        Wallpaper::SignalFeather => "signal-feather.png",
        Wallpaper::Nightwatch => "nightwatch.png",
        Wallpaper::QuietSignal => "quiet-signal.png",
    }
}

/// Resolve the [`Palette`] for a Studio Settings [`Preferences`] draft/preview.
pub fn palette_for_preferences(p: &Preferences) -> Palette {
    if p.studio {
        Palette::studio_neutral()
    } else {
        let theme = match p.theme {
            Theme::Dark => builtin::nullkeel_dark(),
            Theme::Light => builtin::nullkeel_light(),
            Theme::WarmPaper => builtin::warm_paper(),
        };
        Palette::from_theme(&theme)
    }
}

/// Build an in-memory [`ActiveTheme`] fallback when `ThemeStore::for_user()` is unavailable.
pub fn fallback_active_theme(p: &Preferences) -> ActiveTheme {
    let theme = match p.theme {
        Theme::Dark => builtin::nullkeel_dark(),
        Theme::Light => builtin::nullkeel_light(),
        Theme::WarmPaper => builtin::warm_paper(),
    };
    let colors = if p.studio {
        ThemeColors::studio_neutral()
    } else {
        theme.colors
    };
    ActiveTheme {
        schema_version: SCHEMA_VERSION,
        theme_id: theme.id,
        theme_name: theme.name,
        appearance: match p.theme {
            Theme::Dark => Appearance::Dark,
            Theme::Light => Appearance::Light,
            Theme::WarmPaper => Appearance::WarmPaper,
        },
        studio: p.studio,
        background: Some(background_for_wallpaper(p.wallpaper).to_string()),
        background_path: None,
        colors,
        generation: 0,
    }
}

/// Choose the active window [`Palette`]: while the person is previewing an unsaved
/// appearance edit, show the preview palette; otherwise follow the live [`ActiveTheme`].
pub fn resolve_palette(live: &ActiveTheme, draft: &Preferences, previewing: bool) -> Palette {
    if previewing {
        palette_for_preferences(draft)
    } else {
        Palette::from_active(live)
    }
}

/// Synchronize `ThemeStore` (`current-theme.json`, `foot/theme.ini`, `gtk-3.0`/`gtk-4.0`)
/// after Studio Settings commits `appearance.json`.
pub fn sync_theme_store(theme_store: &ThemeStore, prefs: &Preferences) -> Result<ActiveTheme, String> {
    theme_store.apply_theme(
        theme_id_for_choice(prefs.theme),
        Some(background_for_wallpaper(prefs.wallpaper)),
        Some(prefs.studio),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Store;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct SyncFixture {
        root: PathBuf,
        prefs_store: Store,
        theme_store: ThemeStore,
    }

    impl SyncFixture {
        fn new(label: &str) -> Self {
            let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "studio-theme-test-{label}-{}-{stamp}-{seq}",
                std::process::id()
            ));
            let config_dir = root.join("xdg/crowbaros");
            let system_dir = root.join("system-themes");
            fs::create_dir_all(&config_dir).unwrap();
            fs::create_dir_all(&system_dir).unwrap();
            let prefs_store = Store {
                path: config_dir.join("appearance.json"),
            };
            let theme_store = ThemeStore::new(config_dir, system_dir);
            Self {
                root,
                prefs_store,
                theme_store,
            }
        }
    }

    impl Drop for SyncFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn palette_for_preferences_preserves_exact_nullkeel_and_studio_tokens() {
        let dark = palette_for_preferences(&Preferences {
            theme: Theme::Dark,
            studio: false,
            ..Preferences::default()
        });
        assert!(dark.is_dark);
        assert_eq!(dark.canvas, 0x080d12);
        assert_eq!(dark.surface, 0x17212b);
        assert_eq!(dark.text, 0xd8e5ec);
        assert_eq!(dark.muted, 0xa8bac7);
        assert_eq!(dark.line, 0x364957);
        assert_eq!(dark.accent, 0x49d7e8);
        assert_eq!(dark.on_accent, 0x080d12);
        assert_eq!(dark.focus, 0x49d7e8);

        let light = palette_for_preferences(&Preferences {
            theme: Theme::Light,
            studio: false,
            ..Preferences::default()
        });
        assert!(!light.is_dark);
        assert_eq!(light.canvas, 0xf4f7f9);
        assert_eq!(light.surface, 0xeaf0f3);
        assert_eq!(light.focus, 0x086a78);

        let paper = palette_for_preferences(&Preferences {
            theme: Theme::WarmPaper,
            studio: false,
            ..Preferences::default()
        });
        assert!(!paper.is_dark);
        assert_eq!(paper.canvas, 0xf7f2e8);
        assert_eq!(paper.surface, 0xeae3d7);
        assert_eq!(paper.focus, 0x526c6c);

        let studio = palette_for_preferences(&Preferences {
            theme: Theme::Light,
            studio: true,
            ..Preferences::default()
        });
        assert!(studio.is_dark);
        assert_eq!(studio.canvas, 0x111111);
        assert_eq!(studio.surface, 0x212121);
        assert_eq!(studio.accent, 0xe5e5e5);
        assert_eq!(studio.on_accent, 0x111111);
    }

    #[test]
    fn resolve_palette_follows_live_custom_theme_unless_previewing() {
        let fx = SyncFixture::new("live-vs-preview");
        let tokyo = fx.theme_store.apply_theme("tokyo-night", None, Some(false)).unwrap();
        let draft = Preferences {
            theme: Theme::WarmPaper,
            studio: false,
            ..Preferences::default()
        };

        let live_palette = resolve_palette(&tokyo, &draft, false);
        assert_eq!(live_palette.canvas, 0x1a1b26);
        assert_eq!(live_palette.accent, 0x7aa2f7);

        let preview_palette = resolve_palette(&tokyo, &draft, true);
        assert_eq!(preview_palette.canvas, 0xf7f2e8);
        assert_eq!(preview_palette.focus, 0x526c6c);
    }

    #[test]
    fn sync_theme_store_writes_current_theme_foot_gtk_and_keeps_strict_appearance_json() {
        let fx = SyncFixture::new("sync");
        let motion = crate::session::Session::NoAnimations;
        let prefs = Preferences {
            schema_version: 1,
            theme: Theme::Light,
            wallpaper: Wallpaper::Nightwatch,
            studio: false,
            reduced_motion: false,
        };

        fx.prefs_store.save(&prefs, &None, &motion).unwrap();
        let active = sync_theme_store(&fx.theme_store, &prefs).unwrap();

        assert_eq!(active.theme_id, "nullkeel-light");
        assert_eq!(active.background.as_deref(), Some("nightwatch.png"));
        assert!(!active.studio);
        assert!(fx.theme_store.current_theme_path().is_file());
        assert!(fx.theme_store.xdg_config_dir().join("foot/theme.ini").is_file());
        assert!(fx.theme_store.xdg_config_dir().join("gtk-3.0/colors.css").is_file());
        assert!(fx.theme_store.xdg_config_dir().join("gtk-4.0/colors.css").is_file());

        // Strict appearance.json roundtrip still matches saved Preferences exactly.
        assert_eq!(fx.prefs_store.load().unwrap(), Some(prefs));
    }
}
