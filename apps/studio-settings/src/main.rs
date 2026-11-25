// SPDX-License-Identifier: MIT
mod displays;
mod session;
mod settings;
mod theme;

use crowbar_theme::{ActiveTheme, ThemeStore};
use crowbar_ui::{
    ButtonVariant, FONT_MONO, FONT_UI, SIDEBAR_WIDTH, TEXT_BODY, TEXT_LABEL, TEXT_SECTION,
    TEXT_SELECTION_LABEL, TEXT_SUPPORTING, TEXT_TECHNICAL, TEXT_TITLE, action_chip, badge,
    button as ui_button, card, divider, load_fonts, section_header, sidebar, spawn_theme_watcher,
    status_footer, titlebar,
};
use displays::{
    DisplayError, DisplayMode, OutputDraft, OutputInfo, RelativeSide, SCALES, arrange_relative,
    dedup_sorted_modes, format_refresh, format_scale, logical_size, project_diagram,
};
use gpui::{
    App, Application, Bounds, Context, Div, FocusHandle, FontWeight, KeyBinding, ObjectFit,
    Stateful, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, img, prelude::*,
    px, rgb, size,
};
use settings::{Preferences, Store, Theme, Wallpaper};
use std::path::PathBuf;
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
const CONTROL_DISPLAYS: usize = 16;
const CONTROL_DISPLAY_REFRESH: usize = 17;
const CONTROL_COUNT: usize = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Appearance,
    Displays,
    About,
}

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
    live_theme: ActiveTheme,
    previewing_appearance: bool,
    load_error: bool,
    error: Option<String>,
    busy: bool,
    page: Page,
    status: String,
    confirming_close: bool,
    outputs: Vec<OutputInfo>,
    display_drafts: Vec<OutputDraft>,
    selected_display: usize,
    displays_loaded: bool,
    displays_loading: bool,
    displays_unavailable: Option<String>,
    display_error: Option<String>,
    display_status: String,
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
        let live_theme = ThemeStore::for_user()
            .map(|theme_store| theme_store.load_active())
            .unwrap_or_else(|_| theme::fallback_active_theme(&draft));
        spawn_theme_watcher(cx, |this, active, cx| {
            this.live_theme = active;
            if !this.previewing_appearance
                && !this.has_unsaved_edits()
                && !this.busy
                && let Ok(reloaded) = this.store.load()
            {
                this.saved = reloaded;
                this.draft = this.saved.clone().unwrap_or_default();
                this.load_error = false;
                this.error = None;
            }
            cx.notify();
        });
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
            live_theme,
            previewing_appearance: false,
            load_error: error.is_some(),
            error,
            busy: false,
            page: Page::Appearance,
            status: "".into(),
            confirming_close: false,
            outputs: Vec::new(),
            display_drafts: Vec::new(),
            selected_display: 0,
            displays_loaded: false,
            displays_loading: false,
            displays_unavailable: None,
            display_error: None,
            display_status: "".into(),
        }
    }
    fn active_palette(&self) -> Palette {
        theme::resolve_palette(&self.live_theme, &self.draft, self.previewing_appearance)
    }
    fn appearance_dirty(&self) -> bool {
        self.saved.as_ref() != Some(&self.draft)
    }
    fn displays_dirty(&self) -> bool {
        self.outputs.len() == self.display_drafts.len()
            && self
                .outputs
                .iter()
                .zip(&self.display_drafts)
                .any(|(live, draft)| !draft.matches_output(live))
    }
    fn has_unsaved_edits(&self) -> bool {
        (self.appearance_dirty() && self.draft != self.saved.clone().unwrap_or_default())
            || self.displays_dirty()
    }
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.previewing_appearance = true;
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
    fn display_changed(&mut self, cx: &mut Context<Self>) {
        self.display_status.clear();
        self.display_error = None;
        self.confirming_close = false;
        cx.notify();
    }
    fn refresh_displays(&mut self, cx: &mut Context<Self>) {
        if self.displays_loading || self.busy {
            return;
        }
        self.displays_loading = true;
        self.display_error = None;
        cx.notify();
        let previous_name = self
            .outputs
            .get(self.selected_display)
            .map(|o| o.name.clone());
        let task = cx.background_executor().spawn(async move { displays::fetch_outputs() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.displays_loading = false;
                this.displays_loaded = true;
                match result {
                    Ok(outputs) => {
                        let next_selected = previous_name
                            .as_deref()
                            .and_then(|name| outputs.iter().position(|o| o.name == name))
                            .unwrap_or(0);
                        this.display_drafts = outputs.iter().map(OutputDraft::from_output).collect();
                        this.outputs = outputs;
                        this.selected_display = next_selected;
                        this.displays_unavailable = None;
                        this.display_error = None;
                        log::info!(
                            "STUDIO event=displays-loaded count={}",
                            this.outputs.len()
                        );
                    }
                    Err(DisplayError::Unavailable(message)) => {
                        this.outputs.clear();
                        this.display_drafts.clear();
                        this.selected_display = 0;
                        this.displays_unavailable = Some(message);
                        this.display_error = None;
                    }
                    Err(DisplayError::Failed(message)) => {
                        log::warn!("STUDIO event=displays-load-failed {message}");
                        this.displays_unavailable = None;
                        this.display_error = Some(message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn apply_displays(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.displays_loading || !self.displays_dirty() {
            return;
        }
        self.busy = true;
        self.display_error = None;
        self.display_status = "Applying…".into();
        cx.notify();
        let live = self.outputs.clone();
        let drafts = self.display_drafts.clone();
        let previous_name = self
            .outputs
            .get(self.selected_display)
            .map(|o| o.name.clone());
        let task = cx.background_executor().spawn(async move {
            let applied = displays::apply_changes(&live, &drafts)?;
            let refreshed = displays::fetch_outputs()?;
            Ok::<_, DisplayError>((applied, refreshed))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok((applied, refreshed)) => {
                        let next_selected = previous_name
                            .as_deref()
                            .and_then(|name| refreshed.iter().position(|o| o.name == name))
                            .unwrap_or(0);
                        this.display_drafts =
                            refreshed.iter().map(OutputDraft::from_output).collect();
                        this.outputs = refreshed;
                        this.selected_display = next_selected;
                        this.displays_unavailable = None;
                        this.display_error = None;
                        this.display_status = "Display settings applied.".into();
                        this.confirming_close = false;
                        log::info!("STUDIO event=display-applied outputs={}", applied.join(","));
                    }
                    Err(error) => {
                        log::warn!("STUDIO event=display-apply-failed {}", error.message());
                        this.display_error = Some(error.message().to_string());
                        this.display_status = "Display changes not applied.".into();
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
        if self.page == Page::Displays {
            self.apply_displays(cx);
            return;
        }
        if self.busy || self.load_error || !self.appearance_dirty() {
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
            let synced_theme = if result.is_ok() {
                match ThemeStore::for_user()
                    .and_then(|theme_store| theme::sync_theme_store(&theme_store, &draft))
                {
                    Ok(active) => Some(active),
                    Err(err) => {
                        log::warn!("STUDIO event=theme-sync-failed {err}");
                        None
                    }
                }
            } else {
                None
            };
            (draft, result, synced_theme)
        });
        cx.spawn(async move |this, cx| {
            let (draft, result, synced_theme) = task.await;
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
                        this.live_theme = synced_theme
                            .unwrap_or_else(|| theme::fallback_active_theme(&draft));
                        this.previewing_appearance = false;
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
        if self.page == Page::Displays {
            self.display_drafts = self.outputs.iter().map(OutputDraft::from_output).collect();
            self.display_status = "Display changes reset.".into();
            self.display_error = None;
            self.confirming_close = false;
            log::info!("STUDIO event=display-undo");
            cx.notify();
            return;
        }
        self.draft = self.saved.clone().unwrap_or_default();
        self.previewing_appearance = false;
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
        if self.page == Page::Displays {
            self.refresh_displays(cx);
            return;
        }
        match self.store.load() {
            Ok(saved) => {
                self.saved = saved;
                self.draft = self.saved.clone().unwrap_or_default();
                self.live_theme = ThemeStore::for_user()
                    .map(|theme_store| theme_store.load_active())
                    .unwrap_or_else(|_| theme::fallback_active_theme(&self.draft));
                self.previewing_appearance = false;
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
        if self.has_unsaved_edits() {
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
        ui_button(
            ("button", index),
            &self.controls[index],
            label,
            p,
            window,
            if primary {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Secondary
            },
            enabled,
        )
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
        let preview = theme::palette_for_preferences(&Preferences {
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
                .child(divider(p))
                .child(self.toggle(CONTROL_REDUCED_MOTION,if session::on_crowbaros() { "Reduced motion preference" } else { "Reduced motion" },if session::on_crowbaros() { "Saved for compatible desktops. This desktop has no animations." } else { "Turn off desktop window and workspace animations when you save." },self.draft.reduced_motion,p,window,cx)))
            .child(div().text_size(px(12.)).text_color(rgb(p.muted))
                .child(if session::on_crowbaros() { "Saved appearance and wallpaper also apply to the shell. Other applications keep their own themes." } else { "Appearance currently styles Studio Settings. Reduced motion applies to your Hyprland desktop and is restored at login." }))
    }
    fn displays(&self, p: Palette, window: &Window, cx: &Context<Self>) -> Div {
        let mut page = div().flex().flex_col().gap_4().child(
            div()
                .flex()
                .items_start()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(28.))
                                .font_weight(FontWeight::BOLD)
                                .child("Displays"),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(rgb(p.muted))
                                .child("Resolution, scale, and layout for connected monitors."),
                        ),
                )
                .child(
                    self.button(
                        CONTROL_DISPLAY_REFRESH,
                        if self.displays_loading {
                            "Refreshing…"
                        } else {
                            "Refresh"
                        },
                        p,
                        window,
                        false,
                        !self.displays_loading && !self.busy,
                    )
                    .h(px(34.))
                    .px_3()
                    .on_click(cx.listener(|this, _, w, cx| {
                        w.focus(&this.controls[CONTROL_DISPLAY_REFRESH]);
                        this.refresh_displays(cx);
                    })),
                ),
        );

        if self.displays_loading && !self.displays_loaded {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("Reading connected displays…"),
            );
        }

        if let Some(reason) = &self.displays_unavailable {
            return page.child(
                card(p)
                    .p_5()
                    .gap_2()
                    .child(section_header("Display controls unavailable", None, p))
                    .child(
                        div()
                            .text_size(px(TEXT_BODY))
                            .text_color(rgb(p.muted))
                            .child(reason.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_SUPPORTING))
                            .text_color(rgb(p.muted))
                            .child("Sign into a Fulcrum session to adjust resolution, scale, and monitor layout."),
                    ),
            );
        }

        if self.outputs.is_empty() {
            return page.child(
                card(p)
                    .p_5()
                    .text_size(px(TEXT_BODY))
                    .text_color(rgb(p.muted))
                    .child("No displays reported by Fulcrum."),
            );
        }

        let selected = self.selected_display.min(self.outputs.len() - 1);
        let stage_w = 540.0f32;
        let stage_h = 114.0f32;
        let rects = project_diagram(&self.display_drafts, stage_w, stage_h, 10.0);

        // Arrangement canvas
        let mut stage = div().w(px(stage_w)).h(px(stage_h)).relative();
        for (i, ((output, draft), rect)) in self
            .outputs
            .iter()
            .zip(&self.display_drafts)
            .zip(rects)
            .enumerate()
        {
            let is_selected = i == selected;
            let (lw, lh) = logical_size(draft.width, draft.height, draft.scale);
            let modified = !draft.matches_output(output);
            stage = stage.child(
                div()
                    .id(("display-node", i))
                    .absolute()
                    .left(px(rect.x))
                    .top(px(rect.y))
                    .w(px(rect.width))
                    .h(px(rect.height))
                    .rounded(px(8.))
                    .border_2()
                    .border_color(rgb(if is_selected { p.focus } else { p.line }))
                    .bg(rgb(p.canvas))
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(2.))
                    .overflow_hidden()
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.9))
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(if is_selected { p.text } else { p.muted }))
                            .child(output.name.clone()),
                    )
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(TEXT_SELECTION_LABEL))
                            .text_color(rgb(p.muted))
                            .child(format!("{lw}×{lh}")),
                    )
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(TEXT_SELECTION_LABEL))
                            .text_color(rgb(if modified { p.accent } else { p.muted }))
                            .child(format_scale(draft.scale)),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_display = i;
                        cx.notify();
                    })),
            );
        }

        let mut arrangement = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Arrangement"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(p.muted))
                            .child(if self.outputs.len() == 1 {
                                "1 display connected".to_string()
                            } else {
                                format!("{} displays connected · click a display to configure", self.outputs.len())
                            }),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .h(px(126.))
                    .rounded(px(12.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .child(stage),
            );

        if self.outputs.len() >= 2 {
            let anchor_idx = if selected == 0 { 1 } else { 0 };
            let anchor_name = self.outputs[anchor_idx].name.clone();
            let selected_name = self.outputs[selected].name.clone();
            arrangement = arrangement.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(p.muted))
                            .child(format!("Place {selected_name} relative to {anchor_name}")),
                    )
                    .child(div().flex().gap_2().children(
                        RelativeSide::ALL.into_iter().enumerate().map(|(si, side)| {
                            div()
                                .id(("display-side", si))
                                .h(px(30.))
                                .px_3()
                                .rounded(px(8.))
                                .border_1()
                                .border_color(rgb(p.line))
                                .bg(rgb(p.surface))
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(p.text))
                                .cursor_pointer()
                                .hover(|s| s.opacity(0.85))
                                .child(side.label())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.busy {
                                        return;
                                    }
                                    let idx = this
                                        .selected_display
                                        .min(this.display_drafts.len().saturating_sub(1));
                                    arrange_relative(&mut this.display_drafts, idx, side);
                                    this.display_changed(cx);
                                }))
                        }),
                    )),
            );
        }

        page = page.child(arrangement);

        // Selected display summary
        let output = &self.outputs[selected];
        let draft = &self.display_drafts[selected];
        let (lw, lh) = logical_size(draft.width, draft.height, draft.scale);
        let modified = !draft.matches_output(output);
        let identity_text = if output.identity.trim().is_empty() {
            "Connected display".to_string()
        } else {
            output.identity.clone()
        };

        page = page.child(
            card(p)
                .px_4()
                .py_3()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .text_size(px(TEXT_SECTION))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(output.name.clone()),
                                )
                                .child(
                                    div()
                                        .font_family(FONT_MONO)
                                        .text_size(px(TEXT_TECHNICAL))
                                        .text_color(rgb(p.muted))
                                        .child(identity_text),
                                ),
                        )
                        .child(badge(
                            if modified { "Pending" } else { "Active" },
                            p,
                            modified,
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_4()
                        .font_family(FONT_MONO)
                        .text_size(px(TEXT_TECHNICAL))
                        .text_color(rgb(p.text))
                        .child(format!(
                            "Mode: {}×{} @ {}",
                            draft.width,
                            draft.height,
                            format_refresh(draft.refresh)
                        ))
                        .child(format!("Logical: {lw}×{lh}"))
                        .child(format!("Position: ({}, {})", draft.x, draft.y)),
                ),
        );

        // Scale picker
        let current_scale = draft.scale;
        page = page.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(TEXT_SECTION))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Scale"),
                        )
                        .child(
                            div()
                                .text_size(px(TEXT_TECHNICAL))
                                .text_color(rgb(p.muted))
                                .child("Interface size in 25% steps"),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_2()
                        .children(SCALES.into_iter().enumerate().map(|(si, scale)| {
                            let is_active = current_scale == scale;
                            action_chip(("display-scale", si), format_scale(scale), p, is_active)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if this.busy {
                                        return;
                                    }
                                    if let Some(d) = this.display_drafts.get_mut(selected) {
                                        d.scale = scale;
                                        this.display_changed(cx);
                                    }
                                }))
                        })),
                ),
        );

        // Resolution & refresh picker
        let current_mode = draft.mode();
        let live_mode = DisplayMode {
            width: output.width,
            height: output.height,
            refresh: output.refresh,
        };
        let modes = dedup_sorted_modes(&output.modes, current_mode);
        let mut mode_list = div()
            .rounded(px(12.))
            .border_1()
            .border_color(rgb(p.line))
            .bg(rgb(p.surface))
            .overflow_hidden()
            .flex()
            .flex_col();

        for (mi, mode) in modes.into_iter().enumerate() {
            let is_selected = mode == current_mode;
            let is_live = mode == live_mode;
            mode_list = mode_list.child(
                div()
                    .id(("display-mode", mi))
                    .px_4()
                    .py_3()
                    .when(mi > 0, |d| d.border_t_1().border_color(rgb(p.line)))
                    .when(is_selected, |d| d.bg(rgb(p.canvas)))
                    .flex()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.88))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(if is_selected {
                                        FontWeight::BOLD
                                    } else {
                                        FontWeight::SEMIBOLD
                                    })
                                    .child(format!("{} × {}", mode.width, mode.height)),
                            )
                            .child(
                                div()
                                    .font_family(FONT_MONO)
                                    .text_size(px(TEXT_SUPPORTING))
                                    .text_color(rgb(p.muted))
                                    .child(format_refresh(mode.refresh)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(is_selected, |d| {
                                d.child(div().size(px(7.)).rounded_full().bg(rgb(p.accent)))
                            })
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(if is_selected { p.text } else { p.muted }))
                                    .child(if is_selected {
                                        "Selected"
                                    } else if is_live {
                                        "Current"
                                    } else {
                                        ""
                                    }),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.busy {
                            return;
                        }
                        if let Some(d) = this.display_drafts.get_mut(selected) {
                            d.width = mode.width;
                            d.height = mode.height;
                            d.refresh = mode.refresh;
                            this.display_changed(cx);
                        }
                    })),
            );
        }

        page.child(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Resolution & refresh rate"),
                )
                .child(mode_list),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(rgb(p.muted))
                .child("Applied display settings take effect immediately and are remembered for this monitor across sessions."),
        )
    }
    fn about(&self, p: Palette) -> Div {
        div().flex().flex_col().gap_5()
            .child(div().text_size(px(TEXT_TITLE)).font_weight(FontWeight::BOLD).child("Studio Settings"))
            .child(div().text_size(px(TEXT_SECTION)).child("Your workspace, with room to make it your own."))
            .child(div().flex().flex_col().gap_3().text_size(px(TEXT_BODY)).text_color(rgb(p.muted))
                .child("Preview your appearance, then save it for your next session. Configure monitor resolution, fractional scale, and layout on the Displays page.")
                .child("Saved appearance and wallpaper apply to the shell and native CrowbarOS apps. Display settings apply directly through Fulcrum and persist for each monitor.")
                .child("Keyboard: Tab and Shift+Tab move focus. Enter or Space activates a control. Ctrl+S saves or applies; Ctrl+Z undoes; Ctrl+Q closes."))
            .child(div().border_t_1().border_color(rgb(p.line)).pt_5().flex().flex_col().gap_2()
                .child(div().text_size(px(TEXT_LABEL)).child("CrowbarOS · Studio Settings 0.1.3"))
                .child(div().font_family(FONT_MONO).text_size(px(TEXT_TECHNICAL)).text_color(rgb(p.muted)).child("Rust / GPUI 0.2.2 / Wayland")))
    }
}

