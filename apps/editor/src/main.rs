// SPDX-License-Identifier: MIT
mod appearance;
use crowbar_ui::{FONT_UI, ThemeWatcher, UiPalette, load_fonts, spawn_theme_watcher};
use crowbaros_editor::document;
use gpui::{
    Action, App, Application, Bounds, Context, Entity, EntityInputHandler, FocusHandle, Focusable, KeyBinding,
    Subscription, Window, WindowBounds, WindowOptions, actions, div, prelude::*, px, rgb, size,
};
use gpui_component::{
    Root, Rope, RopeExt,
    input::{Input, InputEvent, InputState, Position, Redo},
    text::TextView,
};
use std::path::{Path, PathBuf};

actions!(
    editor,
    [
        New,
        Open,
        Save,
        SaveAs,
        Close,
        Quit,
        NextTab,
        PreviousTab,
        Replace,
        GoTo,
        Preview,
        LineNumbers,
        SoftWrap,
        Dismiss,
        Confirm
    ]
);

struct Tab {
    path: Option<PathBuf>,
    buffer: Entity<InputState>,
    saved: Rope,
    preview: bool,
    _events: Subscription,
}

impl Tab {
    fn name(&self) -> String {
        self.path
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into())
    }

    fn dirty(&self, cx: &App) -> bool {
        self.buffer.read(cx).text() != &self.saved
    }
}

#[derive(Clone, Debug)]
enum Prompt {
    Open,
    SaveAs,
    Line,
    FindForReplace,
    Replacement(String),
    Overwrite(PathBuf),
    Discard { quit: bool },
}

impl Prompt {
    fn label(&self) -> &'static str {
        match self {
            Self::Open => "Open file",
            Self::SaveAs => "Save as",
            Self::Line => "Go to line",
            Self::FindForReplace => "Find text to replace",
            Self::Replacement(_) => "Replace with",
            Self::Overwrite(_) => "Replace the existing file?",
            Self::Discard { .. } => "Discard unsaved changes?",
        }
    }
    fn confirmation(&self) -> bool {
        matches!(self, Self::Overwrite(_) | Self::Discard { .. })
    }
}

struct Editor {
    tabs: Vec<Tab>,
    active: usize,
    prompt: Option<Prompt>,
    field: Entity<InputState>,
    focus: FocusHandle,
    message: String,
    palette: UiPalette,
    numbers: bool,
    wrap: bool,
}

