// SPDX-License-Identifier: MIT
//! Standard themed CrowbarOS GPUI 0.2.2 UI primitives following `DESIGN.md`.

use crate::{
    RADIUS_CARD, RADIUS_CONTROL, RADIUS_PREVIEW, TEXT_LABEL, TEXT_SECTION, TEXT_SUPPORTING, TEXT_TECHNICAL,
    TITLEBAR_HEIGHT, UiPalette,
};
use gpui::{
    Div, ElementId, FocusHandle, FontWeight, IntoElement, MouseButton, SharedString, Stateful, Window, div, prelude::*,
    px, rgb, rgba, transparent_black,
};

/// Visual intent variants for [`button`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonVariant {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// Standard CrowbarOS button (`DESIGN.md`: 40px height, 10px radius, 2px border,
/// dashed focus border vs solid resting border, Manrope 13px SemiBold).
///
/// `focus_handle` accepts either `&FocusHandle` or `Option<&FocusHandle>` (e.g. `None`).
pub fn button<'a>(
    id: impl Into<ElementId>,
    focus_handle: impl Into<Option<&'a FocusHandle>>,
    label: impl Into<SharedString>,
    palette: UiPalette,
    window: &Window,
    variant: ButtonVariant,
    enabled: bool,
) -> Stateful<Div> {
    let focus_handle = focus_handle.into();
    let focused = focus_handle.is_some_and(|h| h.is_focused(window));

    let base = div()
        .id(id)
        .when_some(focus_handle, |d, handle| d.track_focus(handle))
        .flex()
        .items_center()
        .justify_center()
        .h(px(40.))
        .px_4()
        .rounded(px(RADIUS_CONTROL))
        .border_2()
        .when(focused, |d| d.border_dashed())
        .text_size(px(TEXT_LABEL))
        .font_weight(FontWeight::SEMIBOLD);

    let styled = match variant {
        ButtonVariant::Primary => base
            .border_color(rgb(if focused { palette.on_accent } else { palette.accent }))
            .bg(rgb(palette.accent))
            .text_color(rgb(palette.on_accent))
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.opacity(0.85))),
        ButtonVariant::Secondary => base
            .border_color(rgb(if focused { palette.focus } else { palette.line }))
            .bg(rgb(palette.surface))
            .text_color(rgb(palette.text))
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.opacity(0.85))),
        ButtonVariant::Ghost => base
            .border_color(if focused {
                rgb(palette.focus).into()
            } else {
                transparent_black()
            })
            .text_color(rgb(palette.text))
            .when(enabled, |d| {
                d.cursor_pointer()
                    .hover(move |s| s.bg(rgb(palette.surface)).opacity(0.9))
            }),
        ButtonVariant::Danger => {
            let on_danger = palette.on_danger();
            base.border_color(rgb(if focused { on_danger } else { palette.danger }))
                .bg(rgb(palette.danger))
                .text_color(rgb(on_danger))
                .when(enabled, |d| d.cursor_pointer().hover(|s| s.opacity(0.85)))
        }
    };

    styled.when(!enabled, |d| d.opacity(0.5)).child(label.into())
}

/// Compact toggleable action / filter chip (32px height, 10px radius, 2px border).
pub fn action_chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    palette: UiPalette,
    active: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(32.))
        .px_3()
        .rounded(px(RADIUS_CONTROL))
        .border_2()
        .border_color(rgb(if active { palette.accent } else { palette.line }))
        .bg(rgb(if active { palette.accent } else { palette.surface }))
        .text_color(rgb(if active { palette.on_accent } else { palette.text }))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(TEXT_SUPPORTING))
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|s| s.opacity(0.86))
        .child(label.into())
}

