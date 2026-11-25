// SPDX-License-Identifier: MIT
//! Shared CrowbarOS GPUI 0.2.2 UI library: typography, live theme following,
//! and standard themed primitives matching `DESIGN.md`.

pub use crowbar_theme;

pub mod components;
pub mod theme;

pub use components::{
    ButtonVariant, action_chip, badge, button, card, divider, section_header, sidebar, status_footer, swatch_strip,
    titlebar,
};
pub use theme::{DEFAULT_WATCH_INTERVAL, ThemeWatcher, UiPalette, spawn_theme_watcher, spawn_theme_watcher_with};

use std::borrow::Cow;

/// Primary UI font family (bundled static Manrope weights 400, 600, 700).
pub const FONT_UI: &str = "Manrope";

/// Technical monospace font family (bundled IBM Plex Mono Regular).
pub const FONT_MONO: &str = "IBM Plex Mono";

/// Typography scale from `DESIGN.md` (in logical pixels).
pub const TEXT_TITLE: f32 = 28.0;
pub const TEXT_SECTION: f32 = 14.0;
pub const TEXT_BODY: f32 = 13.0;
pub const TEXT_LABEL: f32 = 13.0;
pub const TEXT_SUPPORTING: f32 = 12.0;
pub const TEXT_TECHNICAL: f32 = 11.0;
pub const TEXT_SELECTION_LABEL: f32 = 10.0;

/// Standard control border radius (`DESIGN.md`: `rounded.control`).
pub const RADIUS_CONTROL: f32 = 10.0;

/// Standard card border radius (`DESIGN.md`: `rounded.theme-card`).
pub const RADIUS_CARD: f32 = 12.0;

/// Standard inset preview / badge border radius (`DESIGN.md`: `rounded.theme-preview`).
pub const RADIUS_PREVIEW: f32 = 6.0;

/// Standard native window titlebar height in logical pixels (`DESIGN.md`).
pub const TITLEBAR_HEIGHT: f32 = 48.0;

/// Standard native sidebar width in logical pixels (`DESIGN.md`).
pub const SIDEBAR_WIDTH: f32 = 176.0;

/// Load the bundled CrowbarOS static fonts (Manrope Regular/SemiBold/Bold and
/// IBM Plex Mono Regular) into the GPUI text system.
pub fn load_fonts(cx: &mut gpui::App) {
    cx.text_system()
        .add_fonts(vec![
            Cow::Borrowed(include_bytes!("../fonts/Manrope-Regular.ttf")),
            Cow::Borrowed(include_bytes!("../fonts/Manrope-SemiBold.ttf")),
            Cow::Borrowed(include_bytes!("../fonts/Manrope-Bold.ttf")),
            Cow::Borrowed(include_bytes!("../fonts/IBMPlexMono-Regular.ttf")),
        ])
        .expect("bundled CrowbarOS fonts must load");
}