impl Editor {
    fn new(paths: Vec<PathBuf>, w: &mut Window, cx: &mut Context<Self>) -> Self {
        let field = cx.new(|cx| InputState::new(w, cx));
        let palette = ThemeWatcher::for_user()
            .ok()
            .map(|watcher| UiPalette::from_active(&watcher.current()))
            .unwrap_or_default();
        appearance::apply(palette, cx);
        spawn_theme_watcher(cx, |this, active, cx| {
            this.palette = UiPalette::from_active(&active);
            appearance::apply(this.palette, cx);
            cx.notify();
        });
        let mut this = Self {
            tabs: Vec::new(),
            active: 0,
            prompt: None,
            field,
            focus: cx.focus_handle(),
            message: String::new(),
            palette,
            numbers: true,
            wrap: true,
        };
        for path in paths {
            this.open(path, w, cx);
        }
        if this.tabs.is_empty() {
            this.add(None, String::new(), w, cx);
        }
        let weak = cx.weak_entity();
        w.on_window_should_close(cx, move |w, cx| {
            weak.update(cx, |this, cx| {
                if this.tabs.iter().any(|t| t.dirty(cx)) {
                    this.request_close(true, w, cx);
                    false
                } else {
                    true
                }
            })
            .unwrap_or(true)
        });
        this
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }
    fn focus_buffer(&self, w: &mut Window, cx: &mut Context<Self>) {
        if self.tab().preview {
            self.focus.focus(w);
        } else {
            self.tab().buffer.read(cx).focus_handle(cx).focus(w);
        }
    }
    fn add(&mut self, path: Option<PathBuf>, text: String, w: &mut Window, cx: &mut Context<Self>) {
        let buffer = cx.new(|cx| {
            InputState::new(w, cx)
                .code_editor(document::language(path.as_deref()))
                .line_number(self.numbers)
                .soft_wrap(self.wrap)
                .searchable(true)
                .default_value(text)
        });
        let saved = buffer.read(cx).text().clone();
        let events = cx.subscribe(&buffer, |_, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify()
            }
        });
        self.tabs.push(Tab {
            path,
            buffer,
            saved,
            preview: false,
            _events: events,
        });
        self.select(self.tabs.len() - 1, w, cx);
    }
    fn select(&mut self, index: usize, w: &mut Window, cx: &mut Context<Self>) {
        self.active = index;
        self.prompt = None;
        self.focus_buffer(w, cx);
        cx.notify();
    }
    fn open(&mut self, path: PathBuf, w: &mut Window, cx: &mut Context<Self>) {
        match std::fs::canonicalize(&path).and_then(|p| {
            if let Some(index) = self.tabs.iter().position(|t| t.path.as_ref() == Some(&p)) {
                self.select(index, w, cx);
            } else {
                let text = document::read(&p)?;
                self.add(Some(p), text, w, cx);
            }
            Ok(())
        }) {
            Ok(()) => (),
            Err(e) => self.message = format!("Could not open {} · {e}", path.display()),
        }
    }
    fn ask(&mut self, prompt: Prompt, initial: String, w: &mut Window, cx: &mut Context<Self>) {
        if prompt.confirmation() {
            self.focus.focus(w);
        } else {
            self.field.update(cx, |input, cx| {
                input.set_value(initial, w, cx);
                input.focus(w, cx);
            });
        }
        self.prompt = Some(prompt);
        self.message.clear();
        cx.notify();
    }
    fn write(&mut self, path: PathBuf, w: &mut Window, cx: &mut Context<Self>) {
        let text = self.tab().buffer.read(cx).value();
        match document::save(&path, &text) {
            Ok(()) => {
                let tab = &mut self.tabs[self.active];
                tab.path = Some(std::fs::canonicalize(&path).unwrap_or(path));
                tab.saved = tab.buffer.read(cx).text().clone();
                tab.buffer.update(cx, |input, cx| {
                    input.set_highlighter(document::language(tab.path.as_deref()), cx)
                });
                self.message = format!("Saved {}", tab.name());
                self.prompt = None;
                self.focus_buffer(w, cx);
            }
            Err(e) => self.message = format!("Could not save {} · {e}", path.display()),
        }
        cx.notify();
    }
    fn request_close(&mut self, quit: bool, w: &mut Window, cx: &mut Context<Self>) {
        let dirty = if quit {
            self.tabs.iter().position(|t| t.dirty(cx))
        } else {
            self.tab().dirty(cx).then_some(self.active)
        };
        if let Some(i) = dirty {
            self.active = i;
            self.ask(Prompt::Discard { quit }, String::new(), w, cx);
        } else if quit {
            w.remove_window();
        } else {
            self.remove(w, cx);
        }
    }
    fn remove(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.len() == 1 {
            w.remove_window();
        } else {
            self.tabs.remove(self.active);
            self.select(self.active.min(self.tabs.len() - 1), w, cx);
        }
    }
    fn submit(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let Some(prompt) = self.prompt.clone() else { return };
        let value = self.field.read(cx).value().to_string();
        match prompt {
            Prompt::Open => self.open(PathBuf::from(value), w, cx),
            Prompt::SaveAs => {
                let path = PathBuf::from(value);
                if path.exists() && Some(&path) != self.tab().path.as_ref() {
                    self.ask(Prompt::Overwrite(path), String::new(), w, cx);
                } else {
                    self.write(path, w, cx);
                }
            }
            Prompt::Overwrite(path) => self.write(path, w, cx),
            Prompt::Discard { quit } => {
                if self.tabs.len() == 1 {
                    w.remove_window();
                    return;
                }
                self.remove(w, cx);
                if quit {
                    self.request_close(true, w, cx);
                }
            }
            Prompt::Line => {
                let count = self.tab().buffer.read(cx).text().lines_len();
                if let Some(line) = value.parse::<u32>().ok().filter(|&n| n > 0 && n as usize <= count) {
                    self.tabs[self.active].preview = false;
                    self.tab().buffer.update(cx, |input, cx| {
                        input.set_cursor_position(Position::new(line - 1, 0), w, cx)
                    });
                    self.prompt = None;
                } else {
                    self.message = format!("Enter a line from 1 to {count}");
                }
            }
            Prompt::FindForReplace => {
                if !value.is_empty() {
                    self.ask(Prompt::Replacement(value), String::new(), w, cx);
                }
            }
            Prompt::Replacement(query) => {
                let found = self.tab().buffer.update(cx, |input, cx| {
                    let text = input.value();
                    let Some(range) = document::next_match(&text, &query, input.cursor()) else {
                        return false;
                    };
                    input.replace_text_in_range(Some(range), &value, w, cx);
                    true
                });
                self.message = if found { "Replaced one match" } else { "No match" }.into();
                self.prompt = None;
                self.focus_buffer(w, cx);
            }
        }
        cx.notify();
    }
}