/// Standard 48px native window titlebar with a left-draggable title region
/// (`window.start_window_move()`) and a right-aligned close/action slot.
pub fn titlebar(title: impl Into<SharedString>, close_button: impl IntoElement, palette: UiPalette) -> Div {
    div()
        .h(px(TITLEBAR_HEIGHT))
        .flex_shrink_0()
        .px_4()
        .flex()
        .items_center()
        .justify_between()
        .bg(rgb(palette.surface))
        .border_b_1()
        .border_color(rgb(palette.line))
        .child(
            div()
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .text_size(px(TEXT_LABEL))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(palette.text))
                .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
                .child(title.into()),
        )
        .child(close_button)
}

/// Standard navigation sidebar (`DESIGN.md`: 176px width, surface fill, right 1px divider).
pub fn sidebar(width: f32, palette: UiPalette) -> Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .bg(rgb(palette.surface))
        .border_r_1()
        .border_color(rgb(palette.line))
        .p_4()
        .flex()
        .flex_col()
        .gap_2()
}

/// Standard surface card (`DESIGN.md`: 12px radius, surface fill, 1px hairline border, padding).
pub fn card(palette: UiPalette) -> Div {
    div()
        .p_4()
        .rounded(px(RADIUS_CARD))
        .bg(rgb(palette.surface))
        .border_1()
        .border_color(rgb(palette.line))
        .flex()
        .flex_col()
        .gap_3()
}

/// Section heading (`DESIGN.md`: 14px SemiBold title + optional 12px muted subtitle).
pub fn section_header(title: impl Into<SharedString>, subtitle: Option<&str>, palette: UiPalette) -> Div {
    let mut header = div().flex().flex_col().gap_1().child(
        div()
            .text_size(px(TEXT_SECTION))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(rgb(palette.text))
            .child(title.into()),
    );
    if let Some(sub) = subtitle {
        header = header.child(
            div()
                .text_size(px(TEXT_SUPPORTING))
                .text_color(rgb(palette.muted))
                .child(sub.to_string()),
        );
    }
    header
}

/// Compact status/category pill badge (`6px` radius, `11px` SemiBold).
pub fn badge(label: impl Into<SharedString>, palette: UiPalette, accent: bool) -> Div {
    div()
        .px_2()
        .py(px(2.))
        .rounded(px(RADIUS_PREVIEW))
        .bg(rgb(if accent { palette.accent } else { palette.canvas }))
        .border_1()
        .border_color(rgb(if accent { palette.accent } else { palette.line }))
        .text_size(px(TEXT_TECHNICAL))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(if accent { palette.on_accent } else { palette.muted }))
        .child(label.into())
}

/// Horizontal row of crisp circular color swatches for theme cards and color pickers.
pub fn swatch_strip(colors: &[u32], size_px: f32) -> Div {
    let size_px = size_px.max(4.0);
    let gap = (size_px * 0.3).clamp(3.0, 8.0);
    div()
        .flex()
        .items_center()
        .gap(px(gap))
        .children(colors.iter().copied().map(|color| {
            div()
                .size(px(size_px))
                .flex_shrink_0()
                .rounded_full()
                .bg(rgb(color & 0x00ff_ffff))
                .border_1()
                .border_color(rgba(0x80808055))
        }))
}

/// Full-width 1px hairline divider using the palette's line role.
pub fn divider(palette: UiPalette) -> Div {
    div().w_full().h(px(1.)).flex_shrink_0().bg(rgb(palette.line))
}

/// Stable bottom footer bar (`DESIGN.md`: `save-footer`, 24px × 16px padding, top divider,
/// left-aligned supporting status text). Append action buttons directly via `.child(...)`.
pub fn status_footer(status_text: impl Into<SharedString>, palette: UiPalette) -> Div {
    div()
        .flex_shrink_0()
        .px_6()
        .py_4()
        .border_t_1()
        .border_color(rgb(palette.line))
        .bg(rgb(palette.canvas))
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_size(px(TEXT_SUPPORTING))
                .text_color(rgb(palette.muted))
                .child(status_text.into()),
        )
}
