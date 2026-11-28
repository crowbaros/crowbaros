// SPDX-License-Identifier: MIT
//! Quick Theme Switcher popover: typed LauncherClient actions coordinated
//! through `ActivePopover` with explicit imports (no `use super::*`).

use gpui::{layer_shell::*, prelude::*, *};

use crate::launcher_client::{LauncherClient, THEME_STUDIO_DESKTOP};
use crate::ui::{
    ActivePopover, BAR_HEIGHT, Close, Model, clear_active_popover_if, close_active_popover, layer,
};

#[derive(Clone)]
enum SwitcherAction {
    ApplyTheme { id: String, background: Option<String> },
    CycleBackground,
    CreateTheme,
}

impl SwitcherAction {
    fn execute(self, launcher: &LauncherClient) -> Result<(), String> {
        match self {
            SwitcherAction::ApplyTheme { id, background } => launcher
                .apply_theme(&id, background.as_deref())
                .map_err(|e| e.to_string()),
            SwitcherAction::CycleBackground => {
                launcher.cycle_background().map_err(|e| e.to_string())
            }
            SwitcherAction::CreateTheme => launcher
                .launch_desktop_sync(THEME_STUDIO_DESKTOP)
                .map_err(|e| e.to_string()),
        }
    }

    fn closes_on_success(&self) -> bool {
        matches!(self, SwitcherAction::CreateTheme)
    }
}

pub struct ThemeSwitcher {
    model: Entity<Model>,
    focus: FocusHandle,
    busy: bool,
    error: Option<String>,
    controls: Vec<FocusHandle>,
}

impl ThemeSwitcher {
    pub fn close(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        clear_active_popover_if(
            &self.model,
            |p| matches!(p, ActivePopover::ThemeSwitcher(_)),
            cx,
        );
        w.remove_window();
        cx.refresh_windows();
    }

    fn run_action(&mut self, action: SwitcherAction, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let closes = action.closes_on_success();
        let launcher = self.model.read(cx).launcher_client.clone();
        let model = self.model.clone();
        let task = cx
            .background_executor()
            .spawn(async move { action.execute(&launcher) });
        cx.spawn(async move |view, cx| {
            let result = task.await;
            let launched = closes && result.is_ok();
            let _ = view.update(cx, |s, cx| {
                s.busy = false;
                s.error = result.err();
                cx.notify();
            });
            if launched {
                let _ = cx.update(|cx| {
                    if model.read(cx).is_theme_switcher_open() {
                        close_active_popover(&model, cx);
                    }
                });
            }
        })
        .detach();
    }

    fn button(
        &self,
        id: usize,
        label: String,
        action: SwitcherAction,
        selected: bool,
        w: &Window,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let p = self.model.read(cx).palette();
        let key_action = action.clone();
        let focus = &self.controls[id];
        div()
            .id(("theme", id))
            .track_focus(focus)
            .px(px(10.))
            .py(px(8.))
            .rounded(px(6.))
            .border_1()
            .border_color(rgb(if focus.is_focused(w) || selected {
                p.focus
            } else {
                p.line
            }))
            .when(focus.is_focused(w), |d| d.border_dashed())
            .bg(rgb(if selected { p.surface } else { p.canvas }))
            .text_color(rgb(p.text))
            .cursor_pointer()
            .hover(move |s| s.bg(rgb(p.surface)))
            .child(label)
            .when(!self.busy, |d| {
                d.on_click(cx.listener(move |s, _, _, cx| s.run_action(action.clone(), cx)))
                    .on_key_down(cx.listener(move |s, e: &KeyDownEvent, _, cx| {
                        if matches!(e.keystroke.key.as_str(), "enter" | "space") {
                            s.run_action(key_action.clone(), cx);
                            cx.stop_propagation();
                        }
                    }))
            })
            .when(self.busy, |d| d.opacity(0.5))
    }
}