fn control(label: &'static str, action: impl Action, p: UiPalette) -> impl IntoElement {
    div()
        .id(label)
        .px_3()
        .py_1()
        .rounded_md()
        .cursor_pointer()
        .text_size(px(13.))
        .hover(move |style| style.bg(rgb(p.surface_elevated)))
        .on_click(move |_, w, cx| w.dispatch_action(action.boxed_clone(), cx))
        .child(label)
}

impl Render for Editor {
    fn render(&mut self, w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette;
        let mut view = div()
            .size_full()
            .flex()
            .flex_col()
            .font_family(FONT_UI)
            .text_size(px(14.))
            .bg(rgb(p.canvas))
            .text_color(rgb(p.text))
            .track_focus(&self.focus)
            .key_context(if self.prompt.is_some() {
                "EditorPrompt"
            } else {
                "Editor"
            })
            .on_action(cx.listener(|s, _: &New, w, cx| s.add(None, String::new(), w, cx)))
            .on_action(cx.listener(|s, _: &Open, w, cx| s.ask(Prompt::Open, String::new(), w, cx)))
            .on_action(cx.listener(|s, _: &SaveAs, w, cx| {
                s.ask(
                    Prompt::SaveAs,
                    s.tab()
                        .path
                        .as_deref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default(),
                    w,
                    cx,
                )
            }))
            .on_action(cx.listener(|s, _: &Save, w, cx| {
                if let Some(p) = s.tab().path.clone() {
                    s.write(p, w, cx)
                } else {
                    s.ask(Prompt::SaveAs, String::new(), w, cx)
                }
            }))
            .on_action(cx.listener(|s, _: &Close, w, cx| s.request_close(false, w, cx)))
            .on_action(cx.listener(|s, _: &Quit, w, cx| s.request_close(true, w, cx)))
            .on_action(cx.listener(|s, _: &NextTab, w, cx| s.select((s.active + 1) % s.tabs.len(), w, cx)))
            .on_action(
                cx.listener(|s, _: &PreviousTab, w, cx| s.select((s.active + s.tabs.len() - 1) % s.tabs.len(), w, cx)),
            )
            .on_action(cx.listener(|s, _: &Replace, w, cx| s.ask(Prompt::FindForReplace, String::new(), w, cx)))
            .on_action(cx.listener(|s, _: &GoTo, w, cx| s.ask(Prompt::Line, String::new(), w, cx)))
            .on_action(cx.listener(|s, _: &Preview, w, cx| {
                s.tabs[s.active].preview = !s.tab().preview;
                s.focus_buffer(w, cx);
                cx.notify()
            }))
            .on_action(cx.listener(|s, _: &LineNumbers, w, cx| {
                s.numbers = !s.numbers;
                for t in &s.tabs {
                    t.buffer.update(cx, |i, cx| i.set_line_number(s.numbers, w, cx));
                }
                cx.notify()
            }))
            .on_action(cx.listener(|s, _: &SoftWrap, w, cx| {
                s.wrap = !s.wrap;
                for t in &s.tabs {
                    t.buffer.update(cx, |i, cx| i.set_soft_wrap(s.wrap, w, cx));
                }
                cx.notify()
            }))
            .on_action(cx.listener(|s, _: &Dismiss, w, cx| {
                s.prompt = None;
                s.message.clear();
                s.focus_buffer(w, cx);
                cx.notify()
            }))
            .on_action(cx.listener(|s, _: &Confirm, w, cx| s.submit(w, cx)));
        let mut tabs = div()
            .id("tabs")
            .flex()
            .overflow_x_scroll()
            .flex_shrink_0()
            .bg(rgb(p.surface))
            .border_b_1()
            .border_color(rgb(p.line));
        for (i, t) in self.tabs.iter().enumerate() {
            let label = format!("{}{}", t.name(), if t.dirty(cx) { " •" } else { "" });
            tabs = tabs.child(
                div()
                    .id(("tab", i))
                    .flex_shrink_0()
                    .px_4()
                    .py_2()
                    .cursor_pointer()
                    .when(i == self.active, |d| {
                        d.border_b_2().border_color(rgb(p.focus)).bg(rgb(p.canvas))
                    })
                    .on_click(cx.listener(move |s, _, w, cx| {
                        if s.prompt.is_none() {
                            s.select(i, w, cx)
                        }
                    }))
                    .child(label),
            );
        }
        if self.prompt.is_none() {
            view = view.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .bg(rgb(p.surface))
                    .child(control("New", New, p))
                    .child(control("Open", Open, p))
                    .child(control("Save", Save, p))
                    .child(control("Save as", SaveAs, p))
                    .child(control(if self.tab().preview { "Edit" } else { "Preview" }, Preview, p)),
            );
        }
        view = view.child(tabs);
        if let Some(prompt) = &self.prompt {
            let mut bar = div()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .bg(rgb(p.surface_elevated))
                .child(prompt.label());
            if prompt.confirmation() {
                let detail = match prompt {
                    Prompt::Overwrite(path) => path.display().to_string(),
                    _ => self.tab().name(),
                };
                bar = bar.child(detail).child(control("Cancel", Dismiss, p)).child(control(
                    if matches!(prompt, Prompt::Discard { .. }) {
                        "Discard"
                    } else {
                        "Replace"
                    },
                    Confirm,
                    p,
                ));
            } else {
                bar = bar.child(Input::new(&self.field)).child("Enter · Esc");
            }
            view = view.child(bar);
        }
        let t = self.tab();
        w.set_window_title(&format!("{} — Editor", t.name()));
        let body = if t.preview {
            TextView::markdown("preview", t.buffer.read(cx).value(), w, cx)
                .scrollable(true)
                .into_any_element()
        } else {
            Input::new(&t.buffer).h_full().appearance(false).into_any_element()
        };
        let status = if !self.message.is_empty() {
            self.message.clone()
        } else if t.preview {
            "Markdown preview · Ctrl+Shift+P to edit".into()
        } else {
            t.path
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Ctrl+O  Open file     Ctrl+S  Save".into())
        };
        view.child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .p_3()
                .child(body)
                .when(self.prompt.is_some(), |d| d.child(div().absolute().inset_0().occlude())),
        )
        .child(
            div()
                .flex_shrink_0()
                .px_4()
                .py_1()
                .bg(rgb(p.surface))
                .text_color(rgb(p.muted))
                .text_size(px(12.))
                .child(status),
        )
    }
}

