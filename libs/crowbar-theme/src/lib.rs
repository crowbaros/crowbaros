// SPDX-License-Identifier: MIT
//! CrowbarOS native theming model, theme store, and per-app config generators.
//!
//! Pure Rust with zero GPUI or Wayland dependencies so it can be shared by
//! `crowbar-ui`, `apps/shell`, `apps/studio-settings`, `apps/files`, CLI tools, and tests.

pub mod builtin;
pub mod color;
pub mod generators;
pub mod store;

pub use builtin::{BUILTIN_THEME_IDS, builtin_theme, builtin_themes, is_builtin_id};
pub use color::{
    contrast_ratio, ensure_contrast, from_rgb, hex, hex_bare, hsl_to_rgb, mix, parse_hex, relative_luminance,
    rgb_components, rgb_to_hsl,
};
pub use generators::{render_foot_ini, render_gtk_css};
pub use store::ThemeStore;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Current on-disk JSON schema version for `theme.json`, `current-theme.json`,
/// and `appearance.json`.
pub const SCHEMA_VERSION: u32 = 1;

const fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

/// Broad tonal appearance family of a theme.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Appearance {
    #[default]
    Dark,
    Light,
    WarmPaper,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::Dark, Self::Light, Self::WarmPaper];

    pub const fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }

    pub const fn is_light(self) -> bool {
        !self.is_dark()
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
            Self::WarmPaper => "Warm Paper",
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::WarmPaper => "warm-paper",
        }
    }
}

/// Complete CrowbarOS semantic + 16-color terminal ANSI palette.
///
/// Colors are stored in-memory as 24-bit `0xRRGGBB` integers and serialized
/// in JSON as `"#rrggbb"` strings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeColors {
    #[serde(with = "color::hex_rgb")]
    pub canvas: u32,
    #[serde(with = "color::hex_rgb")]
    pub surface: u32,
    #[serde(with = "color::hex_rgb")]
    pub surface_elevated: u32,
    #[serde(with = "color::hex_rgb")]
    pub text: u32,
    #[serde(with = "color::hex_rgb")]
    pub muted: u32,
    #[serde(with = "color::hex_rgb")]
    pub line: u32,
    #[serde(with = "color::hex_rgb")]
    pub accent: u32,
    #[serde(with = "color::hex_rgb")]
    pub on_accent: u32,
    #[serde(with = "color::hex_rgb")]
    pub focus: u32,
    #[serde(with = "color::hex_rgb")]
    pub danger: u32,
    #[serde(with = "color::hex_rgb")]
    pub warning: u32,
    #[serde(with = "color::hex_rgb")]
    pub success: u32,
    #[serde(with = "color::hex_ansi")]
    pub ansi: [u32; 16],
}

impl ThemeColors {
    /// CrowbarOS neutral monochrome palette used when Studio Mode is enabled.
    pub const fn studio_neutral() -> Self {
        builtin::studio_neutral_colors()
    }
}

/// A discoverable CrowbarOS theme bundle (`theme.json` + optional `backgrounds/`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub author: String,
    pub appearance: Appearance,
    pub colors: ThemeColors,
    #[serde(default)]
    pub backgrounds: Vec<String>,
    #[serde(skip)]
    pub root_dir: Option<PathBuf>,
    #[serde(skip)]
    pub builtin: bool,
}

/// Live active theme state persisted in `<config_dir>/current-theme.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveTheme {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub theme_id: String,
    pub theme_name: String,
    pub appearance: Appearance,
    #[serde(default)]
    pub studio: bool,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub background_path: Option<PathBuf>,
    pub colors: ThemeColors,
    #[serde(default)]
    pub generation: u64,
}

/// Convert a human-readable theme name into a clean lowercase `[a-z0-9-]` slug.
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = true;
    for ch in input.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            out.push(lower);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.len() > 64 {
        out.truncate(64);
        while out.ends_with('-') {
            out.pop();
        }
    }
    out
}