impl Render for ThemeSwitcher {
    fn render(&mut self, w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let m = self.model.read(cx);
        let p = m.palette();
        let catalog = m.themes.clone();
        let mut panel = div()
            .id("theme-panel")
            .w(px(380.))
            .max_h(px(760.))
            .p(px(18.))
            .rounded(px(12.))
            .bg(rgb(p.canvas))
            .border_1()
            .border_color(rgb(p.line))
            .flex()
            .flex_col()
            .gap(px(12.))
            .text_color(rgb(p.text))
            .text_size(px(13.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .text_size(px(20.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Themes"),
            );
        if let Some(catalog) = catalog {
            let mut list = div()
                .id("theme-list")
                .max_h(px(380.))
                .overflow_y_scroll()
                .flex()
                .flex_col()
                .gap(px(6.));
            for (i, theme) in catalog.themes.iter().take(128).enumerate() {
                list = list.child(
                    self.button(
                        i,
                        theme.name.clone(),
                        SwitcherAction::ApplyTheme {
                            id: theme.id.clone(),
                            background: None,
                        },
                        theme.id == catalog.active.theme_id,
                        w,
                        cx,
                    )
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(8.))
                    .child(
                        div().flex().gap(px(3.)).children(
                            [
                                theme.colors.canvas,
                                theme.colors.surface,
                                theme.colors.text,
                                theme.colors.accent,
                            ]
                            .into_iter()
                            .map(|color| div().size(px(13.)).rounded(px(3.)).bg(rgb(color))),
                        ),
                    ),
                );
            }
            panel = panel.child(list);
            if let Some(active) = catalog
                .themes
                .iter()
                .find(|t| t.id == catalog.active.theme_id)
            {
                let mut backgrounds = div()
                    .id("theme-backgrounds")
                    .max_h(px(130.))
                    .overflow_y_scroll()
                    .flex()
                    .flex_wrap()
                    .gap(px(6.));
                for (i, bg) in active.backgrounds.iter().take(128).enumerate() {
                    backgrounds = backgrounds.child(self.button(
                        128 + i,
                        bg.clone(),
                        SwitcherAction::ApplyTheme {
                            id: active.id.clone(),
                            background: Some(bg.clone()),
                        },
                        catalog.active.background.as_ref() == Some(bg),
                        w,
                        cx,
                    ));
                }
                panel = panel
                    .child(div().text_color(rgb(p.muted)).child("Backgrounds"))
                    .child(backgrounds);
                if !active.backgrounds.is_empty() {
                    panel = panel.child(self.button(
                        256,
                        "Next background".into(),
                        SwitcherAction::CycleBackground,
                        false,
                        w,
                        cx,
                    ));
                }
            }
        } else {
            panel = panel.child("Loading installed themes…");
        }
        panel = panel.child(self.button(
            257,
            "Create theme…".into(),
            SwitcherAction::CreateTheme,
            false,
            w,
            cx,
        ));
        if let Some(error) = &self.error {
            panel = panel.child(div().text_color(rgb(p.muted)).child(error.clone()));
        }
        div()
            .id("theme-backdrop")
            .size_full()
            .font_family("Manrope")
            .key_context("ThemeSwitcher")
            .track_focus(&self.focus)
            .pt(px(BAR_HEIGHT + 8.))
            .pr(px(12.))
            .flex()
            .justify_end()
            .items_start()
            .on_action(cx.listener(|s, _: &Close, w, cx| s.close(w, cx)))
            .on_key_down(|e, w, cx| {
                if e.keystroke.key == "tab" {
                    if e.keystroke.modifiers.shift {
                        w.focus_prev();
                    } else {
                        w.focus_next();
                    }
                    cx.stop_propagation();
                }
            })
            .on_mouse_down(MouseButton::Left, cx.listener(|s, _, w, cx| s.close(w, cx)))
            .child(panel)
    }
}

pub fn toggle(model: Entity<Model>, cx: &mut App) {
    if model.read(cx).is_theme_switcher_open() {
        close_active_popover(&model, cx);
        return;
    }
    close_active_popover(&model, cx);
    let shared = model.clone();
    match cx.open_window(
        layer(
            "theme-switcher",
            Layer::Overlay,
            Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
            0.,
            0.,
            Some(-1.),
            KeyboardInteractivity::Exclusive,
        ),
        move |w, cx| {
            let focus = cx.focus_handle();
            w.focus(&focus);
            cx.new(|cx| ThemeSwitcher {
                model: shared,
                focus,
                busy: false,
                error: None,
                controls: (0..258).map(|_| cx.focus_handle().tab_stop(true)).collect(),
            })
        },
    ) {
        Ok(w) => {
            model.update(cx, |m, _| {
                m.active_popover = Some(ActivePopover::ThemeSwitcher(w));
            });
            log::info!("SHELL theme-switcher open=true");
        }
        Err(e) => log::error!("Opening theme switcher: {e}"),
    }
}