fn init(cx: &mut App) {
    gpui_component::init(cx);
    load_fonts(cx);
    cx.bind_keys([
        KeyBinding::new("ctrl-n", New, Some("Editor")),
        KeyBinding::new("ctrl-o", Open, Some("Editor")),
        KeyBinding::new("ctrl-s", Save, Some("Editor")),
        KeyBinding::new("ctrl-shift-s", SaveAs, Some("Editor")),
        KeyBinding::new("ctrl-w", Close, Some("Editor")),
        KeyBinding::new("ctrl-q", Quit, Some("Editor")),
        KeyBinding::new("ctrl-tab", NextTab, Some("Editor")),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("Editor")),
        KeyBinding::new("ctrl-h", Replace, Some("Editor")),
        KeyBinding::new("ctrl-g", GoTo, Some("Editor")),
        KeyBinding::new("ctrl-shift-p", Preview, Some("Editor")),
        KeyBinding::new("alt-l", LineNumbers, Some("Editor")),
        KeyBinding::new("alt-z", SoftWrap, Some("Editor")),
        KeyBinding::new("ctrl-shift-z", Redo, Some("Editor")),
        KeyBinding::new("enter", Confirm, Some("EditorPrompt > Input")),
        KeyBinding::new("escape", Dismiss, Some("EditorPrompt > Input")),
        KeyBinding::new("escape", Dismiss, Some("EditorPrompt")),
        KeyBinding::new("ctrl-enter", Confirm, Some("EditorPrompt")),
    ]);
}

