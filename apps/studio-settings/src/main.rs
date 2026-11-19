// SPDX-License-Identifier: MIT
mod session;
mod settings;
mod theme;

use gpui::{
    App, Application, Bounds, Context, Div, FocusHandle, FontWeight, KeyBinding, ObjectFit,
    Stateful, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, img, prelude::*,
    px, rgb, size,
};
use settings::{Preferences, Store, Theme, Wallpaper};
use std::{borrow::Cow, path::PathBuf};
use theme::Palette;

actions!(
    studio,
    [Next, Previous, Save, Undo, Reload, Quit, CancelClose]
);

const CONTROL_APPEARANCE: usize = 0;
const CONTROL_ABOUT: usize = 1;
const CONTROL_THEME_FIRST: usize = 2;
const CONTROL_WALLPAPER_FIRST: usize = 5;
const CONTROL_STUDIO: usize = 8;
const CONTROL_REDUCED_MOTION: usize = 9;
const CONTROL_UNDO: usize = 10;
const CONTROL_SAVE: usize = 11;
const CONTROL_RELOAD: usize = 12;
const CONTROL_CLOSE: usize = 13;
const CONTROL_KEEP_EDITING: usize = 14;
const CONTROL_DISCARD_CLOSE: usize = 15;
const CONTROL_COUNT: usize = 16;

