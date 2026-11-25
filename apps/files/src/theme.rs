// SPDX-License-Identifier: MIT
//! CrowbarOS Files typography and palette bindings backed by `crowbar-theme` and `crowbar-ui`.

pub use crowbar_theme::{ActiveTheme, ThemeStore};
pub use crowbar_ui::{
    FONT_MONO, FONT_UI, TEXT_BODY, TEXT_LABEL, TEXT_SECTION, TEXT_SUPPORTING, TEXT_TECHNICAL, UiPalette as Palette,
};

/// Load the currently active theme from the person's [`ThemeStore`], falling back to Nullkeel Dark.
pub fn load_active_theme() -> ActiveTheme {
    ThemeStore::for_user()
        .map(|store| store.load_active())
        .unwrap_or_else(|_| {
            let dark = crowbar_theme::builtin::nullkeel_dark();
            ActiveTheme {
                schema_version: crowbar_theme::SCHEMA_VERSION,
                theme_id: dark.id,
                theme_name: dark.name,
                appearance: dark.appearance,
                studio: false,
                background: dark.backgrounds.first().cloned(),
                background_path: None,
                colors: dark.colors,
                generation: 0,
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crowbar_theme::{Appearance, SCHEMA_VERSION, builtin};

    fn active_for(theme: crowbar_theme::Theme, studio: bool) -> ActiveTheme {
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
    fn palettes_match_design_tokens() {
        let dark = Palette::from_active(&active_for(builtin::nullkeel_dark(), false));
        assert_eq!(dark.canvas, 0x080d12);
        assert_eq!(dark.accent, 0x49d7e8);
        assert_eq!(dark.signal_fg(), 0x49d7e8);

        let light = Palette::from_active(&active_for(builtin::nullkeel_light(), false));
        assert_eq!(light.canvas, 0xf4f7f9);
        assert_eq!(light.signal_fg(), 0x086a78);

        let paper = Palette::from_active(&active_for(builtin::warm_paper(), false));
        assert_eq!(paper.canvas, 0xf7f2e8);
        assert_eq!(paper.signal_fg(), 0x526c6c);

        let studio = Palette::from_active(&active_for(builtin::nullkeel_light(), true));
        assert_eq!(studio.canvas, 0x111111);
        assert_eq!(studio.accent, 0xe5e5e5);
        assert_eq!(studio.signal_fg(), 0xe5e5e5);
        assert_eq!(Appearance::Dark.as_str(), "dark");
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