fn main() {
    let paths = document::cli_paths(std::env::args_os().skip(1));
    Application::new()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx| {
            init(cx);
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(900.), px(600.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    app_id: Some("com.crowbaros.Editor".into()),
                    ..Default::default()
                },
                move |w, cx| {
                    let editor = cx.new(|cx| Editor::new(paths, w, cx));
                    cx.new(|cx| Root::new(editor, w, cx))
                },
            )
            .expect("open Editor window");
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn keyboard_edits_undo_redo_replace_and_save(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("note.md");
        std::fs::write(&path, "original").unwrap();
        cx.update(init);
        let mut editor_handle = None;
        let (_, cx) = cx.add_window_view(|w, cx| {
            let editor = cx.new(|cx| Editor::new(vec![path.clone()], w, cx));
            editor_handle = Some(editor.clone());
            Root::new(editor, w, cx)
        });
        let editor = editor_handle.unwrap();
        cx.simulate_keystrokes("ctrl-a");
        cx.simulate_keystrokes("d r a f t");
        cx.simulate_keystrokes("ctrl-z ctrl-s");
        assert_eq!(document::read(&path).unwrap(), "original");
        cx.simulate_keystrokes("ctrl-shift-z ctrl-s");
        assert_eq!(document::read(&path).unwrap(), "draft");
        cx.simulate_keystrokes("ctrl-h");
        cx.update(|_, cx| assert!(matches!(editor.read(cx).prompt, Some(Prompt::FindForReplace))));
        cx.simulate_keystrokes("d r a f t");

        cx.simulate_keystrokes("enter");
        cx.update(|_, cx| assert!(matches!(&editor.read(cx).prompt, Some(Prompt::Replacement(q)) if q=="draft")));
        cx.simulate_keystrokes("r e v i s e d");
        cx.simulate_keystrokes("enter");
        cx.update(|_, cx| assert_eq!(editor.read(cx).tab().buffer.read(cx).value().as_str(), "revised"));
        cx.simulate_keystrokes("ctrl-s");
        assert_eq!(document::read(&path).unwrap(), "revised");
        cx.simulate_keystrokes("ctrl-n n ctrl-tab ctrl-shift-p");
        cx.update(|_, cx| {
            let e = editor.read(cx);
            assert_eq!(e.tabs.len(), 2);
            assert_eq!(e.tab().path.as_deref(), Some(path.as_path()));
            assert!(e.tab().preview);
        });
        cx.simulate_keystrokes("ctrl-tab ctrl-w");
        cx.update(|_, cx| assert!(matches!(editor.read(cx).prompt, Some(Prompt::Discard { quit: false }))));
        cx.simulate_keystrokes("escape");
        cx.update(|_, cx| assert_eq!(editor.read(cx).tab().buffer.read(cx).value().as_str(), "n"));
        cx.simulate_keystrokes("ctrl-w ctrl-enter");
        cx.update(|_, cx| assert_eq!(editor.read(cx).tabs.len(), 1));
    }
}
