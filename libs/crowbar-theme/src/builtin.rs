// SPDX-License-Identifier: MIT
//! Embedded built-in CrowbarOS themes, mirrored in `themes/<id>/theme.json`.

use crate::{Appearance, SCHEMA_VERSION, Theme, ThemeColors};

/// Canonical ordering of built-in theme IDs.
pub const BUILTIN_THEME_IDS: [&str; 7] = [
    "nullkeel-dark",
    "nullkeel-light",
    "warm-paper",
    "catppuccin-mocha",
    "tokyo-night",
    "nord",
    "gruvbox-dark",
];

/// Returns `true` if `id` is one of the embedded built-in themes.
pub fn is_builtin_id(id: &str) -> bool {
    BUILTIN_THEME_IDS.contains(&id)
}

/// Returns the canonical sort index of a built-in theme ID, if built-in.
pub(crate) fn builtin_order(id: &str) -> Option<usize> {
    BUILTIN_THEME_IDS.iter().position(|&b| b == id)
}

/// Return all 7 embedded built-in themes in canonical display order.
pub fn builtin_themes() -> Vec<Theme> {
    vec![
        nullkeel_dark(),
        nullkeel_light(),
        warm_paper(),
        catppuccin_mocha(),
        tokyo_night(),
        nord(),
        gruvbox_dark(),
    ]
}

/// Look up a single embedded built-in theme by ID.
pub fn builtin_theme(id: &str) -> Option<Theme> {
    match id {
        "nullkeel-dark" => Some(nullkeel_dark()),
        "nullkeel-light" => Some(nullkeel_light()),
        "warm-paper" => Some(warm_paper()),
        "catppuccin-mocha" => Some(catppuccin_mocha()),
        "tokyo-night" => Some(tokyo_night()),
        "nord" => Some(nord()),
        "gruvbox-dark" => Some(gruvbox_dark()),
        _ => None,
    }
}

/// Neutral monochrome palette used when CrowbarOS Studio Mode is enabled.
pub const fn studio_neutral_colors() -> ThemeColors {
    ThemeColors {
        canvas: 0x111111,
        surface: 0x212121,
        surface_elevated: 0x2b2b2b,
        text: 0xe5e5e5,
        muted: 0xb8b8b8,
        line: 0x555555,
        accent: 0xe5e5e5,
        on_accent: 0x111111,
        focus: 0xe5e5e5,
        danger: 0xe06c75,
        warning: 0xe5c07b,
        success: 0x98c379,
        ansi: [
            0x111111, 0xd46a6a, 0x7cb885, 0xd4b66a, 0x7a9ec2, 0xb084b8, 0x6fb3b8, 0xc8c8c8, 0x555555, 0xe88787,
            0x98d1a1, 0xe8cd87, 0x99bae0, 0xc9a0d1, 0x8fd1d6, 0xe5e5e5,
        ],
    }
}

pub fn nullkeel_dark() -> Theme {
    Theme {
        schema_version: SCHEMA_VERSION,
        id: "nullkeel-dark".into(),
        name: "Nullkeel Dark".into(),
        author: "CrowbarOS".into(),
        appearance: Appearance::Dark,
        colors: ThemeColors {
            canvas: 0x080d12,
            surface: 0x17212b,
            surface_elevated: 0x1f2d3a,
            text: 0xd8e5ec,
            muted: 0xa8bac7,
            line: 0x364957,
            accent: 0x49d7e8,
            on_accent: 0x080d12,
            focus: 0x49d7e8,
            danger: 0xf07178,
            warning: 0xf2c97d,
            success: 0x7fd4a6,
            ansi: [
                0x0f1720, 0xe06c75, 0x6bc99a, 0xe8c170, 0x5fa8e8, 0xb88ce8, 0x49d7e8, 0xc4d5df, 0x4b6070, 0xf28b92,
                0x8be0b5, 0xf5d790, 0x82c0f5, 0xd0aef5, 0x7ce6f2, 0xeaf2f7,
            ],
        },
        backgrounds: vec![
            "signal-feather.png".into(),
            "nightwatch.png".into(),
            "quiet-signal.png".into(),
        ],
        root_dir: None,
        builtin: true,
    }
}

pub fn nullkeel_light() -> Theme {
    Theme {
        schema_version: SCHEMA_VERSION,
        id: "nullkeel-light".into(),
        name: "Nullkeel Light".into(),
        author: "CrowbarOS".into(),
        appearance: Appearance::Light,
        colors: ThemeColors {
            canvas: 0xf4f7f9,
            surface: 0xeaf0f3,
            surface_elevated: 0xffffff,
            text: 0x080d12,
            muted: 0x3e515d,
            line: 0xb2c2cc,
            accent: 0x49d7e8,
            on_accent: 0x080d12,
            focus: 0x086a78,
            danger: 0xc23640,
            warning: 0x9c6508,
            success: 0x1e7a52,
            ansi: [
                0x080d12, 0xc23640, 0x1e7a52, 0x9c6508, 0x1d5fa3, 0x7c3fa3, 0x086a78, 0x5a6e7c, 0x3e515d, 0xd94550,
                0x289465, 0xb57912, 0x2b75c2, 0x9452bf, 0x0c8799, 0x080d12,
            ],
        },
        backgrounds: vec![
            "signal-feather.png".into(),
            "quiet-signal.png".into(),
            "nightwatch.png".into(),
        ],
        root_dir: None,
        builtin: true,
    }
}