impl Render for Settings {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.active_palette();
        let on_displays = self.page == Page::Displays;
        let active_dirty = if on_displays {
            self.displays_dirty()
        } else {
            self.appearance_dirty()
        };
        let can_primary = if on_displays {
            self.displays_dirty() && !self.busy && !self.displays_loading
        } else {
            self.appearance_dirty() && !self.load_error && !self.busy
        };
        let active_error = if on_displays {
            self.display_error.clone()
        } else {
            self.error.clone()
        };
        let status = if on_displays {
            if !self.display_status.is_empty() {
                self.display_status.clone()
            } else if self.displays_unavailable.is_some() {
                "Display controls unavailable".into()
            } else if self.displays_dirty() {
                "Pending display changes · not applied".into()
            } else if self.displays_loading {
                "Reading displays…".into()
            } else {
                "Display settings up to date".into()
            }
        } else if !self.status.is_empty() {
            self.status.clone()
        } else if self.appearance_dirty() {
            "Preview · not saved".into()
        } else {
            "All changes saved".into()
        };
        let primary_label = if on_displays {
            if self.busy {
                "Applying…"
            } else {
                "Apply changes"
            }
        } else if self.busy {
            "Saving…"
        } else {
            "Save changes"
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
            .font_family(FONT_UI)
            .child(titlebar(
                "Studio Settings",
                self.button(CONTROL_CLOSE, "Close", p, window, false, !self.busy)
                    .h(px(30.))
                    .on_click(cx.listener(|this, _, w, cx| this.quit(&Quit, w, cx))),
                p,
            ))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        sidebar(SIDEBAR_WIDTH, p)
                            .child(
                                self.button(
                                    CONTROL_APPEARANCE,
                                    "Appearance",
                                    p,
                                    window,
                                    self.page == Page::Appearance,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.page = Page::Appearance;
                                    w.focus(&this.controls[CONTROL_APPEARANCE]);
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.button(
                                    CONTROL_DISPLAYS,
                                    "Displays",
                                    p,
                                    window,
                                    self.page == Page::Displays,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.page = Page::Displays;
                                    w.focus(&this.controls[CONTROL_DISPLAYS]);
                                    if !this.displays_loaded && !this.displays_loading {
                                        this.refresh_displays(cx);
                                    }
                                    cx.notify();
                                })),
                            )
                            .child(
                                self.button(
                                    CONTROL_ABOUT,
                                    "About",
                                    p,
                                    window,
                                    self.page == Page::About,
                                    true,
                                )
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.page = Page::About;
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
                                    .child(match self.page {
                                        Page::Appearance => self.appearance(p, window, cx),
                                        Page::Displays => self.displays(p, window, cx),
                                        Page::About => self.about(p),
                                    }),
                            )
                            .when(active_error.is_some(), |d| {
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
                                                .child(active_error.clone().unwrap_or_default()),
                                        )
                                        .child(
                                            self.button(
                                                CONTROL_RELOAD,
                                                if on_displays {
                                                    "Refresh"
                                                } else {
                                                    "Reload saved"
                                                },
                                                p,
                                                window,
                                                false,
                                                !self.busy && !self.displays_loading,
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
                                status_footer(
                                    if self.confirming_close {
                                        "Discard unsaved changes?".into()
                                    } else {
                                        status
                                    },
                                    p,
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
                                                active_dirty && !self.busy,
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
                                                primary_label,
                                                p,
                                                window,
                                                true,
                                                can_primary,
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
        load_fonts(cx);
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
                        if this.has_unsaved_edits() {
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
        assert_eq!(CONTROL_DISPLAYS, 16);
        assert_eq!(CONTROL_DISPLAY_REFRESH, 17);
        assert_eq!(CONTROL_COUNT, 18);
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
