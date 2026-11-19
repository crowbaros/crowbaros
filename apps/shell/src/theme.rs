// SPDX-License-Identifier: MIT
use serde::Deserialize;
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
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
#[derive(Clone, Debug, PartialEq, Deserialize)]
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
    pub fn load() -> Option<Self> {
        if std::env::var_os("CROWBAR_LAUNCH_SOCKET").is_some() {
            let reply =
                crate::session::lab_request(serde_json::json!({"op": "appearance"})).ok()?;
            let prefs: Self = serde_json::from_value(reply["preferences"].clone()).ok()?;
            return (prefs.schema_version == 1).then_some(prefs);
        }
        let home = std::env::var_os("HOME")?;
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| std::path::PathBuf::from(home).join(".config"));
        let p: Self =
            serde_json::from_slice(&std::fs::read(config.join("crowbaros/appearance.json")).ok()?)
                .ok()?;
        (p.schema_version == 1).then_some(p)
    }
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub canvas: u32,
    pub surface: u32,
    pub text: u32,
    pub muted: u32,
    pub line: u32,
    pub accent: u32,
    pub on_accent: u32,
    pub focus: u32,
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
    fn wallpaper_deserializes_kebab_case_variants() {
        assert_eq!(
            serde_json::from_str::<Wallpaper>("\"signal-feather\"").unwrap(),
            Wallpaper::SignalFeather
        );
        assert_eq!(
            serde_json::from_str::<Wallpaper>("\"nightwatch\"").unwrap(),
            Wallpaper::Nightwatch
        );
        assert_eq!(
            serde_json::from_str::<Wallpaper>("\"quiet-signal\"").unwrap(),
            Wallpaper::QuietSignal
        );
        assert_eq!(Wallpaper::default(), Wallpaper::SignalFeather);
    }

    #[test]
    fn preferences_without_wallpaper_field_loads_with_default() {
        let legacy = r#"{"schema_version":1,"theme":"light","studio":false,"reduced_motion":true}"#;
        let prefs: Preferences = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            prefs,
            Preferences {
                schema_version: 1,
                theme: Theme::Light,
                studio: false,
                reduced_motion: true,
                wallpaper: Wallpaper::SignalFeather,
            }
        );
    }

    #[test]
    fn preferences_with_wallpaper_field_loads_selected_wallpaper() {
        let json = r#"{"schema_version":1,"theme":"warm-paper","studio":true,"reduced_motion":false,"wallpaper":"quiet-signal"}"#;
        let prefs: Preferences = serde_json::from_str(json).unwrap();
        assert_eq!(prefs.theme, Theme::WarmPaper);
        assert!(prefs.studio);
        assert_eq!(prefs.wallpaper, Wallpaper::QuietSignal);
        assert_eq!(Palette::for_preferences(&prefs).canvas, 0x111111);
    }
}