pub fn warm_paper() -> Theme {
    Theme {
        schema_version: SCHEMA_VERSION,
        id: "warm-paper".into(),
        name: "Warm Paper".into(),
        author: "CrowbarOS".into(),
        appearance: Appearance::WarmPaper,
        colors: ThemeColors {
            canvas: 0xf7f2e8,
            surface: 0xeae3d7,
            surface_elevated: 0xfcf9f2,
            text: 0x211f1c,
            muted: 0x5c564b,
            line: 0xbcb3a3,
            accent: 0x49d7e8,
            on_accent: 0x080d12,
            focus: 0x526c6c,
            danger: 0xb53932,
            warning: 0x996414,
            success: 0x2f7048,
            ansi: [
                0x211f1c, 0xb53932, 0x2f7048, 0x996414, 0x2d5b85, 0x7c4678, 0x2b6e75, 0x6b6458, 0x5c564b, 0xcf4a42,
                0x3d8a5a, 0xb3781e, 0x3d72a3, 0x965891, 0x39878f, 0x211f1c,
            ],
        },
        backgrounds: vec![
            "quiet-signal.png".into(),
            "signal-feather.png".into(),
            "nightwatch.png".into(),
        ],
        root_dir: None,
        builtin: true,
    }
}

pub fn catppuccin_mocha() -> Theme {
    Theme {
        schema_version: SCHEMA_VERSION,
        id: "catppuccin-mocha".into(),
        name: "Catppuccin Mocha".into(),
        author: "Catppuccin".into(),
        appearance: Appearance::Dark,
        colors: ThemeColors {
            canvas: 0x1e1e2e,
            surface: 0x313244,
            surface_elevated: 0x45475a,
            text: 0xcdd6f4,
            muted: 0xa6adc8,
            line: 0x585b70,
            accent: 0x89b4fa,
            on_accent: 0x11111b,
            focus: 0xb4befe,
            danger: 0xf38ba8,
            warning: 0xf9e2af,
            success: 0xa6e3a1,
            ansi: [
                0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de, 0x585b70, 0xf38ba8,
                0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8,
            ],
        },
        backgrounds: vec!["nightwatch.png".into(), "signal-feather.png".into()],
        root_dir: None,
        builtin: true,
    }
}

pub fn tokyo_night() -> Theme {
    Theme {
        schema_version: SCHEMA_VERSION,
        id: "tokyo-night".into(),
        name: "Tokyo Night".into(),
        author: "Tokyo Night".into(),
        appearance: Appearance::Dark,
        colors: ThemeColors {
            canvas: 0x1a1b26,
            surface: 0x24283b,
            surface_elevated: 0x2f3549,
            text: 0xc0caf5,
            muted: 0x9aa5ce,
            line: 0x414868,
            accent: 0x7aa2f7,
            on_accent: 0x15161e,
            focus: 0x7dcfff,
            danger: 0xf7768e,
            warning: 0xe0af68,
            success: 0x9ece6a,
            ansi: [
                0x15161e, 0xf7768e, 0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xa9b1d6, 0x414868, 0xf7768e,
                0x9ece6a, 0xe0af68, 0x7aa2f7, 0xbb9af7, 0x7dcfff, 0xc0caf5,
            ],
        },
        backgrounds: vec!["nightwatch.png".into(), "signal-feather.png".into()],
        root_dir: None,
        builtin: true,
    }
}

pub fn nord() -> Theme {
    Theme {
        schema_version: SCHEMA_VERSION,
        id: "nord".into(),
        name: "Nord".into(),
        author: "Arctic Ice Studio".into(),
        appearance: Appearance::Dark,
        colors: ThemeColors {
            canvas: 0x2e3440,
            surface: 0x3b4252,
            surface_elevated: 0x434c5e,
            text: 0xeceff4,
            muted: 0xd8dee9,
            line: 0x4c566a,
            accent: 0x88c0d0,
            on_accent: 0x2e3440,
            focus: 0x81a1c1,
            danger: 0xbf616a,
            warning: 0xebcb8b,
            success: 0xa3be8c,
            ansi: [
                0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0, 0x4c566a, 0xbf616a,
                0xa3be8c, 0xebcb8b, 0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
            ],
        },
        backgrounds: vec!["signal-feather.png".into(), "quiet-signal.png".into()],
        root_dir: None,
        builtin: true,
    }
}

pub fn gruvbox_dark() -> Theme {
    Theme {
        schema_version: SCHEMA_VERSION,
        id: "gruvbox-dark".into(),
        name: "Gruvbox Dark".into(),
        author: "Gruvbox".into(),
        appearance: Appearance::Dark,
        colors: ThemeColors {
            canvas: 0x282828,
            surface: 0x3c3836,
            surface_elevated: 0x504945,
            text: 0xebdbb2,
            muted: 0xbdae93,
            line: 0x665c54,
            accent: 0xfabd2f,
            on_accent: 0x282828,
            focus: 0xfe8019,
            danger: 0xfb4934,
            warning: 0xfabd2f,
            success: 0xb8bb26,
            ansi: [
                0x282828, 0xcc241d, 0x98971a, 0xd79921, 0x458588, 0xb16286, 0x689d6a, 0xa89984, 0x928374, 0xfb4934,
                0xb8bb26, 0xfabd2f, 0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
            ],
        },
        backgrounds: vec!["nightwatch.png".into(), "quiet-signal.png".into()],
        root_dir: None,
        builtin: true,
    }
}
