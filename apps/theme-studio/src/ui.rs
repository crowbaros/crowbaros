// SPDX-License-Identifier: MIT
use crowbar_theme::{Appearance, Theme as Bundle, ThemeColors, ThemeStore, extract, images};
use crowbar_ui::{ButtonVariant, FONT_MONO, FONT_UI, ThemeWatcher, UiPalette, button, load_fonts, spawn_theme_watcher};
use gpui::{actions, prelude::*, *};
use gpui_component::{
    Root, Theme, ThemeMode,
    input::{Input, InputEvent, InputState},
};
use std::{path::PathBuf, sync::Arc};
actions!(studio, [Quit, Tab, BackTab, CancelClose]);
#[derive(Clone)]
enum Action {
    Close,
    KeepEditing,
    DiscardClose,
    Refresh,
    Apply(String),
    Delete(String),
    Load(bool),
    Save,
    Mode(Appearance),
    Accent(u32),
    Remove(usize),
}
struct Loaded {
    path: PathBuf,
    pixels: image::RgbaImage,
    candidates: Vec<u32>,
    append: bool,
}
struct Outcome {
    message: String,
    loaded: Option<Loaded>,
}
struct Studio {
    store: ThemeStore,
    themes: Vec<Bundle>,
    active: String,
    path: Entity<InputState>,
    name: Entity<InputState>,
    appearance: Appearance,
    candidates: Vec<u32>,
    colors: ThemeColors,
    chosen_accent: u32,
    backgrounds: Vec<PathBuf>,
    preview: Option<Arc<RenderImage>>,
    palette: UiPalette,
    message: String,
    error: bool,
    busy: bool,
    confirm_delete: Option<String>,
    focus: FocusHandle,
    controls: Vec<FocusHandle>,
    draft: crowbaros_theme_studio::DraftState,
    _name_subscription: Subscription,
}
fn appearance(p: UiPalette, cx: &mut App) {
    Theme::change(if p.is_dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    let t = Theme::global_mut(cx);
    t.font_family = FONT_UI.into();
    t.mono_font_family = FONT_MONO.into();
    t.background = rgb(p.canvas).into();
    t.foreground = rgb(p.text).into();
    t.border = rgb(p.line).into();
    t.ring = rgb(p.focus).into();
    t.caret = rgb(p.focus).into();
    t.input = rgb(p.surface).into();
    t.muted_foreground = rgb(p.muted).into();
}
impl Studio {
    fn new(w: &mut Window, cx: &mut Context<Self>) -> Self {
        w.set_window_title("Theme Studio");
        let store = ThemeStore::for_user().expect("person configuration directory");
        let palette = ThemeWatcher::for_user()
            .ok()
            .map(|t| UiPalette::from_active(&t.current()))
            .unwrap_or_default();
        appearance(palette, cx);
        spawn_theme_watcher(cx, |s, active, cx| {
            s.active = active.theme_id.clone();
            s.palette = UiPalette::from_active(&active);
            appearance(s.palette, cx);
            cx.notify();
        });
        let path = cx.new(|cx| {
            InputState::new(w, cx)
                .placeholder("Image path — PNG, JPEG, WebP, GIF, BMP or TIFF")
                .validate(|s, _| s.len() <= 4096)
        });
        let name = cx.new(|cx| {
            InputState::new(w, cx)
                .placeholder("Name your theme")
                .validate(|s, _| s.len() <= 80)
        });
        let name_subscription = cx.subscribe_in(&name, w, |s: &mut Self, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                s.draft.changed();
                cx.notify();
            }
        });
        let mut s = Self {
            draft: Default::default(),
            _name_subscription: name_subscription,
            store,
            themes: vec![],
            active: String::new(),
            path,
            name,
            appearance: Appearance::Dark,
            candidates: vec![],
            colors: extract::palette_from_accent(0x49d7e8, Appearance::Dark),
            chosen_accent: 0x49d7e8,
            backgrounds: vec![],
            preview: None,
            palette,
            message: "Load an image to create your own palette.".into(),
            error: false,
            busy: false,
            confirm_delete: None,
            focus: cx.focus_handle(),
            controls: (0..640).map(|_| cx.focus_handle().tab_stop(true)).collect(),
        };
        let weak = cx.weak_entity();
        w.on_window_should_close(cx, move |w, cx| {
            weak.update(cx, |s, cx| {
                if s.busy {
                    return false;
                }
                if s.draft.request_close() {
                    true
                } else {
                    s.controls[16].focus(w);
                    cx.notify();
                    false
                }
            })
            .unwrap_or(true)
        });
        s.path.read(cx).focus_handle(cx).focus(w);
        s.execute(Action::Refresh, w, cx);
        s
    }
    fn execute(&mut self, action: Action, w: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Close => {
                if self.busy {
                    self.message = "Wait for the current operation to finish before closing.".into();
                } else if self.draft.request_close() {
                    w.remove_window();
                    return;
                } else {
                    self.controls[16].focus(w);
                }
                cx.notify();
                return;
            }
            Action::KeepEditing => {
                self.draft.keep_editing();
                self.name.read(cx).focus_handle(cx).focus(w);
                cx.notify();
                return;
            }
            Action::DiscardClose => {
                w.remove_window();
                return;
            }
            _ => (),
        }
        if self.busy {
            return;
        }
        match action {
            Action::Mode(mode) => {
                if !self.backgrounds.is_empty() {
                    self.draft.changed();
                }
                self.appearance = mode;
                self.colors = extract::palette_from_accent(self.chosen_accent, mode);
                cx.notify();
                return;
            }
            Action::Accent(color) => {
                self.draft.changed();
                self.chosen_accent = color;
                self.colors = extract::palette_from_accent(color, self.appearance);
                cx.notify();
                return;
            }
            Action::Remove(index) => {
                if index < self.backgrounds.len() {
                    self.backgrounds.remove(index);
                    self.draft.changed();
                }
                cx.notify();
                return;
            }
            Action::Delete(ref id) if self.confirm_delete.as_ref() != Some(id) => {
                self.confirm_delete = Some(id.clone());
                self.message = "Select Delete again to remove this theme and its copied backgrounds.".into();
                cx.notify();
                return;
            }
            _ => (),
        }
        let store = self.store.clone();
        let path = PathBuf::from(self.path.read(cx).value().as_str());
        let name = self.name.read(cx).value().to_string();
        if matches!(action, Action::Save) && name.trim().is_empty() {
            self.name.read(cx).focus_handle(cx).focus(w);
            self.message = "Name your theme before saving.".into();
            self.error = true;
            cx.notify();
            return;
        }
        let colors = self.colors.clone();
        let mode = self.appearance;
        let backgrounds = self.backgrounds.clone();
        let saving_revision = matches!(action, Action::Save).then_some(self.draft.revision());
        self.busy = true;
        self.error = false;
        self.message = "Working…".into();
        self.confirm_delete = None;
        cx.notify();
        let task = cx.background_executor().spawn(async move {
            let result = (|| -> Result<Outcome, String> {
                let mut loaded = None;
                let message = match action {
                    Action::Refresh => "Choose a theme, or create one from an image.".into(),
                    Action::Apply(id) => format!("Applied {}", images::apply(&store, &id, None)?.theme_name),
                    Action::Delete(id) => {
                        if store.load_active().theme_id == id {
                            images::apply(&store, "nullkeel-dark", None)?;
                        }
                        store.delete_custom_theme(&id)?;
                        "Theme deleted.".into()
                    }
                    Action::Load(append) => {
                        if append && backgrounds.len() >= 16 {
                            return Err("A theme can hold up to 16 backgrounds.".into());
                        }
                        let pixels = images::load(&path)?;
                        let candidates = extract::dominant_colors(pixels.as_raw())?;
                        loaded = Some(Loaded {
                            path,
                            pixels,
                            candidates,
                            append,
                        });
                        if append {
                            "Background added.".into()
                        } else {
                            "Choose an accent and name your theme.".into()
                        }
                    }
                    Action::Save => {
                        if backgrounds.is_empty() {
                            return Err("Load an image first.".into());
                        }
                        let id = crowbar_theme::slugify(&name);
                        if store.get_theme(&id).is_some() {
                            return Err("That theme name is already used. Choose another name.".into());
                        }
                        let theme = store.save_custom_theme(&name, colors, mode, &backgrounds)?;
                        images::apply(&store, &theme.id, None)?;
                        format!("Saved and applied {}", theme.name)
                    }
                    _ => unreachable!(),
                };
                Ok(Outcome { message, loaded })
            })();
            (
                result,
                store.list_themes().into_iter().take(128).collect::<Vec<_>>(),
                store.load_active().theme_id,
            )
        });
        cx.spawn(async move |entity, cx| {
            let (result, themes, active) = task.await;
            let _ = entity.update(cx, |s, cx| {
                s.busy = false;
                s.themes = themes;
                s.active = active;
                match result {
                    Ok(out) => {
                        if let Some(revision) = saving_revision {
                            s.draft.saved(revision);
                        }
                        s.message = out.message;
                        if let Some(mut loaded) = out.loaded {
                            s.draft.changed();
                            if !loaded.append {
                                s.backgrounds.clear();
                                s.candidates = loaded.candidates;
                                s.chosen_accent = s.candidates[0];
                                s.colors = extract::palette_from_accent(s.chosen_accent, s.appearance);
                                for p in loaded.pixels.chunks_exact_mut(4) {
                                    p.swap(0, 2);
                                }
                                s.preview = Some(Arc::new(RenderImage::new([image::Frame::new(loaded.pixels)])));
                            }
                            if !s.backgrounds.contains(&loaded.path) {
                                s.backgrounds.push(loaded.path);
                            }
                        }
                    }
                    Err(error) => {
                        s.message = error;
                        s.error = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn control(
        &self,
        id: usize,
        label: impl Into<SharedString>,
        action: Action,
        enabled: bool,
        primary: bool,
        w: &Window,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let enabled = enabled && (!self.busy || matches!(action, Action::Close));
        let key_action = action.clone();
        button(
            ("theme-control", id),
            &self.controls[id],
            label,
            self.palette,
            w,
            if primary {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Secondary
            },
            enabled,
        )
        .when(enabled, |d| {
            d.on_click(cx.listener(move |s, _, w, cx| s.execute(action.clone(), w, cx)))
                .on_key_down(cx.listener(move |s, event: &KeyDownEvent, w, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        s.execute(key_action.clone(), w, cx);
                        cx.stop_propagation();
                    }
                }))
        })
    }
}
impl Render for Studio {
    fn render(&mut self, w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let c = &self.colors;
        let mut library = div()
            .id("themes")
            .w(px(250.))
            .flex_shrink_0()
            .h_full()
            .overflow_y_scroll()
            .p(px(18.))
            .bg(rgb(p.surface))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Installed themes"),
            );
        for (i, theme) in self.themes.iter().enumerate() {
            let active = theme.id == self.active;
            let mut row = div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .py(px(8.))
                .border_b_1()
                .border_color(rgb(p.line))
                .child(self.control(
                    32 + i * 3,
                    theme.name.clone(),
                    Action::Apply(theme.id.clone()),
                    true,
                    active,
                    w,
                    cx,
                ))
                .child(swatches(&theme.colors))
                .child(div().text_size(px(11.)).text_color(rgb(p.muted)).child(if active {
                    "Active theme".into()
                } else {
                    format!("{} backgrounds", theme.backgrounds.len())
                }));
            if !theme.builtin {
                row = row.child(self.control(
                    33 + i * 3,
                    if self.confirm_delete.as_ref() == Some(&theme.id) {
                        "Confirm delete"
                    } else {
                        "Delete"
                    },
                    Action::Delete(theme.id.clone()),
                    true,
                    false,
                    w,
                    cx,
                ));
            }
            library = library.child(row);
        }
        let mut accents = div().flex().flex_wrap().gap(px(8.));
        for (i, color) in self.candidates.iter().copied().enumerate() {
            accents = accents.child(
                self.control(
                    8 + i,
                    crowbar_theme::hex(color),
                    Action::Accent(color),
                    true,
                    color == self.chosen_accent,
                    w,
                    cx,
                )
                .gap(px(8.))
                .font_family(FONT_MONO)
                .child(div().size(px(14.)).rounded(px(3.)).bg(rgb(color))),
            );
        }
        let mut backgrounds = div().flex().flex_col().gap(px(6.));
        for (i, path) in self.backgrounds.iter().enumerate() {
            backgrounds = backgrounds.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(path.file_name().unwrap_or_default().to_string_lossy().into_owned()),
                    )
                    .child(self.control(440 + i, "Remove", Action::Remove(i), true, false, w, cx)),
            );
        }
        let preview = div()
            .flex_shrink_0()
            .h(px(144.))
            .w_full()
            .rounded(px(10.))
            .overflow_hidden()
            .bg(rgb(c.canvas))
            .when_some(self.preview.clone(), |d, img_data| {
                d.child(img(img_data).size_full().object_fit(ObjectFit::Cover))
            })
            .when(self.preview.is_none(), |d| {
                d.flex()
                    .items_center()
                    .justify_center()
                    .text_color(rgb(c.muted))
                    .child("Your image becomes your palette")
            });
        let editor_content = div()
            .p(px(24.))
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(
                div()
                    .text_size(px(24.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Create a theme"),
            )
            .child(
                div()
                    .text_color(rgb(p.muted))
                    .child("Start with an image. Choose its colors. Make the desktop yours."),
            )
            .child(preview)
            .child(Input::new(&self.path))
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(self.control(0, "Extract colors", Action::Load(false), true, true, w, cx))
                    .child(self.control(
                        1,
                        "Add background",
                        Action::Load(true),
                        self.preview.is_some(),
                        false,
                        w,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap(px(8.))
                    .child(self.control(
                        2,
                        "Dark",
                        Action::Mode(Appearance::Dark),
                        true,
                        self.appearance == Appearance::Dark,
                        w,
                        cx,
                    ))
                    .child(self.control(
                        3,
                        "Light",
                        Action::Mode(Appearance::Light),
                        true,
                        self.appearance == Appearance::Light,
                        w,
                        cx,
                    )),
            )
            .child(accents)
            .child(
                div()
                    .p(px(16.))
                    .rounded(px(10.))
                    .bg(rgb(c.surface))
                    .text_color(rgb(c.text))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .text_size(px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("A readable palette, in every shade"),
                    )
                    .child(
                        div()
                            .text_color(rgb(c.muted))
                            .child("Text and accent labels meet WCAG AA contrast."),
                    )
                    .child(swatches(c)),
            )
            .child(Input::new(&self.name))
            .child(backgrounds);
        let editor = div()
            .id("editor")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .child(editor_content);
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(p.canvas))
            .text_color(rgb(p.text))
            .font_family(FONT_UI)
            .text_size(px(13.))
            .key_context("ThemeStudio")
            .track_focus(&self.focus)
            .on_action(cx.listener(|s, _: &Quit, w, cx| s.execute(Action::Close, w, cx)))
            .on_action(cx.listener(|s, _: &CancelClose, w, cx| {
                if s.draft.confirming_close {
                    s.execute(Action::KeepEditing, w, cx);
                }
            }))
            .on_action(cx.listener(|_, _: &Tab, w, _| w.focus_next()))
            .on_action(cx.listener(|_, _: &BackTab, w, _| w.focus_prev()))
            .child(crowbar_ui::titlebar(
                "Theme Studio",
                div()
                    .flex()
                    .gap(px(8.))
                    .child(self.control(4, "Refresh themes", Action::Refresh, true, false, w, cx))
                    .child(self.control(6, "Close", Action::Close, true, false, w, cx)),
                p,
            ))
            .child(div().flex_1().min_h_0().flex().child(library).child(editor))
            .child(
                div()
                    .min_h(px(70.))
                    .flex_shrink_0()
                    .px(px(20.))
                    .py(px(12.))
                    .flex()
                    .gap(px(16.))
                    .items_center()
                    .border_t_1()
                    .border_color(rgb(p.line))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(rgb(if self.error { p.danger } else { p.muted }))
                            .child(if self.draft.confirming_close {
                                "Discard this unsaved theme?".to_string()
                            } else {
                                self.message.clone()
                            }),
                    )
                    .when(self.draft.confirming_close, |d| {
                        d.child(self.control(16, "Keep editing", Action::KeepEditing, true, true, w, cx))
                            .child(self.control(17, "Discard & close", Action::DiscardClose, true, false, w, cx))
                    })
                    .when(!self.draft.confirming_close, |d| {
                        d.child(self.control(
                            5,
                            "Save & apply",
                            Action::Save,
                            !self.backgrounds.is_empty(),
                            true,
                            w,
                            cx,
                        ))
                    }),
            )
    }
}
fn swatches(c: &ThemeColors) -> Div {
    div().flex().gap(px(5.)).children(
        [c.canvas, c.surface, c.text, c.accent, c.danger, c.success]
            .into_iter()
            .map(|color| div().w(px(24.)).h(px(14.)).rounded(px(3.)).bg(rgb(color))),
    )
}
pub fn run() {
    Application::new().with_assets(gpui_component_assets::Assets).run(|cx| {
        gpui_component::init(cx);
        load_fonts(cx);
        cx.bind_keys([
            KeyBinding::new("ctrl-q", Quit, Some("ThemeStudio")),
            KeyBinding::new("escape", CancelClose, Some("ThemeStudio")),
            KeyBinding::new("tab", Tab, Some("ThemeStudio")),
            KeyBinding::new("shift-tab", BackTab, Some("ThemeStudio")),
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1000.), px(850.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(820.), px(660.))),
                app_id: Some("com.crowbaros.ThemeStudio".into()),
                ..Default::default()
            },
            |w, cx| {
                let studio = cx.new(|cx| Studio::new(w, cx));
                cx.new(|cx| Root::new(studio, w, cx))
            },
        )
        .expect("open Theme Studio window");
    });
}