fn wallpaper_path(wallpaper: Wallpaper) -> Option<PathBuf> {
    let installed = PathBuf::from("/usr/share/crowbaros-shell").join(wallpaper.asset_name());
    if installed.is_file() {
        return Some(installed);
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let shell_asset = manifest_dir
        .join("../shell/assets")
        .join(wallpaper.asset_name());
    if shell_asset.is_file() {
        return Some(shell_asset);
    }
    let brand_kit_name = match wallpaper {
        Wallpaper::SignalFeather => "signal-feather.png",
        Wallpaper::Nightwatch => "nightwatch.png",
        Wallpaper::QuietSignal => "quiet-signal-4k.png",
    };
    let brand_kit = manifest_dir
        .join("../../docs/branding/crowbaros-brand-kit/wallpapers")
        .join(brand_kit_name);
    if brand_kit.is_file() {
        return Some(brand_kit);
    }
    None
}

struct Settings {
    root: FocusHandle,
    controls: Vec<FocusHandle>,
    wallpaper_paths: [Option<PathBuf>; 3],
    store: Store,
    saved: Option<Preferences>,
    draft: Preferences,
    load_error: bool,
    error: Option<String>,
    busy: bool,
    about: bool,
    status: String,
    confirming_close: bool,
}

impl Settings {
    fn new(store: Store, window: &mut Window, cx: &mut Context<Self>) -> Self {
        window.set_window_title("Studio Settings");
        let controls: Vec<_> = (0..CONTROL_COUNT as isize)
            .map(|i| cx.focus_handle().tab_index(i).tab_stop(true))
            .collect();
        window.focus(&controls[CONTROL_THEME_FIRST]);
        let loaded = store.load();
        let error = loaded.as_ref().err().cloned();
        let saved = loaded.unwrap_or_default();
        let draft = saved.clone().unwrap_or_default();
        log::info!(
            "STUDIO event=opened theme={:?} studio={} reduced_motion={} wallpaper={:?}",
            draft.theme,
            draft.studio,
            draft.reduced_motion,
            draft.wallpaper
        );
        Self {
            root: cx.focus_handle(),
            controls,
            wallpaper_paths: Wallpaper::ALL.map(wallpaper_path),
            store,
            saved,
            draft,
            load_error: error.is_some(),
            error,
            busy: false,
            about: false,
            status: "".into(),
            confirming_close: false,
        }
    }
    fn dirty(&self) -> bool {
        self.saved.as_ref() != Some(&self.draft)
    }
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.status.clear();
        self.confirming_close = false;
        if !self.load_error {
            self.error = None;
        }
        log::info!(
            "STUDIO event=preview theme={:?} studio={} reduced_motion={} wallpaper={:?}",
            self.draft.theme,
            self.draft.studio,
            self.draft.reduced_motion,
            self.draft.wallpaper
        );
        cx.notify();
    }
    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.load_error || !self.dirty() {
            return;
        }
        self.busy = true;
        self.error = None;
        self.status = "Saving…".into();
        cx.notify();
        let store = self.store.clone();
        let expected = self.saved.clone();
        let draft = self.draft.clone();
        let task = cx.background_executor().spawn(async move {
            let result = session::Session::connect()
                .and_then(|motion| store.save(&draft, &expected, &motion));
            (draft, result)
        });
        cx.spawn(async move |this, cx| {
            let (draft, result) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => {
                        log::info!(
                            "STUDIO event=saved theme={:?} studio={} reduced_motion={} wallpaper={:?}",
                            draft.theme,
                            draft.studio,
                            draft.reduced_motion,
                            draft.wallpaper
                        );
                        this.saved = Some(draft);
                        this.status = "Saved.".into();
                        this.confirming_close = false;
                    }
                    Err(error) => {
                        log::warn!("STUDIO event=save-failed {error}");
                        this.error = Some(error);
                        this.status = "Changes not saved.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.draft = self.saved.clone().unwrap_or_default();
        self.status = "Preview reset.".into();
        self.confirming_close = false;
        if !self.load_error {
            self.error = None;
        }
        log::info!("STUDIO event=undo");
        cx.notify();
    }
    fn reload(&mut self, _: &Reload, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        match self.store.load() {
            Ok(saved) => {
                self.saved = saved;
                self.draft = self.saved.clone().unwrap_or_default();
                self.error = None;
                self.load_error = false;
                self.status = "Reloaded saved preferences.".into();
                self.confirming_close = false;
            }
            Err(e) => {
                self.error = Some(e);
                self.load_error = true;
            }
        }
        cx.notify();
    }
    fn quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.dirty() && self.draft != self.saved.clone().unwrap_or_default() {
            self.confirming_close = true;
            window.focus(&self.controls[CONTROL_KEEP_EDITING]);
            cx.notify();
        } else {
            cx.quit();
        }
    }
    fn button(
        &self,
        index: usize,
        label: impl Into<gpui::SharedString>,
        p: Palette,
        window: &Window,
        primary: bool,
        enabled: bool,
    ) -> Stateful<Div> {
        div()
            .id(("button", index))
            .track_focus(&self.controls[index])
            .flex()
            .items_center()
            .justify_center()
            .h(px(40.))
            .px_4()
            .rounded(px(10.))
            .border_2()
            .border_color(rgb(if self.controls[index].is_focused(window) {
                if primary { p.on_accent } else { p.focus }
            } else if primary {
                p.accent
            } else {
                p.line
            }))
            .when(self.controls[index].is_focused(window), |d| {
                d.border_dashed()
            })
            .bg(rgb(if primary { p.accent } else { p.surface }))
            .text_color(rgb(if primary { p.on_accent } else { p.text }))
            .text_size(px(13.))
            .font_weight(FontWeight::SEMIBOLD)
            .when(enabled, |d| d.cursor_pointer().hover(|s| s.opacity(0.85)))
            .when(!enabled, |d| d.opacity(0.5))
            .child(label.into())
    }
    fn theme_choice(
        &self,
        index: usize,
        theme: Theme,
        p: Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.draft.theme == theme;
        let preview = Palette::for_preferences(&Preferences {
            theme,
            studio: self.draft.studio,
            ..Preferences::default()
        });
        let control = CONTROL_THEME_FIRST + index;
        let focus = self.controls[control].is_focused(window);
        div()
            .id(("theme", index))
            .track_focus(&self.controls[control])
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .rounded(px(12.))
            .border_2()
            .border_color(rgb(if focus || selected { p.focus } else { p.line }))
            .when(focus, |d| d.border_dashed())
            .bg(rgb(p.surface))
            .cursor_pointer()
            .hover(|s| s.opacity(0.86))
            .child(
                div()
                    .h(px(72.))
                    .rounded(px(6.))
                    .bg(rgb(preview.canvas))
                    .p_3()
                    .flex()
                    .gap_2()
                    .child(div().w(px(18.)).rounded(px(3.)).bg(rgb(preview.surface)))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .gap_2()
                            .child(
                                div()
                                    .h(px(5.))
                                    .w(px(42.))
                                    .rounded(px(2.))
                                    .bg(rgb(preview.text)),
                            )
                            .child(
                                div()
                                    .h(px(4.))
                                    .w_full()
                                    .rounded(px(2.))
                                    .bg(rgb(preview.line)),
                            )
                            .child(
                                div()
                                    .h(px(12.))
                                    .w(px(32.))
                                    .rounded(px(3.))
                                    .bg(rgb(preview.accent)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_1()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(theme.label())
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(p.muted))
                            .child(if selected { "Selected" } else { "" }),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.busy {
                    return;
                }
                window.focus(&this.controls[control]);
                this.draft.theme = theme;
                this.changed(cx);
            }))
    }
    fn wallpaper_choice(
        &self,
        index: usize,
        wallpaper: Wallpaper,
        p: Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.draft.wallpaper == wallpaper;
        let control = CONTROL_WALLPAPER_FIRST + index;
        let focus = self.controls[control].is_focused(window);
        let preview = if self.draft.studio {
            div()
                .h(px(72.))
                .rounded(px(6.))
                .bg(rgb(p.canvas))
                .p_2()
                .flex()
                .flex_col()
                .justify_between()
                .items_center()
                .child(div().w_full().h(px(5.)).rounded(px(2.)).bg(rgb(p.surface)))
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(p.muted))
                        .child("Neutral canvas"),
                )
                .child(div().w(px(28.)).h(px(4.)).rounded_full().bg(rgb(p.line)))
        } else {
            let mut box_div = div()
                .h(px(72.))
                .rounded(px(6.))
                .overflow_hidden()
                .bg(rgb(0x080d12));
            if let Some(path) = self.wallpaper_paths[index].clone() {
                box_div = box_div.child(
                    img(path)
                        .size_full()
                        .rounded(px(6.))
                        .object_fit(ObjectFit::Cover),
                );
            }
            box_div
        };
        div()
            .id(("wallpaper", index))
            .track_focus(&self.controls[control])
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap_3()
            .p_3()
            .rounded(px(12.))
            .border_2()
            .border_color(rgb(if focus || selected { p.focus } else { p.line }))
            .when(focus, |d| d.border_dashed())
            .bg(rgb(p.surface))
            .cursor_pointer()
            .hover(|s| s.opacity(0.86))
            .child(preview)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_1()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(wallpaper.label())
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(rgb(p.muted))
                            .child(if selected { "Selected" } else { "" }),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.busy {
                    return;
                }
                window.focus(&this.controls[control]);
                this.draft.wallpaper = wallpaper;
                this.changed(cx);
            }))
    }
    fn toggle(
        &self,
        index: usize,
        title: &'static str,
        description: &'static str,
        value: bool,
        p: Palette,
        window: &Window,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(("toggle", index))
            .track_focus(&self.controls[index])
            .flex()
            .items_center()
            .gap_5()
            .py_4()
            .px_3()
            .rounded(px(10.))
            .border_2()
            .border_color(rgb(if self.controls[index].is_focused(window) {
                p.focus
            } else {
                p.canvas
            }))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(p.surface)))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(p.muted))
                            .child(description),
                    ),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(p.muted))
                    .child(if value { "On" } else { "Off" }),
            )
            .child(
                div()
                    .w(px(44.))
                    .h(px(26.))
                    .px(px(3.))
                    .flex()
                    .items_center()
                    .rounded_full()
                    .bg(rgb(if value { p.accent } else { p.line }))
                    .when(value, |d| d.justify_end())
                    .child(div().size(px(20.)).rounded_full().bg(rgb(if value {
                        p.on_accent
                    } else {
                        p.text
                    }))),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.busy {
                    return;
                }
                window.focus(&this.controls[index]);
                if index == CONTROL_STUDIO {
                    this.draft.studio = !this.draft.studio;
                } else {
                    this.draft.reduced_motion = !this.draft.reduced_motion;
                }
                this.changed(cx);
            }))
    }
    fn appearance(&self, p: Palette, window: &Window, cx: &Context<Self>) -> Div {
        div().flex().flex_col().gap_5()
            .child(div().flex().flex_col().gap_2()
                .child(div().text_size(px(28.)).font_weight(FontWeight::BOLD).child("Make it yours."))
                .child(div().text_size(px(13.)).text_color(rgb(p.muted)).child("A comfortable surface. A calmer workspace.")))
            .child(div().flex().flex_col().gap_3()
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child("Appearance"))
                .child(div().flex().gap_3().children(Theme::ALL.into_iter().enumerate().map(|(i,t)|self.theme_choice(i,t,p,window,cx))))
                .child(div().text_size(px(12.)).text_color(rgb(p.muted)).child(if self.draft.studio{
                    "Studio Mode is showing neutral grays. Your selected mode is kept for later."
                }else{"Previewed here immediately. Save to remember your choice."})))
            .child(div().flex().flex_col().gap_3()
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).child("Wallpaper"))
                .child(div().flex().gap_3().children(Wallpaper::ALL.into_iter().enumerate().map(|(i,w)|self.wallpaper_choice(i,w,p,window,cx))))
                .when(self.draft.studio, |d| {
                    d.child(div().text_size(px(12.)).text_color(rgb(p.muted)).child(
                        "Studio Mode uses a neutral canvas. Your selected wallpaper is kept for later.",
                    ))
                }))
            .child(div().flex().flex_col().border_t_1().border_b_1().border_color(rgb(p.line))
                .child(self.toggle(CONTROL_STUDIO,"Studio Mode","Use neutral grays for visual work. This does not calibrate your display.",self.draft.studio,p,window,cx))
                .child(div().h(px(1.)).bg(rgb(p.line)))
                .child(self.toggle(CONTROL_REDUCED_MOTION,if session::is_alpha() { "Reduced motion preference" } else { "Reduced motion" },if session::is_alpha() { "Saved for compatible desktops. This desktop has no animations." } else { "Turn off desktop window and workspace animations when you save." },self.draft.reduced_motion,p,window,cx)))
            .child(div().text_size(px(12.)).text_color(rgb(p.muted))
                .child(if session::is_alpha() { "Saved appearance and wallpaper also apply to the shell. Other applications keep their own themes." } else { "Appearance currently styles Studio Settings. Reduced motion applies to your Hyprland desktop and is restored at login." }))
    }
    fn about(&self, p: Palette) -> Div {
        div().flex().flex_col().gap_5()
            .child(div().text_size(px(28.)).font_weight(FontWeight::BOLD).child("Studio Settings"))
            .child(div().text_size(px(14.)).child("Your workspace, with room to make it your own."))
            .child(div().flex().flex_col().gap_3().text_size(px(13.)).text_color(rgb(p.muted))
                .child("Preview your appearance, then save it for your next session. Available motion controls depend on your desktop." )
                .child("Saved appearance and wallpaper apply to the shell. Display controls, desktop-wide styling, and other settings will follow.")
                .child("Keyboard: Tab and Shift+Tab move focus. Enter or Space activates a control. Ctrl+S saves; Ctrl+Z undoes the preview; Ctrl+Q closes."))
            .child(div().border_t_1().border_color(rgb(p.line)).pt_5().flex().flex_col().gap_2()
                .child(div().text_size(px(13.)).child("CrowbarOS · Studio Settings 0.1.3"))
                .child(div().font_family("IBM Plex Mono").text_size(px(11.)).text_color(rgb(p.muted)).child("Rust / GPUI 0.2.2 / Wayland")))
    }
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Palette::for_preferences(&self.draft);
        let can_save = self.dirty() && !self.load_error && !self.busy;
        let status = if !self.status.is_empty() {
            self.status.clone()
        } else if self.dirty() {
            "Preview · not saved".into()
        } else {
            "All changes saved".into()
        };
        div()
            .id("studio-settings")
            .key_context("StudioSettings")
            .track_focus(&self.root)
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::reload))
            .on_action(cx.listener(Self::quit))
            .on_action(cx.listener(|this, _: &Next, w, cx| {
                w.focus_next();
                cx.notify();
                log::info!("STUDIO event=focus-next");
                this.status.clear();
            }))
            .on_action(cx.listener(|_, _: &Previous, w, cx| {
                w.focus_prev();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &CancelClose, window, cx| {
                this.confirming_close = false;
                window.focus(&this.controls[CONTROL_SAVE]);
                cx.notify();
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(p.canvas))
            .text_color(rgb(p.text))
            .font_family("Manrope")
            .child(
                div()
                    .h(px(48.))
                    .flex_shrink_0()
                    .px_4()
                    .flex()
                    .items_center()
                    .justify_between()
                    .bg(rgb(p.surface))
                    .border_b_1()
                    .border_color(rgb(p.line))
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .flex()
                            .items_center()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .on_mouse_down(gpui::MouseButton::Left, |_, window, _| {
                                window.start_window_move()
                            })
                            .child("Studio Settings"),
                    )
                    .child(
                        self.button(CONTROL_CLOSE, "Close", p, window, false, !self.busy)
                            .h(px(30.))
                            .on_click(cx.listener(|this, _, w, cx| this.quit(&Quit, w, cx))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        div()
                            .w(px(176.))
                            .flex_shrink_0()
                            .bg(rgb(p.surface))
                            .border_r_1()
                            .border_color(rgb(p.line))
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                self.button(
                                    CONTROL_APPEARANCE,
                                    "Appearance",
                                    p,
                                    window,
                                    !self.about,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.about = false;
                                    w.focus(&this.controls[CONTROL_APPEARANCE]);
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.button(CONTROL_ABOUT, "About", p, window, self.about, true)
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        this.about = true;
                                        w.focus(&this.controls[CONTROL_ABOUT]);
                                        cx.notify();
                                    })),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(p.muted))
                                    .child("CrowbarOS"),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .id("settings-content")
                                    .flex_1()
                                    .min_h(px(0.))
                                    .overflow_y_scroll()
                                    .p_6()
                                    .child(if self.about {
                                        self.about(p)
                                    } else {
                                        self.appearance(p, window, cx)
                                    }),
                            )
                            .when(self.error.is_some(), |d| {
                                d.child(
                                    div()
                                        .px_6()
                                        .py_3()
                                        .border_t_1()
                                        .border_color(rgb(p.line))
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .flex_shrink_0()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w(px(0.))
                                                .text_size(px(12.))
                                                .child(self.error.clone().unwrap_or_default()),
                                        )
                                        .child(
                                            self.button(
                                                CONTROL_RELOAD,
                                                "Reload saved",
                                                p,
                                                window,
                                                false,
                                                !self.busy,
                                            )
                                            .flex_shrink_0()
                                            .on_click(
                                                cx.listener(|this, _, w, cx| {
                                                    this.reload(&Reload, w, cx)
                                                }),
                                            ),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .px_6()
                                    .py_4()
                                    .border_t_1()
                                    .border_color(rgb(p.line))
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_size(px(12.))
                                            .text_color(rgb(p.muted))
                                            .child(if self.confirming_close {
                                                "Discard unsaved changes?".into()
                                            } else {
                                                status
                                            }),
                                    )
                                    .when(self.confirming_close, |d| {
                                        d.child(
                                            self.button(
                                                CONTROL_KEEP_EDITING,
                                                "Keep editing",
                                                p,
                                                window,
                                                false,
                                                true,
                                            )
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.confirming_close = false;
                                                window.focus(&this.controls[CONTROL_SAVE]);
                                                cx.notify();
                                            })),
                                        )
                                        .child(
                                            self.button(
                                                CONTROL_DISCARD_CLOSE,
                                                "Discard & close",
                                                p,
                                                window,
                                                true,
                                                true,
                                            )
                                            .on_click(|_, _, cx| cx.quit()),
                                        )
                                    })
                                    .when(!self.confirming_close, |d| {
                                        d.child(
                                            self.button(
                                                CONTROL_UNDO,
                                                "Undo",
                                                p,
                                                window,
                                                false,
                                                self.dirty() && !self.busy,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, w, cx| {
                                                    this.undo(&Undo, w, cx)
                                                }),
                                            ),
                                        )
                                        .child(
                                            self.button(
                                                CONTROL_SAVE,
                                                if self.busy {
                                                    "Saving…"
                                                } else {
                                                    "Save changes"
                                                },
                                                p,
                                                window,
                                                true,
                                                can_save,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, w, cx| {
                                                    this.save(&Save, w, cx)
                                                }),
                                            ),
                                        )
                                    }),
                            ),
                    ),
            )
    }
}

