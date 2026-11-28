// SPDX-License-Identifier: MIT
//! Unified appearance module: theme preferences & palettes, installed theme
//! catalog watcher, bounded custom wallpaper watcher, and Quick Theme Switcher.

pub mod catalog;
pub mod switcher;
pub mod theme;
pub mod wallpaper;

pub use catalog::{ThemeCatalog, watch as watch_themes};
pub use switcher::{ThemeSwitcher, toggle as toggle_theme_switcher};
pub use theme::{Palette, Preferences, Theme, Wallpaper};
pub use wallpaper::{CustomWallpaper, watch as watch_wallpaper};
