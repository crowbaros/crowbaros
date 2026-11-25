// SPDX-License-Identifier: MIT
//! CrowbarOS appearance preferences, typography tokens, and Nullkeel / Light / Warm Paper / Studio palettes.
use serde::Deserialize;
use std::path::PathBuf;

/// Interface font for human-facing chrome, navigation, menus, dialogs, and copy (`DESIGN.md`).
pub const FONT_UI: &str = "Manrope";
/// Technical font for paths, sizes, dates, permissions, shortcuts, and code previews (`DESIGN.md`).
pub const FONT_MONO: &str = "IBM Plex Mono";

/// Typography scale from `DESIGN.md` (in logical pixels).
pub const TEXT_SECTION: f32 = 14.;
pub const TEXT_BODY: f32 = 13.;
pub const TEXT_LABEL: f32 = 13.;
pub const TEXT_SUPPORTING: f32 = 12.;
pub const TEXT_TECHNICAL: f32 = 11.;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Theme {
    #[default]
    Dark,
    Light,
    WarmPaper,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Wallpaper {
    #[default]
    SignalFeather,
    Nightwatch,
    QuietSignal,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Preferences {
    pub schema_version: u32,
    pub theme: Theme,
    pub studio: bool,
    pub reduced_motion: bool,
    #[serde(default)]
    pub wallpaper: Wallpaper,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            theme: Theme::Dark,
            studio: false,
            reduced_motion: false,
            wallpaper: Wallpaper::default(),
        }
    }
}

impl Preferences {
    pub fn path() -> Option<PathBuf> {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(config.join("crowbaros/appearance.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        let Ok(prefs) = serde_json::from_slice::<Self>(&bytes) else {
            return Self::default();
        };
        if prefs.schema_version == 1 {
            prefs
        } else {
            Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub canvas: u32,
    pub surface: u32,
    pub text: u32,
    pub muted: u32,
    pub line: u32,
    pub accent: u32,
    pub on_accent: u32,
    pub focus: u32,
    /// Soft translucent fill for the active/selected row (0xRRGGBBAA).
    pub selection_bg: u32,
    /// Accent color for folder icons & section headers (keeps contrast on pale surfaces).
    pub signal_fg: u32,
}

impl Palette {
    pub fn for_preferences(p: &Preferences) -> Self {
        if p.studio {
            Self {
                canvas: 0x111111,
                surface: 0x212121,
                text: 0xe5e5e5,
                muted: 0xb8b8b8,
                line: 0x555555,
                accent: 0xe5e5e5,
                on_accent: 0x111111,
                focus: 0xe5e5e5,
                selection_bg: 0xe5e5e526,
                signal_fg: 0xe5e5e5,
            }
        } else {
            match p.theme {
                Theme::Dark => Self {
                    canvas: 0x080d12,
                    surface: 0x17212b,
                    text: 0xd8e5ec,
                    muted: 0xa8bac7,
                    line: 0x364957,
                    accent: 0x49d7e8,
                    on_accent: 0x080d12,
                    focus: 0x49d7e8,
                    selection_bg: 0x49d7e824,
                    signal_fg: 0x49d7e8,
                },
                Theme::Light => Self {
                    canvas: 0xf4f7f9,
                    surface: 0xeaf0f3,
                    text: 0x080d12,
                    muted: 0x3e515d,
                    line: 0xb2c2cc,
                    accent: 0x49d7e8,
                    on_accent: 0x080d12,
                    focus: 0x086a78,
                    selection_bg: 0x086a7822,
                    signal_fg: 0x086a78,
                },
                Theme::WarmPaper => Self {
                    canvas: 0xf7f2e8,
                    surface: 0xeae3d7,
                    text: 0x211f1c,
                    muted: 0x5c564b,
                    line: 0xbcb3a3,
                    accent: 0x49d7e8,
                    on_accent: 0x080d12,
                    focus: 0x526c6c,
                    selection_bg: 0x526c6c24,
                    signal_fg: 0x526c6c,
                },
            }
        }
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::for_preferences(&Preferences::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palettes_match_design_tokens() {
        let dark = Palette::for_preferences(&Preferences::default());
        assert_eq!(dark.canvas, 0x080d12);
        assert_eq!(dark.accent, 0x49d7e8);
        assert_eq!(dark.signal_fg, 0x49d7e8);

        let light = Palette::for_preferences(&Preferences {
            theme: Theme::Light,
            ..Default::default()
        });
        assert_eq!(light.canvas, 0xf4f7f9);
        assert_eq!(light.signal_fg, 0x086a78);

        let studio = Palette::for_preferences(&Preferences {
            studio: true,
            ..Default::default()
        });
        assert_eq!(studio.canvas, 0x111111);
        assert_eq!(studio.accent, 0xe5e5e5);
    }

    #[test]
    fn typography_tokens_match_design_system() {
        assert_eq!(FONT_UI, "Manrope");
        assert_eq!(FONT_MONO, "IBM Plex Mono");
        assert_eq!(TEXT_SECTION, 14.);
        assert_eq!(TEXT_BODY, 13.);
        assert_eq!(TEXT_LABEL, 13.);
        assert_eq!(TEXT_SUPPORTING, 12.);
        assert_eq!(TEXT_TECHNICAL, 11.);
    }
}