fn cli() -> Result<bool, String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return Ok(false);
    }
    match args.as_slice() {
        [arg] if arg == "--version" => println!(
            "crowbaros-studio-settings {} (GPUI 0.2.2; Wayland)",
            env!("CARGO_PKG_VERSION")
        ),
        [arg] if arg == "--print-settings" => println!(
            "{}",
            serde_json::to_string_pretty(&Store::for_user()?.load()?.unwrap_or_default())
                .map_err(|e| e.to_string())?
        ),
        [arg] if arg == "--apply-session" => {
            if let Some(p) = Store::for_user()?.load()? {
                session::restore_motion(p.reduced_motion)?;
            }
        }
        _ => {
            return Err(
                "Usage: crowbaros-studio-settings [--version|--print-settings|--apply-session]"
                    .into(),
            );
        }
    }
    Ok(true)
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    match cli() {
        Ok(true) => return,
        Err(e) => {
            eprintln!("Studio Settings: {e}");
            std::process::exit(1);
        }
        _ => (),
    }
    let store = Store::for_user().unwrap_or_else(|e| {
        eprintln!("Studio Settings: {e}");
        std::process::exit(1)
    });
    Application::new().run(move |cx: &mut App| {
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../fonts/Manrope-Regular.ttf")),
                Cow::Borrowed(include_bytes!("../fonts/Manrope-SemiBold.ttf")),
                Cow::Borrowed(include_bytes!("../fonts/Manrope-Bold.ttf")),
                Cow::Borrowed(include_bytes!("../fonts/IBMPlexMono-Regular.ttf")),
            ])
            .expect("bundled fonts must load");
        cx.bind_keys([
            KeyBinding::new("tab", Next, Some("StudioSettings")),
            KeyBinding::new("shift-tab", Previous, Some("StudioSettings")),
            KeyBinding::new("ctrl-s", Save, Some("StudioSettings")),
            KeyBinding::new("ctrl-z", Undo, Some("StudioSettings")),
            KeyBinding::new("ctrl-q", Quit, Some("StudioSettings")),
            KeyBinding::new("escape", CancelClose, Some("StudioSettings")),
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(980.), px(720.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(800.), px(640.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Studio Settings".into()),
                    ..Default::default()
                }),
                app_id: Some("com.crowbaros.StudioSettings".into()),
                ..Default::default()
            },
            |w, cx| {
                let entity = cx.new(|cx| Settings::new(store, w, cx));
                let weak = entity.downgrade();
                w.on_window_should_close(cx, move |window, cx| {
                    weak.update(cx, |this, cx| {
                        if this.busy {
                            return false;
                        }
                        if this.dirty() && this.draft != this.saved.clone().unwrap_or_default() {
                            this.confirming_close = true;
                            window.focus(&this.controls[CONTROL_KEEP_EDITING]);
                            cx.notify();
                            false
                        } else {
                            true
                        }
                    })
                    .unwrap_or(true)
                });
                entity
            },
        )
        .expect("Wayland window must open");
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_tab_order_preserves_theme_indices_and_shifts_remaining() {
        assert_eq!(CONTROL_APPEARANCE, 0);
        assert_eq!(CONTROL_ABOUT, 1);
        assert_eq!(CONTROL_THEME_FIRST, 2);
        assert_eq!(CONTROL_THEME_FIRST + Theme::ALL.len(), CONTROL_WALLPAPER_FIRST);
        assert_eq!(CONTROL_WALLPAPER_FIRST, 5);
        assert_eq!(
            CONTROL_WALLPAPER_FIRST + Wallpaper::ALL.len(),
            CONTROL_STUDIO
        );
        assert_eq!(CONTROL_STUDIO, 8);
        assert_eq!(CONTROL_REDUCED_MOTION, 9);
        assert_eq!(CONTROL_UNDO, 10);
        assert_eq!(CONTROL_SAVE, 11);
        assert_eq!(CONTROL_RELOAD, 12);
        assert_eq!(CONTROL_CLOSE, 13);
        assert_eq!(CONTROL_KEEP_EDITING, 14);
        assert_eq!(CONTROL_DISCARD_CLOSE, 15);
        assert_eq!(CONTROL_COUNT, 16);
    }

    #[test]
    fn wallpaper_paths_resolve_in_checkout() {
        for wallpaper in Wallpaper::ALL {
            let path = wallpaper_path(wallpaper)
                .unwrap_or_else(|| panic!("expected preview asset for {wallpaper:?}"));
            assert!(path.is_file(), "resolved path must exist: {}", path.display());
        }
    }
}