/// Validate a theme ID slug: non-empty, <= 64 chars, only ASCII `[a-z0-9-]`,
/// no leading/trailing hyphens, and no path separators or `.` traversal characters.
pub fn validate_slug(slug: &str) -> Result<(), String> {
    if slug.is_empty() || slug.len() > 64 {
        return Err(format!(
            "Invalid theme id '{slug}': must be 1..=64 lowercase characters."
        ));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(format!("Invalid theme id '{slug}': must not start or end with '-'."));
    }
    if !slug
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(format!(
            "Invalid theme id '{slug}': only lowercase ASCII letters, digits, and '-' are allowed."
        ));
    }
    Ok(())
}

/// Validate a background filename: single relative filename with no path traversal,
/// separators, NUL bytes, or leading dot.
pub fn validate_filename(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 255 {
        return Err(format!("Invalid filename '{name}': must be 1..=255 bytes."));
    }
    if name == "."
        || name == ".."
        || name.starts_with('.')
        || name.contains("..")
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(format!(
            "Invalid filename '{name}': path traversal or separator characters are forbidden."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::Path,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TestFixture {
        root: PathBuf,
        store: ThemeStore,
    }

    impl TestFixture {
        fn new(label: &str) -> Self {
            let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let seq = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "crowbar-theme-test-{label}-{}-{stamp}-{seq}",
                std::process::id()
            ));
            let config_dir = root.join("xdg/crowbaros");
            let system_dir = root.join("system-themes");
            fs::create_dir_all(&config_dir).unwrap();
            fs::create_dir_all(&system_dir).unwrap();
            let store = ThemeStore::new(config_dir, system_dir);
            Self { root, store }
        }

        fn write_stub_png(&self, filename: &str, tag: &[u8]) -> PathBuf {
            let path = self.root.join(filename);
            let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
            bytes.extend_from_slice(tag);
            fs::write(&path, bytes).unwrap();
            path
        }
    }

    impl Drop for TestFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn hex_and_contrast_helpers_work() {
        assert_eq!(hex(0x080d12), "#080d12");
        assert_eq!(hex(0xff49_d7e8), "#49d7e8");
        assert_eq!(parse_hex("#49D7E8").unwrap(), 0x49d7e8);
        assert_eq!(parse_hex("080d12").unwrap(), 0x080d12);
        assert!(parse_hex("#fff").is_err());
        assert!(parse_hex("#12345g").is_err());
        assert!(parse_hex("").is_err());

        let bw = contrast_ratio(0xffffff, 0x000000);
        assert!((bw - 21.0).abs() < 0.01);
        assert!((contrast_ratio(0x123456, 0x123456) - 1.0).abs() < 0.001);

        let fixed_dark = ensure_contrast(0x223344, 0x080d12, 4.5);
        assert!(contrast_ratio(fixed_dark, 0x080d12) >= 4.5);

        let fixed_light = ensure_contrast(0xccddee, 0xf4f7f9, 4.5);
        assert!(contrast_ratio(fixed_light, 0xf4f7f9) >= 4.5);
    }

    #[test]
    fn builtin_themes_are_complete_and_match_repo_manifests() {
        let fx = TestFixture::new("builtins");
        let themes = fx.store.list_themes();
        assert_eq!(themes.len(), 7);
        let ids: Vec<&str> = themes.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, BUILTIN_THEME_IDS);

        // Verify exact CrowbarOS Nullkeel Dark/Light/Warm Paper + Studio values.
        let dark = fx.store.get_theme("nullkeel-dark").unwrap();
        assert_eq!(dark.colors.canvas, 0x080d12);
        assert_eq!(dark.colors.surface, 0x17212b);
        assert_eq!(dark.colors.accent, 0x49d7e8);
        assert_eq!(
            dark.backgrounds,
            vec!["signal-feather.png", "nightwatch.png", "quiet-signal.png"]
        );

        let light = fx.store.get_theme("nullkeel-light").unwrap();
        assert_eq!(light.colors.canvas, 0xf4f7f9);
        assert_eq!(light.colors.surface, 0xeaf0f3);
        assert_eq!(light.colors.accent, 0x49d7e8);
        assert_eq!(light.colors.focus, 0x086a78);

        let paper = fx.store.get_theme("warm-paper").unwrap();
        assert_eq!(paper.appearance, Appearance::WarmPaper);
        assert_eq!(paper.colors.canvas, 0xf7f2e8);
        assert_eq!(paper.colors.surface, 0xeae3d7);
        assert_eq!(paper.colors.focus, 0x526c6c);

        let studio = ThemeColors::studio_neutral();
        assert_eq!(studio.canvas, 0x111111);
        assert_eq!(studio.surface, 0x212121);
        assert_eq!(studio.surface_elevated, 0x2b2b2b);
        assert_eq!(studio.text, 0xe5e5e5);
        assert_eq!(studio.muted, 0xb8b8b8);
        assert_eq!(studio.line, 0x555555);
        assert_eq!(studio.accent, 0xe5e5e5);
        assert_eq!(studio.on_accent, 0x111111);
        assert_eq!(studio.focus, 0xe5e5e5);

        // Verify JSON serialization uses "#rrggbb" strings.
        let json = serde_json::to_value(&dark).unwrap();
        assert_eq!(json["colors"]["canvas"], "#080d12");
        assert_eq!(json["colors"]["ansi"].as_array().unwrap().len(), 16);
        assert_eq!(json["colors"]["ansi"][6], "#49d7e8");

        // Verify repo fallback resolves signal-feather.png -> apps/shell/assets/wallpaper.png.
        let resolved = fx
            .store
            .resolve_background_path(&dark, Some("signal-feather.png"))
            .expect("signal-feather resolves via repo fallback");
        assert!(resolved.is_file());
        assert_eq!(fx.store.list_background_paths(&dark).len(), 3);
    }

    #[test]
    fn custom_theme_save_list_and_delete() {
        let fx = TestFixture::new("custom-crud");
        let bg1 = fx.write_stub_png("Ocean Wave.png", b"ocean");
        let bg2 = fx.write_stub_png("dusk.png", b"dusk");

        let saved = fx
            .store
            .save_custom_theme("Ocean Signal", builtin::nord().colors, Appearance::Dark, &[bg1, bg2])
            .unwrap();

        assert_eq!(saved.id, "ocean-signal");
        assert_eq!(saved.name, "Ocean Signal");
        assert!(!saved.builtin);
        assert_eq!(saved.backgrounds, vec!["ocean-wave.png", "dusk.png"]);

        let resolved = fx
            .store
            .resolve_background_path(&saved, Some("ocean-wave.png"))
            .unwrap();
        assert!(resolved.is_file());
        assert_eq!(fx.store.list_background_paths(&saved).len(), 2);

        let all = fx.store.list_themes();
        assert_eq!(all.len(), 8);
        assert_eq!(all[7].id, "ocean-signal");

        // Built-in themes cannot be deleted.
        assert!(fx.store.delete_custom_theme("nullkeel-dark").is_err());
        assert!(fx.store.delete_custom_theme("nord").is_err());

        // Custom theme can be deleted cleanly.
        fx.store.delete_custom_theme("ocean-signal").unwrap();
        assert!(fx.store.get_theme("ocean-signal").is_none());
        assert_eq!(fx.store.list_themes().len(), 7);
    }

    #[test]
    fn multi_background_add_cycle_and_remove() {
        let fx = TestFixture::new("multi-bg");
        let bg1 = fx.write_stub_png("one.png", b"one");
        let bg2 = fx.write_stub_png("two.png", b"two");
        let bg3 = fx.write_stub_png("three.png", b"three");

        let created = fx
            .store
            .save_custom_theme("Multi BG", builtin::tokyo_night().colors, Appearance::Dark, &[bg1])
            .unwrap();
        assert_eq!(created.backgrounds, vec!["one.png"]);

        let added2 = fx.store.add_background("multi-bg", &bg2).unwrap();
        assert_eq!(added2.backgrounds, vec!["one.png", "two.png"]);
        let added3 = fx.store.add_background("multi-bg", &bg3).unwrap();
        assert_eq!(added3.backgrounds, vec!["one.png", "two.png", "three.png"]);

        // Apply and cycle through all 3 backgrounds and wrap around.
        let a1 = fx.store.apply_theme("multi-bg", None, Some(false)).unwrap();
        assert_eq!(a1.background.as_deref(), Some("one.png"));
        assert!(a1.background_path.as_ref().unwrap().is_file());

        let a2 = fx.store.cycle_background().unwrap();
        assert_eq!(a2.background.as_deref(), Some("two.png"));
        assert!(a2.generation > a1.generation);

        let a3 = fx.store.cycle_background().unwrap();
        assert_eq!(a3.background.as_deref(), Some("three.png"));

        let a4 = fx.store.cycle_background().unwrap();
        assert_eq!(a4.background.as_deref(), Some("one.png"));

        // Remove middle background and verify file is removed.
        let updated = fx.store.remove_background("multi-bg", "two.png").unwrap();
        assert_eq!(updated.backgrounds, vec!["one.png", "three.png"]);
        assert!(!fx.store.user_themes_dir().join("multi-bg/backgrounds/two.png").exists());
    }

    #[test]
    fn apply_theme_writes_current_theme_strict_appearance_foot_and_gtk() {
        let fx = TestFixture::new("apply");

        // Pre-seed appearance.json with reduced_motion=false to verify preservation.
        let initial_appearance = r#"{
            "schema_version": 1,
            "theme": "dark",
            "wallpaper": "nightwatch",
            "studio": false,
            "reduced_motion": false
        }"#;
        fs::write(fx.store.appearance_path(), initial_appearance).unwrap();

        // Fallback load_active reads legacy appearance.json before current-theme.json exists.
        let legacy_loaded = fx.store.load_active();
        assert_eq!(legacy_loaded.theme_id, "nullkeel-dark");
        assert_eq!(legacy_loaded.background.as_deref(), Some("nightwatch.png"));
        assert!(fx.store.active_fingerprint().is_some());

        // Apply Catppuccin Mocha.
        let active = fx
            .store
            .apply_theme("catppuccin-mocha", Some("nightwatch.png"), Some(false))
            .unwrap();
        assert_eq!(active.schema_version, 1);
        assert_eq!(active.theme_id, "catppuccin-mocha");
        assert_eq!(active.generation, 1);
        assert_eq!(active.colors.canvas, 0x1e1e2e);

        // Reload current-theme.json.
        let reloaded = fx.store.load_active();
        assert_eq!(reloaded.theme_id, "catppuccin-mocha");
        assert_eq!(reloaded.generation, 1);
        assert_eq!(reloaded.colors.accent, 0x89b4fa);

        // Verify strict 5-field appearance.json (compatible with studio-settings deny_unknown_fields).
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct StrictStudioPrefs {
            schema_version: u32,
            theme: Appearance,
            wallpaper: String,
            studio: bool,
            reduced_motion: bool,
        }
        let appearance_raw = fs::read_to_string(fx.store.appearance_path()).unwrap();
        let strict: StrictStudioPrefs = serde_json::from_str(&appearance_raw).unwrap();
        assert_eq!(strict.schema_version, 1);
        assert_eq!(strict.theme, Appearance::Dark);
        assert_eq!(strict.wallpaper, "nightwatch");
        assert!(!strict.studio);
        assert!(!strict.reduced_motion); // preserved!

        // Verify foot/theme.ini.
        let foot_path = fx.store.xdg_config_dir().join("foot/theme.ini");
        let foot = fs::read_to_string(&foot_path).unwrap();
        assert!(foot.contains("initial-color-theme=dark"));
        assert!(foot.contains("[colors-dark]"));
        assert!(foot.contains("[colors-light]"));
        assert!(foot.contains("background=1e1e2e"));
        assert!(foot.contains("foreground=cdd6f4"));
        assert!(foot.contains("cursor=11111b 89b4fa"));
        assert!(foot.contains("regular0=45475a"));
        assert!(foot.contains("bright7=a6adc8"));

        // Verify gtk-3.0/colors.css and gtk-4.0/colors.css.
        for ver in ["gtk-3.0", "gtk-4.0"] {
            let css_path = fx.store.xdg_config_dir().join(ver).join("colors.css");
            let css = fs::read_to_string(&css_path).unwrap();
            assert!(css.contains("@define-color window_bg_color #1e1e2e;"));
            assert!(css.contains("@define-color window_fg_color #cdd6f4;"));
            assert!(css.contains("@define-color view_bg_color #313244;"));
            assert!(css.contains("@define-color headerbar_bg_color #313244;"));
            assert!(css.contains("@define-color accent_bg_color #89b4fa;"));
            assert!(css.contains("@define-color accent_fg_color #11111b;"));
            assert!(css.contains("@define-color card_bg_color #45475a;"));
            assert!(css.contains("@define-color borders #585b70;"));
        }

        // Studio mode overrides effective colors to studio_neutral() while retaining theme_id & appearance.
        let studio_active = fx.store.apply_theme("nullkeel-light", None, Some(true)).unwrap();
        assert_eq!(studio_active.theme_id, "nullkeel-light");
        assert_eq!(studio_active.appearance, Appearance::Light);
        assert!(studio_active.studio);
        assert_eq!(studio_active.colors, ThemeColors::studio_neutral());
        assert_eq!(studio_active.generation, 2);
        let foot_studio = fs::read_to_string(&foot_path).unwrap();
        assert!(foot_studio.contains("initial-color-theme=dark"));
        assert!(foot_studio.contains("background=111111"));
    }

    #[test]
    fn path_traversal_and_invalid_inputs_are_rejected() {
        let fx = TestFixture::new("security");
        let valid_img = fx.write_stub_png("good.png", b"good");

        // Traversal in apply_theme.
        assert!(fx.store.apply_theme("../etc", None, None).is_err());
        assert!(fx.store.apply_theme("a/b", None, None).is_err());
        assert!(fx.store.apply_theme("", None, None).is_err());
        assert!(
            fx.store
                .apply_theme("nullkeel-dark", Some("../secret.png"), None)
                .is_err()
        );
        assert!(fx.store.apply_theme("nullkeel-dark", Some("sub/bg.png"), None).is_err());
        assert!(
            fx.store
                .apply_theme("nullkeel-dark", Some(".hidden.png"), None)
                .is_err()
        );

        // Traversal in save_custom_theme.
        assert!(
            fx.store
                .save_custom_theme("../escape", builtin::nord().colors, Appearance::Dark, &[])
                .is_err()
        );
        assert!(
            fx.store
                .save_custom_theme("bad/name", builtin::nord().colors, Appearance::Dark, &[])
                .is_err()
        );
        assert!(
            fx.store
                .save_custom_theme("   ", builtin::nord().colors, Appearance::Dark, &[])
                .is_err()
        );

        // Traversal in add_background / remove_background / delete_custom_theme.
        assert!(fx.store.add_background("../nord", &valid_img).is_err());
        assert!(fx.store.remove_background("nullkeel-dark", "../passwd").is_err());
        assert!(fx.store.delete_custom_theme("../nord").is_err());
        assert!(fx.store.delete_custom_theme("a/b").is_err());

        // Non-image file rejected in add_background.
        let bad_file = fx.root.join("notes.txt");
        fs::write(&bad_file, b"hello").unwrap();
        assert!(fx.store.add_background("nullkeel-dark", Path::new(&bad_file)).is_err());

        // resolve_background_path rejects traversal filenames.
        let dark = fx.store.get_theme("nullkeel-dark").unwrap();
        assert!(
            fx.store
                .resolve_background_path(&dark, Some("../../Cargo.toml"))
                .is_none()
        );
    }

    #[test]
    fn system_and_user_overrides_and_legacy_fallback_work() {
        let fx = TestFixture::new("overrides");

        // Legacy appearance.json with warm-paper + quiet-signal + studio=true.
        fs::write(
            fx.store.appearance_path(),
            r#"{"schema_version":1,"theme":"warm-paper","wallpaper":"quiet-signal","studio":true,"reduced_motion":true}"#,
        )
        .unwrap();
        let active = fx.store.load_active();
        assert_eq!(active.theme_id, "warm-paper");
        assert_eq!(active.appearance, Appearance::WarmPaper);
        assert!(active.studio);
        assert_eq!(active.background.as_deref(), Some("quiet-signal.png"));
        assert_eq!(active.colors, ThemeColors::studio_neutral());

        // Add a system theme and a user override of nord.
        let mut sys_theme = builtin::nord();
        sys_theme.id = "solarized-dark".into();
        sys_theme.name = "Solarized Dark".into();
        let sys_dir = fx.store.system_dir().join("solarized-dark");
        fs::create_dir_all(&sys_dir).unwrap();
        fs::write(
            sys_dir.join("theme.json"),
            serde_json::to_vec_pretty(&sys_theme).unwrap(),
        )
        .unwrap();

        let mut user_nord = builtin::nord();
        user_nord.colors.accent = 0xff8800;
        let user_nord_dir = fx.store.user_themes_dir().join("nord");
        fs::create_dir_all(&user_nord_dir).unwrap();
        fs::write(
            user_nord_dir.join("theme.json"),
            serde_json::to_vec_pretty(&user_nord).unwrap(),
        )
        .unwrap();

        let themes = fx.store.list_themes();
        assert_eq!(themes.len(), 8);
        assert_eq!(themes[5].id, "nord");
        assert_eq!(themes[5].colors.accent, 0xff8800);
        assert_eq!(themes[7].id, "solarized-dark");

        // Custom theme with non-legacy background keeps appearance.json strictly valid.
        let custom_bg = fx.write_stub_png("custom-art.png", b"custom");
        let custom = fx
            .store
            .save_custom_theme(
                "Zebra Theme",
                builtin::gruvbox_dark().colors,
                Appearance::Dark,
                &[custom_bg],
            )
            .unwrap();
        let applied = fx.store.apply_theme(&custom.id, None, Some(false)).unwrap();
        assert_eq!(applied.background.as_deref(), Some("custom-art.png"));
        let appearance_val: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(fx.store.appearance_path()).unwrap()).unwrap();
        let obj = appearance_val.as_object().unwrap();
        assert_eq!(obj.len(), 5);
        assert_eq!(obj["wallpaper"], "quiet-signal"); // preserved valid legacy wallpaper!

        // Deleting active custom theme falls back to nullkeel-dark.
        fx.store.delete_custom_theme(&custom.id).unwrap();
        assert_eq!(fx.store.load_active().theme_id, "nullkeel-dark");
    }

    #[test]
    fn ensure_contrast_flips_direction_for_mid_gray_backgrounds() {
        let flipped = ensure_contrast(0x909090, 0x808080, 4.5);
        assert!(contrast_ratio(flipped, 0x808080) >= 4.5);
    }

    #[test]
    fn all_builtin_themes_meet_wcag_readability_targets() {
        for theme in builtin_themes() {
            let c = &theme.colors;
            assert!(
                contrast_ratio(c.text, c.canvas) >= 7.0,
                "{}: text/canvas < 7.0",
                theme.id
            );
            assert!(
                contrast_ratio(c.text, c.surface) >= 4.5,
                "{}: text/surface < 4.5",
                theme.id
            );
            assert!(
                contrast_ratio(c.on_accent, c.accent) >= 4.5,
                "{}: on_accent/accent < 4.5",
                theme.id
            );
            assert!(
                contrast_ratio(c.muted, c.surface) >= 4.5,
                "{}: muted/surface < 4.5",
                theme.id
            );
            assert!(
                contrast_ratio(c.focus, c.canvas) >= 3.0,
                "{}: focus/canvas < 3.0",
                theme.id
            );
        }
    }
}
