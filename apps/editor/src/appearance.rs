// SPDX-License-Identifier: MIT
use crowbar_ui::{FONT_MONO, FONT_UI, UiPalette};
use gpui::{App, px, rgb, rgba};
use gpui_component::highlighter::{SyntaxColors, ThemeStyle};
use gpui_component::{Theme, ThemeMode};
use std::sync::Arc;

// The component exposes its token style through the theme configuration format.
fn token(color: u32) -> Option<ThemeStyle> {
    Some(
        serde_json::from_value(serde_json::json!({"color":format!("#{color:06x}")}))
            .expect("a six-digit RGB color is a valid theme style"),
    )
}

pub fn apply(p: UiPalette, cx: &mut App) {
    Theme::change(if p.is_dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    let theme = Theme::global_mut(cx);
    theme.font_family = FONT_UI.into();
    theme.mono_font_family = FONT_MONO.into();
    theme.mono_font_size = px(14.);
    theme.background = rgb(p.canvas).into();
    theme.foreground = rgb(p.text).into();
    theme.caret = rgb(p.focus).into();
    theme.selection = rgba(p.selection_bg()).into();
    theme.border = rgb(p.line).into();
    theme.ring = rgb(p.focus).into();
    let style = &mut Arc::make_mut(&mut theme.highlight_theme).style;
    let accent = token(p.focus);
    let literal = token(p.success);
    let muted = token(p.muted);
    style.syntax = SyntaxColors {
        keyword: accent,
        function: accent,
        property: accent,
        title: accent,
        type_: accent,
        string: literal,
        text_literal: literal,
        number: token(p.warning),
        boolean: token(p.warning),
        comment: muted,
        comment_doc: muted,
        punctuation: muted,
        ..Default::default()
    };
    style.editor_background = Some(rgb(p.canvas).into());
    style.editor_foreground = Some(rgb(p.text).into());
    style.editor_line_number = Some(rgb(p.muted).into());
    style.editor_active_line = Some(rgb(p.surface).into());
    style.editor_active_line_number = Some(rgb(p.focus).into());
}
