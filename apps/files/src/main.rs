// SPDX-License-Identifier: MIT
mod fs_model;
mod input;
mod state;
mod theme;

use fs_model::{Entry, SidebarEntry, SortField, SortOrder};
use gpui::{
    AnyElement, App, Application, AssetSource, Bounds, Context, Div, Entity, FocusHandle, Focusable, FontWeight,
    KeyBinding, MouseButton, MouseDownEvent, ObjectFit, ScrollHandle, SharedString, Stateful, Svg, TitlebarOptions,
    Window, WindowBounds, WindowOptions, actions, div, img, prelude::*, px, rgb, rgba, size, svg,
};
use input::TextInput;
use state::{ClipboardOp, FileClipboard, Location, Pane, SavedState, ViewMode};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::SystemTime,
};
use theme::{Palette, Preferences};

actions!(
    files,
    [
        MoveUp,
        MoveDown,
        MoveLeft,
        MoveRight,
        SelectFirst,
        SelectLast,
        OpenSelected,
        QuickPreview,
        GoUp,
        GoBack,
        GoForward,
        ViewColumns,
        ViewList,
        ViewGrid,
        FocusPath,
        FocusSearch,
        CopyItem,
        CutItem,
        PasteItem,
        DuplicateItem,
        RenameItem,
        TrashItem,
        DeletePermanent,
        NewFolder,
        TogglePin,
        ToggleHidden,
        ToggleInspector,
        ToggleSplit,
        SwitchPane,
        OpenTerminal,
        OpenContextMenu,
        ToggleHelp,
        Refresh,
        CancelOverlay,
        Quit,
        PromptSubmit,
        PromptCancel,
    ]
);

struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        Ok(Some(Cow::Borrowed(match path {
            "folder" => include_bytes!("../assets/folder.svg"),
            "folder-open" => include_bytes!("../assets/folder-open.svg"),
            "file" => include_bytes!("../assets/file.svg"),
            "file-text" => include_bytes!("../assets/file-text.svg"),
            "file-code" => include_bytes!("../assets/file-code.svg"),
            "file-image" => include_bytes!("../assets/file-image.svg"),
            "file-audio" => include_bytes!("../assets/file-audio.svg"),
            "file-video" => include_bytes!("../assets/file-video.svg"),
            "file-archive" => include_bytes!("../assets/file-archive.svg"),
            "documents" => include_bytes!("../assets/documents.svg"),
            "downloads" => include_bytes!("../assets/downloads.svg"),
            "music" => include_bytes!("../assets/music.svg"),
            "pictures" => include_bytes!("../assets/pictures.svg"),
            "videos" => include_bytes!("../assets/videos.svg"),
            "home" => include_bytes!("../assets/home.svg"),
            "clock" => include_bytes!("../assets/clock.svg"),
            "trash" => include_bytes!("../assets/trash.svg"),
            "drive" => include_bytes!("../assets/drive.svg"),
            "pin" => include_bytes!("../assets/pin.svg"),
            "view-columns" => include_bytes!("../assets/view-columns.svg"),
            "view-list" => include_bytes!("../assets/view-list.svg"),
            "view-grid" => include_bytes!("../assets/view-grid.svg"),
            "sort" => include_bytes!("../assets/sort.svg"),
            "panel-right" => include_bytes!("../assets/panel-right.svg"),
            "split" => include_bytes!("../assets/split.svg"),
            "search" => include_bytes!("../assets/search.svg"),
            "chevron-right" => include_bytes!("../assets/chevron-right.svg"),
            "chevron-down" => include_bytes!("../assets/chevron-down.svg"),
            "open-external" => include_bytes!("../assets/open-external.svg"),
            "arrow-left" => include_bytes!("../assets/arrow-left.svg"),
            "arrow-right" => include_bytes!("../assets/arrow-right.svg"),
            "help" => include_bytes!("../assets/help.svg"),
            "close" => include_bytes!("../assets/close.svg"),
            "folder-plus" => include_bytes!("../assets/folder-plus.svg"),
            "symlink" => include_bytes!("../assets/symlink.svg"),
            _ => return Ok(None),
        })))
    }

    fn list(&self, _: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(vec![])
    }
}

fn elide_end(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        let mut trimmed: String = prefix.chars().take(max_chars.saturating_sub(1)).collect();
        trimmed.push('…');
        trimmed
    } else {
        prefix
    }
}

fn icon(name: &'static str, color: u32, size_px: f32) -> Svg {
    svg().path(name).size(px(size_px)).text_color(rgb(color))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PromptMode {
    Path,
    Search,
    Rename(PathBuf),
    NewFolder(PathBuf),
}

#[derive(Clone, Debug)]
enum Overlay {
    QuickPreview(Entry, Option<String>),
    Help,
    ViewMenu,
    SortMenu,
    ContextMenu(Entry),
    ConfirmDelete(Entry),
}

struct FilesApp {
    focus: FocusHandle,
    home: PathBuf,
    preferences: Preferences,
    prefs_mtime: Option<SystemTime>,
    saved: SavedState,
    sort_field: SortField,
    sort_order: SortOrder,
    panes: Vec<Pane>,
    pane_scrolls: Vec<ScrollHandle>,
    active_pane: usize,
    home_dirs: Vec<SidebarEntry>,
    drives: Vec<SidebarEntry>,
    clipboard: Option<FileClipboard>,
    prompt: Option<PromptMode>,
    prompt_input: Entity<TextInput>,
    overlay: Option<Overlay>,
    status: Option<String>,
    error: Option<String>,
    cached_preview_path: Option<PathBuf>,
    cached_preview_text: Option<String>,
}

impl FilesApp {
    fn new(initial_dir: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let home = fs_model::home_dir();
        let preferences = Preferences::load();
        let prefs_mtime = Preferences::path()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok());
        let saved = SavedState::load();
        let sort_field = SortField::Name;
        let sort_order = SortOrder::Asc;

        let start = if initial_dir.is_dir() {
            initial_dir.clone()
        } else if let Some(parent) = initial_dir.parent().filter(|p| p.is_dir()) {
            parent.to_path_buf()
        } else {
            home.clone()
        };

        let mut primary = Pane::new(start, saved.show_hidden, sort_field, sort_order);
        if initial_dir.is_file()
            && let Some(idx) = primary.entries.iter().position(|e| e.path == initial_dir)
        {
            primary.selected = Some(idx);
        }

        let focus = cx.focus_handle();
        window.focus(&focus);
        window.set_window_title("Files");

        let palette = Palette::for_preferences(&preferences);
        let prompt_input = cx.new(|cx| {
            let mut input = TextInput::with_placeholder("", 4096, cx);
            input.palette = palette;
            input
        });

        let mut app = Self {
            focus,
            home: home.clone(),
            preferences,
            prefs_mtime,
            saved,
            sort_field,
            sort_order,
            panes: vec![primary],
            pane_scrolls: vec![ScrollHandle::new()],
            active_pane: 0,
            home_dirs: fs_model::home_folders(&home),
            drives: fs_model::drives(),
            clipboard: None,
            prompt: None,
            prompt_input,
            overlay: None,
            status: None,
            error: None,
            cached_preview_path: None,
            cached_preview_text: None,
        };
        app.refresh_inspector_preview();
        app.update_title(window);
        log::info!(
            "FILES event=opened path={} view={:?}",
            app.pane().location.title(&app.home),
            app.saved.view_mode
        );
        app
    }

    fn palette(&self) -> Palette {
        Palette::for_preferences(&self.preferences)
    }

    fn pane(&self) -> &Pane {
        &self.panes[self.active_pane]
    }

    fn pane_mut(&mut self) -> &mut Pane {
        &mut self.panes[self.active_pane]
    }

    fn sync_appearance_if_changed(&mut self, cx: &mut Context<Self>) {
        let Some(path) = Preferences::path() else {
            return;
        };
        let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok());
        if mtime != self.prefs_mtime {
            self.prefs_mtime = mtime;
            let loaded = Preferences::load();
            if loaded != self.preferences {
                self.preferences = loaded;
                let palette = self.palette();
                self.prompt_input.update(cx, |i, _| i.palette = palette);
                cx.notify();
            }
        }
    }

    fn update_title(&self, window: &mut Window) {
        let loc = self.pane().location.title(&self.home);
        window.set_window_title(&format!("Files — {loc}"));
    }

    fn scroll_selected_into_view(&self) {
        if let (Some(idx), Some(handle)) = (self.pane().selected, self.pane_scrolls.get(self.active_pane)) {
            handle.scroll_to_item(idx);
        }
    }

    fn refresh_inspector_preview(&mut self) {
        let selected = self.pane().selected_entry().cloned();
        match selected {
            Some(entry) if entry.is_previewable_text() => {
                if self.cached_preview_path.as_ref() != Some(&entry.path) {
                    self.cached_preview_text = fs_model::read_text_preview(&entry.path, 4096, 14);
                    self.cached_preview_path = Some(entry.path);
                }
            }
            _ => {
                self.cached_preview_path = None;
                self.cached_preview_text = None;
            }
        }
    }

    fn clear_notices(&mut self) {
        self.status = None;
        self.error = None;
    }

    fn navigate_active(
        &mut self,
        target: Location,
        select_path: Option<&Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_notices();
        self.overlay = None;
        let show_hidden = self.saved.show_hidden;
        let sf = self.sort_field;
        let so = self.sort_order;
        self.pane_mut().navigate(target, show_hidden, sf, so, select_path);
        self.refresh_inspector_preview();
        self.update_title(window);
        cx.notify();
    }

    fn reload_all(&mut self, prefer_select: Option<&Path>, cx: &mut Context<Self>) {
        let show_hidden = self.saved.show_hidden;
        let sf = self.sort_field;
        let so = self.sort_order;
        for (idx, pane) in self.panes.iter_mut().enumerate() {
            let pick = if idx == self.active_pane { prefer_select } else { None };
            pane.reload(show_hidden, sf, so, pick);
        }
        self.home_dirs = fs_model::home_folders(&self.home);
        self.drives = fs_model::drives();
        self.cached_preview_path = None;
        self.refresh_inspector_preview();
        cx.notify();
    }

    fn open_entry(&mut self, entry: Entry, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane().location.is_trash() {
            self.restore_entry(&entry, cx);
            return;
        }
        if entry.is_dir {
            self.navigate_active(Location::Directory(entry.path), None, window, cx);
        } else {
            self.launch_external(&entry.path, cx);
        }
    }

    fn launch_external(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.clear_notices();
        match Command::new("xdg-open")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => {
                self.status = Some(format!(
                    "Opened {}",
                    path.file_name().and_then(|n| n.to_str()).unwrap_or("file")
                ));
            }
            Err(e) => {
                self.error = Some(format!("Could not open file with xdg-open: {e}"));
            }
        }
        cx.notify();
    }

    fn open_terminal_here(&mut self, cx: &mut Context<Self>) {
        let dir = self
            .pane()
            .location
            .directory()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.home.clone());
        self.clear_notices();
        let spawned = Command::new("foot")
            .arg("-D")
            .arg(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .or_else(|_| {
                Command::new("xdg-terminal-exec")
                    .current_dir(&dir)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            });
        match spawned {
            Ok(_) => self.status = Some("Opened terminal".into()),
            Err(e) => self.error = Some(format!("Could not open terminal: {e}")),
        }
        cx.notify();
    }

    fn open_prompt(
        &mut self,
        mode: PromptMode,
        placeholder: &str,
        initial: &str,
        select_end: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.overlay = None;
        self.clear_notices();
        let palette = self.palette();
        let end = select_end.unwrap_or(initial.len());
        self.prompt_input.update(cx, |input, cx| {
            input.palette = palette;
            input.set_placeholder(placeholder.to_string(), cx);
            if select_end.is_some() {
                input.set_text_selected(initial.to_string(), 0..end, cx);
            } else {
                input.set_text(initial.to_string(), cx);
            }
        });
        self.prompt = Some(mode);
        let handle = self.prompt_input.read(cx).focus_handle(cx);
        window.focus(&handle);
        cx.notify();
    }

    fn close_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prompt = None;
        window.focus(&self.focus);
        cx.notify();
    }

    fn submit_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mode) = self.prompt.take() else {
            return;
        };
        let value = self.prompt_input.read(cx).text();
        window.focus(&self.focus);
        match mode {
            PromptMode::Path => {
                let current = self
                    .pane()
                    .location
                    .directory()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.home.clone());
                let resolved = fs_model::expand_path(&value, &self.home, &current);
                if resolved.is_dir() {
                    self.navigate_active(Location::Directory(resolved), None, window, cx);
                } else if resolved.is_file() {
                    if let Some(parent) = resolved.parent() {
                        self.navigate_active(Location::Directory(parent.to_path_buf()), Some(&resolved), window, cx);
                    }
                } else {
                    self.error = Some(format!("Path does not exist: {}", resolved.display()));
                }
            }
            PromptMode::Search => {
                let q = value.trim().to_string();
                let root = self
                    .pane()
                    .location
                    .directory()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| self.home.clone());
                if q.is_empty() {
                    self.navigate_active(Location::Directory(root), None, window, cx);
                } else {
                    self.navigate_active(Location::Search { root, query: q }, None, window, cx);
                }
            }
            PromptMode::Rename(src) => match fs_model::rename_entry(&src, &value) {
                Ok(new_path) => {
                    self.status = Some(format!(
                        "Renamed to {}",
                        new_path.file_name().and_then(|n| n.to_str()).unwrap_or("")
                    ));
                    self.reload_all(Some(&new_path), cx);
                }
                Err(e) => self.error = Some(e),
            },
            PromptMode::NewFolder(dir) => match fs_model::create_folder(&dir, &value) {
                Ok(created) => {
                    self.status = Some(format!(
                        "Created folder {}",
                        created.file_name().and_then(|n| n.to_str()).unwrap_or("")
                    ));
                    self.reload_all(Some(&created), cx);
                }
                Err(e) => self.error = Some(e),
            },
        }
        cx.notify();
    }

    fn restore_entry(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        self.clear_notices();
        match fs_model::restore_from_trash(&entry.path) {
            Ok(dst) => {
                self.status = Some(format!("Restored to {}", fs_model::display_path(&dst, &self.home)));
                self.reload_all(None, cx);
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    fn trash_selected(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.pane().selected_entry().cloned() else {
            return;
        };
        self.clear_notices();
        if self.pane().location.is_trash() {
            self.overlay = Some(Overlay::ConfirmDelete(entry));
            cx.notify();
            return;
        }
        match fs_model::move_to_trash(&entry.path) {
            Ok(_) => {
                self.status = Some(format!("Moved “{}” to Trash", entry.name));
                self.reload_all(None, cx);
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    fn confirm_delete_permanent(&mut self, entry: &Entry, cx: &mut Context<Self>) {
        self.overlay = None;
        self.clear_notices();
        match fs_model::delete_permanently(&entry.path) {
            Ok(()) => {
                self.status = Some(format!("Deleted “{}” permanently", entry.name));
                self.reload_all(None, cx);
            }
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    fn set_view_mode(&mut self, mode: ViewMode, cx: &mut Context<Self>) {
        self.saved.view_mode = mode;
        self.saved.save();
        self.overlay = None;
        cx.notify();
    }

    fn set_sort(&mut self, field: SortField, cx: &mut Context<Self>) {
        if self.sort_field == field {
            self.sort_order = self.sort_order.toggle();
        } else {
            self.sort_field = field;
            self.sort_order = match field {
                SortField::Modified | SortField::Size => SortOrder::Desc,
                SortField::Name | SortField::Kind => SortOrder::Asc,
            };
        }
        self.reload_all(None, cx);
    }

    fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.saved.show_hidden = !self.saved.show_hidden;
        self.saved.save();
        self.reload_all(None, cx);
    }

    fn toggle_inspector(&mut self, cx: &mut Context<Self>) {
        self.saved.show_inspector = !self.saved.show_inspector;
        self.saved.save();
        cx.notify();
    }

    fn toggle_split(&mut self, cx: &mut Context<Self>) {
        if self.panes.len() > 1 {
            let keep = self.panes.remove(self.active_pane);
            let keep_scroll = self.pane_scrolls.remove(self.active_pane);
            self.panes = vec![keep];
            self.pane_scrolls = vec![keep_scroll];
            self.active_pane = 0;
        } else {
            let dir = self
                .pane()
                .location
                .directory()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.home.clone());
            let second = Pane::new(dir, self.saved.show_hidden, self.sort_field, self.sort_order);
            self.panes.push(second);
            self.pane_scrolls.push(ScrollHandle::new());
            self.active_pane = 1;
        }
        self.refresh_inspector_preview();
        cx.notify();
    }

    fn toggle_pin_current(&mut self, target_dir: Option<PathBuf>, cx: &mut Context<Self>) {
        let dir = target_dir.or_else(|| {
            self.pane()
                .selected_entry()
                .filter(|e| e.is_dir)
                .map(|e| e.path.clone())
                .or_else(|| self.pane().location.directory().map(Path::to_path_buf))
        });
        let Some(dir) = dir else {
            return;
        };
        let pinned = self.saved.toggle_pin(&dir);
        self.saved.save();
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("folder");
        self.status = Some(if pinned {
            format!("Pinned “{name}”")
        } else {
            format!("Unpinned “{name}”")
        });
        cx.notify();
    }

    fn paste_into_active(&mut self, cx: &mut Context<Self>) {
        let Some(clip) = self.clipboard.clone() else {
            return;
        };
        let Some(dst_dir) = self.pane().location.directory().map(Path::to_path_buf) else {
            self.error = Some("Switch to a folder to paste here.".into());
            cx.notify();
            return;
        };
        self.clear_notices();
        let result = match clip.op {
            ClipboardOp::Copy => fs_model::copy_entry(&clip.path, &dst_dir),
            ClipboardOp::Cut => fs_model::move_entry(&clip.path, &dst_dir),
        };
        match result {
            Ok(dest) => {
                if clip.op == ClipboardOp::Cut {
                    self.clipboard = None;
                }
                self.status = Some(format!(
                    "{} {}",
                    if clip.op == ClipboardOp::Copy {
                        "Copied"
                    } else {
                        "Moved"
                    },
                    dest.file_name().and_then(|n| n.to_str()).unwrap_or("")
                ));
                self.reload_all(Some(&dest), cx);
            }
            Err(e) => {
                self.error = Some(e);
                cx.notify();
            }
        }
    }

    fn open_quick_preview(&mut self, cx: &mut Context<Self>) {
        if matches!(self.overlay, Some(Overlay::QuickPreview(_, _))) {
            self.overlay = None;
            cx.notify();
            return;
        }
        let Some(entry) = self.pane().selected_entry().cloned() else {
            return;
        };
        let preview_text = if entry.is_dir {
            fs_model::read_directory(&entry.path, self.saved.show_hidden, SortField::Name, SortOrder::Asc)
                .ok()
                .map(|items| {
                    if items.is_empty() {
                        "Empty folder".to_string()
                    } else {
                        items
                            .iter()
                            .take(28)
                            .map(|e| {
                                if e.is_dir {
                                    format!("{}/", e.name)
                                } else {
                                    format!("{}  ({})", e.name, fs_model::format_size(e.size))
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                })
        } else if entry.is_previewable_text() {
            fs_model::read_text_preview(&entry.path, 65_536, 80)
        } else {
            None
        };
        self.overlay = Some(Overlay::QuickPreview(entry, preview_text));
        cx.notify();
    }

    // ── Action handlers ───────────────────────────────────────────────────────

    fn on_move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() {
            return;
        }
        let step = if self.saved.view_mode == ViewMode::Grid { -4 } else { -1 };
        self.pane_mut().move_selection(step);
        self.refresh_inspector_preview();
        self.scroll_selected_into_view();
        cx.notify();
    }

    fn on_move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() {
            return;
        }
        let step = if self.saved.view_mode == ViewMode::Grid { 4 } else { 1 };
        self.pane_mut().move_selection(step);
        self.refresh_inspector_preview();
        self.scroll_selected_into_view();
        cx.notify();
    }

    fn on_move_left(&mut self, _: &MoveLeft, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() {
            return;
        }
        if self.saved.view_mode == ViewMode::Grid {
            self.pane_mut().move_selection(-1);
            self.refresh_inspector_preview();
            self.scroll_selected_into_view();
            cx.notify();
        } else {
            let show_hidden = self.saved.show_hidden;
            let sf = self.sort_field;
            let so = self.sort_order;
            if self.pane_mut().go_up(show_hidden, sf, so) {
                self.refresh_inspector_preview();
                self.update_title(window);
                cx.notify();
            }
        }
    }

    fn on_move_right(&mut self, _: &MoveRight, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() {
            return;
        }
        if self.saved.view_mode == ViewMode::Grid {
            self.pane_mut().move_selection(1);
            self.refresh_inspector_preview();
            self.scroll_selected_into_view();
            cx.notify();
        } else if let Some(entry) = self.pane().selected_entry().cloned()
            && entry.is_dir
        {
            self.navigate_active(Location::Directory(entry.path), None, window, cx);
        }
    }

    fn on_select_first(&mut self, _: &SelectFirst, _: &mut Window, cx: &mut Context<Self>) {
        self.pane_mut().select_first();
        self.refresh_inspector_preview();
        self.scroll_selected_into_view();
        cx.notify();
    }

    fn on_select_last(&mut self, _: &SelectLast, _: &mut Window, cx: &mut Context<Self>) {
        self.pane_mut().select_last();
        self.refresh_inspector_preview();
        self.scroll_selected_into_view();
        cx.notify();
    }

    fn on_open_selected(&mut self, _: &OpenSelected, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Overlay::ConfirmDelete(entry)) = self.overlay.clone() {
            self.confirm_delete_permanent(&entry, cx);
            return;
        }
        if self.overlay.is_some() {
            return;
        }
        if let Some(entry) = self.pane().selected_entry().cloned() {
            self.open_entry(entry, window, cx);
        }
    }

    fn on_go_up(&mut self, _: &GoUp, window: &mut Window, cx: &mut Context<Self>) {
        let show_hidden = self.saved.show_hidden;
        let sf = self.sort_field;
        let so = self.sort_order;
        if self.pane_mut().go_up(show_hidden, sf, so) {
            self.overlay = None;
            self.refresh_inspector_preview();
            self.update_title(window);
            cx.notify();
        }
    }

    fn on_go_back(&mut self, _: &GoBack, window: &mut Window, cx: &mut Context<Self>) {
        let show_hidden = self.saved.show_hidden;
        let sf = self.sort_field;
        let so = self.sort_order;
        if self.pane_mut().go_back(show_hidden, sf, so) {
            self.overlay = None;
            self.refresh_inspector_preview();
            self.update_title(window);
            cx.notify();
        }
    }

    fn on_go_forward(&mut self, _: &GoForward, window: &mut Window, cx: &mut Context<Self>) {
        let show_hidden = self.saved.show_hidden;
        let sf = self.sort_field;
        let so = self.sort_order;
        if self.pane_mut().go_forward(show_hidden, sf, so) {
            self.overlay = None;
            self.refresh_inspector_preview();
            self.update_title(window);
            cx.notify();
        }
    }

    fn on_focus_path(&mut self, _: &FocusPath, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .pane()
            .location
            .directory()
            .map(|p| fs_model::display_path(p, &self.home))
            .unwrap_or_else(|| "~".into());
        self.open_prompt(
            PromptMode::Path,
            "Type a path…",
            &current,
            Some(current.len()),
            window,
            cx,
        );
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        let existing = match &self.pane().location {
            Location::Search { query, .. } => query.clone(),
            _ => String::new(),
        };
        self.open_prompt(
            PromptMode::Search,
            "Search here and below…",
            &existing,
            None,
            window,
            cx,
        );
    }

    fn on_rename(&mut self, _: &RenameItem, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane().location.is_trash() {
            return;
        }
        let Some(entry) = self.pane().selected_entry().cloned() else {
            return;
        };
        let stem_end = fs_model::rename_stem_end(&entry.name, entry.is_dir);
        self.open_prompt(
            PromptMode::Rename(entry.path),
            "New name",
            &entry.name,
            Some(stem_end),
            window,
            cx,
        );
    }

    fn on_new_folder(&mut self, _: &NewFolder, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dir) = self.pane().location.directory().map(Path::to_path_buf) else {
            return;
        };
        self.open_prompt(
            PromptMode::NewFolder(dir),
            "New folder name",
            "New folder",
            Some("New folder".len()),
            window,
            cx,
        );
    }

    fn on_copy(&mut self, _: &CopyItem, _: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.pane().selected_entry().cloned() else {
            return;
        };
        self.clipboard = Some(FileClipboard {
            op: ClipboardOp::Copy,
            path: entry.path,
        });
        self.status = Some(format!("Copied “{}”", entry.name));
        cx.notify();
    }

    fn on_cut(&mut self, _: &CutItem, _: &mut Window, cx: &mut Context<Self>) {
        if self.pane().location.is_trash() {
            return;
        }
        let Some(entry) = self.pane().selected_entry().cloned() else {
            return;
        };
        self.clipboard = Some(FileClipboard {
            op: ClipboardOp::Cut,
            path: entry.path,
        });
        self.status = Some(format!("Cut “{}”", entry.name));
        cx.notify();
    }

    fn on_duplicate(&mut self, _: &DuplicateItem, _: &mut Window, cx: &mut Context<Self>) {
        if self.pane().location.is_trash() {
            return;
        }
        let Some(entry) = self.pane().selected_entry().cloned() else {
            return;
        };
        self.clear_notices();
        match fs_model::duplicate_entry(&entry.path) {
            Ok(dup) => {
                self.status = Some(format!(
                    "Duplicated as “{}”",
                    dup.file_name().and_then(|n| n.to_str()).unwrap_or("")
                ));
                self.reload_all(Some(&dup), cx);
            }
            Err(e) => {
                self.error = Some(e);
                cx.notify();
            }
        }
    }

    fn on_cancel_overlay(&mut self, _: &CancelOverlay, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay.is_some() {
            self.overlay = None;
            cx.notify();
        } else if self.prompt.is_some() {
            self.close_prompt(window, cx);
        } else if matches!(self.pane().location, Location::Search { .. }) {
            let root = self
                .pane()
                .location
                .directory()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.home.clone());
            self.navigate_active(Location::Directory(root), None, window, cx);
        } else if self.clipboard.is_some() || self.status.is_some() || self.error.is_some() {
            self.clipboard = None;
            self.clear_notices();
            cx.notify();
        }
    }

    // ── Rendering helpers ─────────────────────────────────────────────────────

    fn render_header(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let loc = self.pane().location.clone();
        let can_back = !self.pane().back.is_empty();
        let can_fwd = !self.pane().forward.is_empty();
        let in_header_prompt = matches!(self.prompt, Some(PromptMode::Path | PromptMode::Search));
        let view_open = matches!(self.overlay, Some(Overlay::ViewMenu));
        let sort_open = matches!(self.overlay, Some(Overlay::SortMenu));

        let left_area: AnyElement = if in_header_prompt {
            let prefix_icon = match self.prompt {
                Some(PromptMode::Search) => "search",
                _ => "folder",
            };
            div()
                .flex_1()
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(28.))
                .px(px(10.))
                .rounded(px(4.))
                .bg(rgb(p.surface))
                .border_1()
                .border_color(rgb(p.focus))
                .key_context("FilesPrompt")
                .on_action(cx.listener(|this, _: &PromptSubmit, w, cx| this.submit_prompt(w, cx)))
                .on_action(cx.listener(|this, _: &PromptCancel, w, cx| this.close_prompt(w, cx)))
                .child(icon(prefix_icon, p.muted, 13.5))
                .child(
                    div()
                        .flex_1()
                        .font_family("IBM Plex Mono")
                        .text_size(px(12.5))
                        .text_color(rgb(p.text))
                        .child(self.prompt_input.clone()),
                )
                .into_any_element()
        } else {
            let mut crumbs = div()
                .id("breadcrumb-bar")
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .gap(px(4.))
                .overflow_hidden()
                .on_click(cx.listener(|this, _, w, cx| {
                    this.on_focus_path(&FocusPath, w, cx);
                }));

            if can_back || can_fwd {
                crumbs = crumbs
                    .child(
                        div()
                            .id("nav-back")
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .when(can_back, |d| {
                                d.cursor_pointer()
                                    .hover(move |s| s.bg(rgb(p.surface)))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        cx.stop_propagation();
                                        this.on_go_back(&GoBack, w, cx);
                                    }))
                            })
                            .child(icon("arrow-left", if can_back { p.muted } else { p.line }, 13.)),
                    )
                    .child(
                        div()
                            .id("nav-fwd")
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .when(can_fwd, |d| {
                                d.cursor_pointer()
                                    .hover(move |s| s.bg(rgb(p.surface)))
                                    .on_click(cx.listener(|this, _, w, cx| {
                                        cx.stop_propagation();
                                        this.on_go_forward(&GoForward, w, cx);
                                    }))
                            })
                            .child(icon("arrow-right", if can_fwd { p.muted } else { p.line }, 13.)),
                    );
            }

            match &loc {
                Location::Directory(dir) => {
                    let parts = fs_model::breadcrumbs(dir, &self.home);
                    let total = parts.len();
                    for (idx, (label, path)) in parts.into_iter().enumerate() {
                        let is_last = idx + 1 == total;
                        if idx > 0 {
                            crumbs = crumbs.child(div().text_size(px(12.)).text_color(rgb(p.muted)).child("/"));
                        }
                        crumbs = crumbs.child(
                            div()
                                .id(("crumb", idx))
                                .px(px(6.))
                                .py(px(3.))
                                .rounded(px(4.))
                                .cursor_pointer()
                                .font_family("IBM Plex Mono")
                                .text_size(px(12.5))
                                .text_color(rgb(if is_last { p.text } else { p.muted }))
                                .hover(move |s| s.bg(rgb(p.surface)).text_color(rgb(p.text)))
                                .child(label)
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    cx.stop_propagation();
                                    this.navigate_active(Location::Directory(path.clone()), None, w, cx);
                                })),
                        );
                    }
                }
                Location::Recents => {
                    crumbs = crumbs.child(
                        div()
                            .px(px(6.))
                            .font_family("IBM Plex Mono")
                            .text_size(px(12.5))
                            .text_color(rgb(p.text))
                            .child("Recents"),
                    );
                }
                Location::Trash => {
                    crumbs = crumbs.child(
                        div()
                            .px(px(6.))
                            .font_family("IBM Plex Mono")
                            .text_size(px(12.5))
                            .text_color(rgb(p.text))
                            .child("Trash"),
                    );
                }
                Location::Search { query, root } => {
                    let root_clone = root.clone();
                    crumbs = crumbs
                        .child(
                            div()
                                .px(px(6.))
                                .font_family("IBM Plex Mono")
                                .text_size(px(12.5))
                                .text_color(rgb(p.signal_fg))
                                .child(format!("Search “{query}”")),
                        )
                        .child(
                            div()
                                .id("clear-search")
                                .px(px(6.))
                                .py(px(2.))
                                .rounded(px(4.))
                                .bg(rgb(p.surface))
                                .text_size(px(11.))
                                .text_color(rgb(p.muted))
                                .cursor_pointer()
                                .child("Clear")
                                .on_click(cx.listener(move |this, _, w, cx| {
                                    cx.stop_propagation();
                                    this.navigate_active(Location::Directory(root_clone.clone()), None, w, cx);
                                })),
                        );
                }
            }
            crumbs.into_any_element()
        };

        let header_btn = |id: &'static str, ic: &'static str, active: bool| {
            div()
                .id(id)
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .cursor_pointer()
                .when(active, |d| d.bg(rgb(p.surface)).border_1().border_color(rgb(p.line)))
                .hover(move |s| s.bg(rgb(p.surface)))
                .child(icon(ic, if active { p.text } else { p.muted }, 15.))
        };

        div()
            .h(px(42.))
            .flex_shrink_0()
            .px(px(12.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .bg(rgb(p.canvas))
            .border_b_1()
            .border_color(rgb(p.line))
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 1 {
                    window.start_window_move();
                }
            })
            .child(left_area)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(
                        header_btn("btn-view-mode", self.saved.view_mode.icon(), view_open).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.overlay = if matches!(this.overlay, Some(Overlay::ViewMenu)) {
                                    None
                                } else {
                                    Some(Overlay::ViewMenu)
                                };
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        header_btn("btn-sort", "sort", sort_open).on_click(cx.listener(|this, _, _, cx| {
                            this.overlay = if matches!(this.overlay, Some(Overlay::SortMenu)) {
                                None
                            } else {
                                Some(Overlay::SortMenu)
                            };
                            cx.notify();
                        })),
                    )
                    .child(
                        header_btn("btn-inspector", "panel-right", self.saved.show_inspector).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.toggle_inspector(cx);
                            },
                        )),
                    ),
            )
    }

    fn render_sidebar(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let active_loc = self.pane().location.clone();

        let section_header = |label: &'static str, accent: bool| {
            div()
                .px(px(12.))
                .pt(px(14.))
                .pb(px(6.))
                .font_family("IBM Plex Mono")
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(if accent { p.signal_fg } else { p.muted }))
                .child(label)
        };

        let mut list = div()
            .id("sidebar-scroll")
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .pb(px(12.));

        // PINNED section (shown when user has pinned folders)
        if !self.saved.pinned.is_empty() {
            list = list.child(section_header("PINNED", true));
            for (idx, pin_path) in self.saved.pinned.iter().enumerate() {
                let label = pin_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(String::from)
                    .unwrap_or_else(|| pin_path.display().to_string());
                let is_active = active_loc == Location::Directory(pin_path.clone());
                let target = pin_path.clone();
                list = list.child(
                    div()
                        .id(("pin-item", idx))
                        .mx(px(6.))
                        .px(px(8.))
                        .h(px(27.))
                        .flex()
                        .items_center()
                        .gap(px(9.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(is_active, |d| d.bg(rgb(p.surface)))
                        .hover(move |s| s.bg(rgb(p.surface)))
                        .child(icon("pin", if is_active { p.signal_fg } else { p.muted }, 13.5))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .font_family("IBM Plex Mono")
                                .text_size(px(12.))
                                .text_color(rgb(if is_active { p.text } else { p.muted }))
                                .child(label),
                        )
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.navigate_active(Location::Directory(target.clone()), None, w, cx);
                        })),
                );
            }
        }

        // HOME section — clicking HOME navigates to ~
        let home_target = self.home.clone();
        list = list.child(
            div()
                .id("sidebar-home-header")
                .px(px(12.))
                .pt(px(14.))
                .pb(px(6.))
                .cursor_pointer()
                .font_family("IBM Plex Mono")
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(p.signal_fg))
                .hover(move |s| s.text_color(rgb(p.text)))
                .child("HOME")
                .on_click(cx.listener(move |this, _, w, cx| {
                    this.navigate_active(Location::Directory(home_target.clone()), None, w, cx);
                })),
        );

        for (idx, item) in self.home_dirs.iter().enumerate() {
            let is_active = active_loc == Location::Directory(item.path.clone());
            let target = item.path.clone();
            list = list.child(
                div()
                    .id(("home-dir", idx))
                    .mx(px(6.))
                    .px(px(8.))
                    .h(px(27.))
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(is_active, |d| d.bg(rgb(p.surface)))
                    .hover(move |s| s.bg(rgb(p.surface)))
                    .child(icon(item.icon, if is_active { p.signal_fg } else { p.muted }, 14.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_family("IBM Plex Mono")
                            .text_size(px(12.))
                            .text_color(rgb(if is_active { p.text } else { p.muted }))
                            .child(elide_end(&item.label, 17)),
                    )
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.navigate_active(Location::Directory(target.clone()), None, w, cx);
                    })),
            );
        }

        // VIEWS section (Recents, Trash)
        list = list.child(section_header("VIEWS", false));
        let views: [(&'static str, &'static str, Location); 2] = [
            ("Recents", "clock", Location::Recents),
            ("Trash", "trash", Location::Trash),
        ];
        for (idx, (label, ic, target)) in views.into_iter().enumerate() {
            let is_active = active_loc == target;
            list = list.child(
                div()
                    .id(("view-item", idx))
                    .mx(px(6.))
                    .px(px(8.))
                    .h(px(27.))
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(is_active, |d| d.bg(rgb(p.surface)))
                    .hover(move |s| s.bg(rgb(p.surface)))
                    .child(icon(ic, if is_active { p.signal_fg } else { p.muted }, 14.))
                    .child(
                        div()
                            .flex_1()
                            .font_family("IBM Plex Mono")
                            .text_size(px(12.))
                            .text_color(rgb(if is_active { p.text } else { p.muted }))
                            .child(label),
                    )
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.navigate_active(target.clone(), None, w, cx);
                    })),
            );
        }

        // DRIVES section
        list = list.child(section_header("DRIVES", false));
        for (idx, drive) in self.drives.iter().enumerate() {
            let is_active = active_loc == Location::Directory(drive.path.clone());
            let target = drive.path.clone();
            list = list.child(
                div()
                    .id(("drive-item", idx))
                    .mx(px(6.))
                    .px(px(8.))
                    .h(px(27.))
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(is_active, |d| d.bg(rgb(p.surface)))
                    .hover(move |s| s.bg(rgb(p.surface)))
                    .child(icon(drive.icon, if is_active { p.signal_fg } else { p.muted }, 14.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_family("IBM Plex Mono")
                            .text_size(px(12.))
                            .text_color(rgb(if is_active { p.text } else { p.muted }))
                            .child(drive.label.clone()),
                    )
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.navigate_active(Location::Directory(target.clone()), None, w, cx);
                    })),
            );
        }

        // Bottom-left `?` shortcuts help button
        let help_open = matches!(self.overlay, Some(Overlay::Help));
        div()
            .w(px(204.))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(p.canvas))
            .border_r_1()
            .border_color(rgb(p.line))
            .child(list)
            .child(
                div()
                    .h(px(36.))
                    .flex_shrink_0()
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_t_1()
                    .border_color(rgb(p.line))
                    .child(
                        div()
                            .id("btn-help")
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .cursor_pointer()
                            .when(help_open, |d| d.bg(rgb(p.surface)))
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(icon("help", if help_open { p.text } else { p.muted }, 15.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.overlay = if matches!(this.overlay, Some(Overlay::Help)) {
                                    None
                                } else {
                                    Some(Overlay::Help)
                                };
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .id("btn-new-folder")
                            .size(px(24.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(icon("folder-plus", p.muted, 14.5))
                            .on_click(cx.listener(|this, _, w, cx| {
                                this.on_new_folder(&NewFolder, w, cx);
                            })),
                    ),
            )
    }

    fn render_entry_icon(&self, entry: &Entry, p: Palette, selected: bool, size_px: f32) -> Div {
        if entry.is_previewable_image() {
            div()
                .size(px(size_px + 2.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .rounded(px(2.))
                .child(
                    img(entry.path.clone())
                        .size(px(size_px + 2.))
                        .object_fit(ObjectFit::Cover),
                )
        } else {
            let color = if entry.is_dir || selected { p.signal_fg } else { p.muted };
            div()
                .size(px(size_px + 2.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(icon(entry.icon_name(), color, size_px))
        }
    }

    fn render_list_pane(&self, pane_idx: usize, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let pane = &self.panes[pane_idx];
        let is_active_pane = pane_idx == self.active_pane;

        if let Some(err) = &pane.error {
            return div()
                .flex_1()
                .p(px(24.))
                .font_family("IBM Plex Mono")
                .text_size(px(12.5))
                .text_color(rgb(p.muted))
                .child(err.clone())
                .into_any_element();
        }
        if pane.entries.is_empty() {
            let msg = match &pane.location {
                Location::Trash => "Trash is empty.",
                Location::Search { .. } => "No matching files found.",
                Location::Recents => "No recent files.",
                Location::Directory(_) => "This folder is empty.",
            };
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .font_family("IBM Plex Mono")
                .text_size(px(12.5))
                .text_color(rgb(p.muted))
                .child(msg)
                .into_any_element();
        }

        let mut list = div()
            .id(("pane-list", pane_idx))
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.pane_scrolls[pane_idx])
            .px(px(12.))
            .py(px(10.))
            .flex()
            .flex_col()
            .gap(px(1.));

        for (row_idx, entry) in pane.entries.iter().enumerate() {
            let selected = is_active_pane && pane.selected == Some(row_idx);
            let is_cut = self
                .clipboard
                .as_ref()
                .is_some_and(|c| c.op == ClipboardOp::Cut && c.path == entry.path);
            let meta_col = if entry.is_dir {
                fs_model::format_item_count(entry.child_count)
            } else {
                fs_model::format_size(entry.size)
            };
            let date_col = fs_model::format_short_date(entry.modified);
            let entry_open = entry.clone();
            let entry_ctx = entry.clone();

            list = list.child(
                div()
                    .id(("row", pane_idx * 100_000 + row_idx))
                    .h(px(30.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(rgba(p.selection_bg)))
                    .when(!selected, |d| d.hover(move |s| s.bg(rgb(p.surface))))
                    .when(is_cut, |d| d.opacity(0.5))
                    .child(self.render_entry_icon(entry, p, selected, 15.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_family("IBM Plex Mono")
                            .text_size(px(12.5))
                            .text_color(rgb(if selected { p.signal_fg } else { p.text }))
                            .child(entry.name.clone()),
                    )
                    .child(
                        div()
                            .w(px(86.))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .font_family("IBM Plex Mono")
                            .text_size(px(11.5))
                            .text_color(rgb(p.muted))
                            .child(meta_col),
                    )
                    .child(
                        div()
                            .w(px(72.))
                            .flex_shrink_0()
                            .flex()
                            .justify_end()
                            .font_family("IBM Plex Mono")
                            .text_size(px(11.5))
                            .text_color(rgb(p.muted))
                            .child(date_col),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, w, cx| {
                            this.active_pane = pane_idx;
                            this.overlay = None;
                            this.panes[pane_idx].selected = Some(row_idx);
                            this.refresh_inspector_preview();
                            if event.click_count >= 2 {
                                this.open_entry(entry_open.clone(), w, cx);
                            } else {
                                cx.notify();
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.active_pane = pane_idx;
                            this.panes[pane_idx].selected = Some(row_idx);
                            this.refresh_inspector_preview();
                            this.overlay = Some(Overlay::ContextMenu(entry_ctx.clone()));
                            cx.notify();
                        }),
                    ),
            );
        }
        list.into_any_element()
    }

    fn render_grid_pane(&self, pane_idx: usize, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let pane = &self.panes[pane_idx];
        let is_active_pane = pane_idx == self.active_pane;

        if pane.entries.is_empty() {
            return self.render_list_pane(pane_idx, p, cx);
        }

        let mut grid = div()
            .id(("pane-grid", pane_idx))
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .track_scroll(&self.pane_scrolls[pane_idx])
            .p(px(14.))
            .flex()
            .flex_wrap()
            .content_start()
            .gap(px(10.));

        for (idx, entry) in pane.entries.iter().enumerate() {
            let selected = is_active_pane && pane.selected == Some(idx);
            let entry_open = entry.clone();
            let entry_ctx = entry.clone();
            let thumb = if entry.is_previewable_image() {
                div()
                    .w(px(76.))
                    .h(px(58.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .rounded(px(4.))
                    .child(
                        img(entry.path.clone())
                            .max_w(px(76.))
                            .max_h(px(58.))
                            .object_fit(ObjectFit::Contain),
                    )
            } else {
                div()
                    .w(px(76.))
                    .h(px(58.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(
                        entry.icon_name(),
                        if entry.is_dir { p.signal_fg } else { p.muted },
                        40.,
                    ))
            };

            grid = grid.child(
                div()
                    .id(("grid-tile", pane_idx * 100_000 + idx))
                    .w(px(124.))
                    .h(px(106.))
                    .p(px(8.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_between()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(selected, |d| {
                        d.bg(rgba(p.selection_bg)).border_1().border_color(rgb(p.focus))
                    })
                    .when(!selected, |d| {
                        d.border_1()
                            .border_color(rgb(p.canvas))
                            .hover(move |s| s.bg(rgb(p.surface)))
                    })
                    .child(thumb)
                    .child(
                        div()
                            .w_full()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .flex()
                            .justify_center()
                            .font_family("IBM Plex Mono")
                            .text_size(px(11.5))
                            .text_color(rgb(if selected { p.signal_fg } else { p.text }))
                            .child(elide_end(&entry.name, 15)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, w, cx| {
                            this.active_pane = pane_idx;
                            this.overlay = None;
                            this.panes[pane_idx].selected = Some(idx);
                            this.refresh_inspector_preview();
                            if event.click_count >= 2 {
                                this.open_entry(entry_open.clone(), w, cx);
                            } else {
                                cx.notify();
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.active_pane = pane_idx;
                            this.panes[pane_idx].selected = Some(idx);
                            this.refresh_inspector_preview();
                            this.overlay = Some(Overlay::ContextMenu(entry_ctx.clone()));
                            cx.notify();
                        }),
                    ),
            );
        }
        grid.into_any_element()
    }

    fn render_columns_pane(&self, pane_idx: usize, p: Palette, cx: &mut Context<Self>) -> AnyElement {
        let pane = &self.panes[pane_idx];
        let Location::Directory(current_dir) = &pane.location else {
            return self.render_list_pane(pane_idx, p, cx);
        };

        // Build Miller columns: up to 2 ancestor columns + current directory + selected child folder preview.
        let mut ancestors: Vec<PathBuf> = Vec::new();
        let mut cursor = current_dir.as_path();
        if current_dir != &self.home {
            while let Some(parent) = cursor.parent() {
                if parent == cursor || ancestors.len() >= 2 {
                    break;
                }
                ancestors.push(parent.to_path_buf());
                if parent == self.home {
                    break;
                }
                cursor = parent;
            }
        }
        ancestors.reverse();

        let mut strip = div()
            .id(("pane-cols", pane_idx))
            .flex_1()
            .min_h(px(0.))
            .flex()
            .overflow_x_scroll();

        // Ancestor columns
        for (col_i, anc_dir) in ancestors.iter().enumerate() {
            let next_in_chain = ancestors.get(col_i + 1).cloned().unwrap_or_else(|| current_dir.clone());
            let entries = fs_model::read_directory(anc_dir, self.saved.show_hidden, self.sort_field, self.sort_order)
                .unwrap_or_default();
            let mut col = div()
                .id(("anc-col", pane_idx * 100 + col_i))
                .w(px(210.))
                .flex_shrink_0()
                .h_full()
                .overflow_y_scroll()
                .p(px(6.))
                .border_r_1()
                .border_color(rgb(p.line))
                .flex()
                .flex_col()
                .gap(px(1.));
            for (i, entry) in entries.into_iter().enumerate() {
                let is_on_path = entry.path == next_in_chain;
                let target = entry.clone();
                col = col.child(
                    div()
                        .id(("anc-row", col_i * 10_000 + i))
                        .h(px(27.))
                        .px(px(8.))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .rounded(px(4.))
                        .cursor_pointer()
                        .when(is_on_path, |d| d.bg(rgb(p.surface)))
                        .hover(move |s| s.bg(rgb(p.surface)))
                        .child(self.render_entry_icon(&entry, p, is_on_path, 14.))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .font_family("IBM Plex Mono")
                                .text_size(px(12.))
                                .text_color(rgb(if is_on_path { p.text } else { p.muted }))
                                .child(entry.name),
                        )
                        .when(entry.is_dir, |d| d.child(icon("chevron-right", p.muted, 12.)))
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.active_pane = pane_idx;
                            if target.is_dir {
                                this.navigate_active(Location::Directory(target.path.clone()), None, w, cx);
                            }
                        })),
                );
            }
            strip = strip.child(col);
        }

        // Current directory column
        let is_active_pane = pane_idx == self.active_pane;
        let mut active_col = div()
            .id(("cur-col", pane_idx))
            .w(px(240.))
            .flex_shrink_0()
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.pane_scrolls[pane_idx])
            .p(px(6.))
            .border_r_1()
            .border_color(rgb(p.line))
            .flex()
            .flex_col()
            .gap(px(1.));

        for (row_idx, entry) in pane.entries.iter().enumerate() {
            let selected = is_active_pane && pane.selected == Some(row_idx);
            let entry_open = entry.clone();
            let entry_ctx = entry.clone();
            active_col = active_col.child(
                div()
                    .id(("col-row", pane_idx * 100_000 + row_idx))
                    .h(px(27.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(selected, |d| d.bg(rgba(p.selection_bg)))
                    .when(!selected, |d| d.hover(move |s| s.bg(rgb(p.surface))))
                    .child(self.render_entry_icon(entry, p, selected, 14.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_family("IBM Plex Mono")
                            .text_size(px(12.))
                            .text_color(rgb(if selected { p.signal_fg } else { p.text }))
                            .child(entry.name.clone()),
                    )
                    .when(entry.is_dir, |d| {
                        d.child(icon("chevron-right", if selected { p.signal_fg } else { p.muted }, 12.))
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, w, cx| {
                            this.active_pane = pane_idx;
                            this.overlay = None;
                            this.panes[pane_idx].selected = Some(row_idx);
                            this.refresh_inspector_preview();
                            if event.click_count >= 2 {
                                this.open_entry(entry_open.clone(), w, cx);
                            } else {
                                cx.notify();
                            }
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.active_pane = pane_idx;
                            this.panes[pane_idx].selected = Some(row_idx);
                            this.refresh_inspector_preview();
                            this.overlay = Some(Overlay::ContextMenu(entry_ctx.clone()));
                            cx.notify();
                        }),
                    ),
            );
        }
        strip = strip.child(active_col);

        // Selected subfolder preview column
        if let Some(sel) = pane.selected_entry().filter(|e| e.is_dir) {
            let child_entries =
                fs_model::read_directory(&sel.path, self.saved.show_hidden, self.sort_field, self.sort_order)
                    .unwrap_or_default();
            let mut next_col = div()
                .id(("next-col", pane_idx))
                .w(px(230.))
                .flex_shrink_0()
                .h_full()
                .overflow_y_scroll()
                .p(px(6.))
                .flex()
                .flex_col()
                .gap(px(1.));
            if child_entries.is_empty() {
                next_col = next_col.child(
                    div()
                        .p(px(12.))
                        .font_family("IBM Plex Mono")
                        .text_size(px(11.5))
                        .text_color(rgb(p.muted))
                        .child("No items"),
                );
            } else {
                let parent_dir = sel.path.clone();
                for (i, entry) in child_entries.into_iter().take(200).enumerate() {
                    let select_child = entry.path.clone();
                    let nav_dir = parent_dir.clone();
                    next_col = next_col.child(
                        div()
                            .id(("next-row", i))
                            .h(px(27.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(move |s| s.bg(rgb(p.surface)))
                            .child(self.render_entry_icon(&entry, p, false, 14.))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .font_family("IBM Plex Mono")
                                    .text_size(px(12.))
                                    .text_color(rgb(p.muted))
                                    .child(entry.name),
                            )
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.active_pane = pane_idx;
                                this.navigate_active(Location::Directory(nav_dir.clone()), Some(&select_child), w, cx);
                            })),
                    );
                }
            }
            strip = strip.child(next_col);
        }

        strip.into_any_element()
    }

    fn render_pane(&self, pane_idx: usize, p: Palette, cx: &mut Context<Self>) -> Div {
        let is_split = self.panes.len() > 1;
        let is_active = pane_idx == self.active_pane;
        let content = match self.saved.view_mode {
            ViewMode::List => self.render_list_pane(pane_idx, p, cx),
            ViewMode::Grid => self.render_grid_pane(pane_idx, p, cx),
            ViewMode::Columns => self.render_columns_pane(pane_idx, p, cx),
        };
        let mut wrap = div()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .flex()
            .flex_col()
            .when(pane_idx > 0, |d| d.border_l_1().border_color(rgb(p.line)));

        if is_split {
            let label = self.panes[pane_idx].location.title(&self.home);
            wrap = wrap.child(
                div()
                    .id(("split-hdr", pane_idx))
                    .h(px(26.))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .bg(rgb(if is_active { p.surface } else { p.canvas }))
                    .border_b_1()
                    .border_color(rgb(p.line))
                    .font_family("IBM Plex Mono")
                    .text_size(px(11.))
                    .text_color(rgb(if is_active { p.signal_fg } else { p.muted }))
                    .child(label)
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.active_pane = pane_idx;
                        this.refresh_inspector_preview();
                        this.update_title(w);
                        cx.notify();
                    })),
            );
        }
        wrap.child(content)
    }

    fn render_center_bars(&self, p: Palette, cx: &mut Context<Self>) -> Option<Div> {
        // Inline Rename / New Folder prompt bar
        if let Some(mode) = &self.prompt
            && matches!(mode, PromptMode::Rename(_) | PromptMode::NewFolder(_))
        {
            let label = match mode {
                PromptMode::Rename(_) => "Rename",
                PromptMode::NewFolder(_) => "New folder",
                _ => "",
            };
            return Some(
                div()
                    .px(px(14.))
                    .py(px(8.))
                    .bg(rgb(p.surface))
                    .border_b_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .key_context("FilesPrompt")
                    .on_action(cx.listener(|this, _: &PromptSubmit, w, cx| this.submit_prompt(w, cx)))
                    .on_action(cx.listener(|this, _: &PromptCancel, w, cx| this.close_prompt(w, cx)))
                    .child(
                        div()
                            .font_family("IBM Plex Mono")
                            .text_size(px(11.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(p.signal_fg))
                            .child(label),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px(26.))
                            .px(px(8.))
                            .flex()
                            .items_center()
                            .rounded(px(4.))
                            .bg(rgb(p.canvas))
                            .border_1()
                            .border_color(rgb(p.focus))
                            .font_family("IBM Plex Mono")
                            .text_size(px(12.))
                            .text_color(rgb(p.text))
                            .child(self.prompt_input.clone()),
                    )
                    .child(
                        div()
                            .id("prompt-ok")
                            .px(px(10.))
                            .py(px(4.))
                            .rounded(px(4.))
                            .bg(rgb(p.accent))
                            .text_color(rgb(p.on_accent))
                            .text_size(px(11.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .child("Apply")
                            .on_click(cx.listener(|this, _, w, cx| this.submit_prompt(w, cx))),
                    )
                    .child(
                        div()
                            .id("prompt-cancel")
                            .px(px(10.))
                            .py(px(4.))
                            .rounded(px(4.))
                            .bg(rgb(p.canvas))
                            .border_1()
                            .border_color(rgb(p.line))
                            .text_color(rgb(p.muted))
                            .text_size(px(11.5))
                            .cursor_pointer()
                            .child("Cancel")
                            .on_click(cx.listener(|this, _, w, cx| this.close_prompt(w, cx))),
                    ),
            );
        }
        None
    }

    fn render_footer_notice(&self, p: Palette, cx: &mut Context<Self>) -> Option<Div> {
        let (msg, accent) = if let Some(err) = &self.error {
            (err.clone(), true)
        } else if let Some(clip) = &self.clipboard {
            let verb = if clip.op == ClipboardOp::Copy { "Copied" } else { "Cut" };
            let name = clip.path.file_name().and_then(|n| n.to_str()).unwrap_or("item");
            (format!("{verb} “{name}” · Ctrl+V to paste"), false)
        } else if let Some(status) = &self.status {
            (status.clone(), false)
        } else {
            return None;
        };

        Some(
            div()
                .h(px(30.))
                .px(px(14.))
                .flex()
                .items_center()
                .justify_between()
                .bg(rgb(p.surface))
                .border_t_1()
                .border_color(rgb(p.line))
                .font_family("IBM Plex Mono")
                .text_size(px(11.5))
                .text_color(rgb(if accent { p.signal_fg } else { p.muted }))
                .child(msg)
                .child(
                    div()
                        .id("dismiss-notice")
                        .cursor_pointer()
                        .child(icon("close", p.muted, 12.))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.clipboard = None;
                            this.clear_notices();
                            cx.notify();
                        })),
                ),
        )
    }

    fn render_inspector(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let selected = self.pane().selected_entry().cloned();
        let in_trash = self.pane().location.is_trash();

        let mut panel = div()
            .w(px(280.))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(rgb(p.canvas))
            .border_l_1()
            .border_color(rgb(p.line));

        let Some(entry) = selected else {
            return panel.child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_family("IBM Plex Mono")
                    .text_size(px(12.))
                    .text_color(rgb(p.muted))
                    .child("Nothing selected"),
            );
        };

        // Preview stage (top center)
        let preview_stage = if entry.is_previewable_image() {
            div()
                .h(px(200.))
                .flex_shrink_0()
                .p(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    img(entry.path.clone())
                        .max_w(px(244.))
                        .max_h(px(164.))
                        .object_fit(ObjectFit::Contain)
                        .rounded(px(4.)),
                )
        } else if let Some(snippet) = &self.cached_preview_text {
            div().h(px(200.)).flex_shrink_0().p(px(16.)).flex().child(
                div()
                    .size_full()
                    .p(px(10.))
                    .rounded(px(4.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.line))
                    .overflow_hidden()
                    .font_family("IBM Plex Mono")
                    .text_size(px(10.))
                    .text_color(rgb(p.muted))
                    .child(snippet.clone()),
            )
        } else {
            div()
                .h(px(200.))
                .flex_shrink_0()
                .flex()
                .items_center()
                .justify_center()
                .child(icon(
                    entry.icon_name(),
                    if entry.is_dir { p.signal_fg } else { p.muted },
                    54.,
                ))
        };

        let meta_row = |key: &'static str, value: String| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(12.))
                .font_family("IBM Plex Mono")
                .text_size(px(11.5))
                .child(div().text_color(rgb(p.muted)).child(key))
                .child(
                    div()
                        .text_color(rgb(p.text))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(value),
                )
        };

        let mut details = div()
            .flex_1()
            .min_h(px(0.))
            .px(px(20.))
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .font_family("IBM Plex Mono")
                    .text_size(px(13.5))
                    .font_weight(FontWeight::BOLD)
                    .text_color(rgb(p.text))
                    .child(entry.name.clone()),
            )
            .child(
                div()
                    .font_family("IBM Plex Mono")
                    .text_size(px(11.5))
                    .text_color(rgb(p.muted))
                    .child(entry.subtitle()),
            )
            .child(
                div()
                    .mt(px(16.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(meta_row("Modified", fs_model::format_inspector_date(entry.modified)))
                    .when(!entry.is_dir, |d| {
                        d.child(meta_row("Size", fs_model::format_size(entry.size)))
                    })
                    .child(meta_row("Mode", fs_model::format_permissions(entry.mode)))
                    .when_some(entry.symlink_target.clone(), |d, target| {
                        d.child(meta_row("Target", target.display().to_string()))
                    })
                    .when_some(entry.trash_original.clone(), |d, orig| {
                        d.child(meta_row("Original", fs_model::display_path(&orig, &self.home)))
                    }),
            );

        if entry.is_previewable_text() || entry.is_previewable_image() {
            details = details.child(
                div()
                    .mt(px(12.))
                    .font_family("IBM Plex Mono")
                    .text_size(px(11.))
                    .text_color(rgb(p.muted))
                    .child("Space to preview"),
            );
        }

        // Bottom primary action button
        let cta_label = if in_trash {
            "Restore"
        } else if entry.is_dir {
            "Open folder"
        } else {
            "Open"
        };
        let entry_cta = entry.clone();

        panel = panel.child(preview_stage).child(details).child(
            div().p(px(16.)).flex_shrink_0().child(
                div()
                    .id("inspector-cta")
                    .h(px(36.))
                    .px(px(14.))
                    .rounded(px(4.))
                    .bg(rgb(p.accent))
                    .text_color(rgb(p.on_accent))
                    .flex()
                    .items_center()
                    .justify_between()
                    .cursor_pointer()
                    .hover(|s| s.opacity(0.88))
                    .font_family("Manrope")
                    .text_size(px(12.5))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(cta_label)
                    .child(icon("open-external", p.on_accent, 14.))
                    .on_click(cx.listener(move |this, _, w, cx| {
                        this.open_entry(entry_cta.clone(), w, cx);
                    })),
            ),
        );
        panel
    }

    fn render_overlay(&self, p: Palette, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let overlay = self.overlay.as_ref()?;
        match overlay {
            Overlay::ViewMenu => {
                let is_split = self.panes.len() > 1;
                let modes: [(ViewMode, &'static str); 3] = [
                    (ViewMode::Columns, "Ctrl+1"),
                    (ViewMode::List, "Ctrl+2"),
                    (ViewMode::Grid, "Ctrl+3"),
                ];
                let mut menu = div()
                    .id("view-popover")
                    .w(px(210.))
                    .p(px(6.))
                    .rounded(px(6.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .flex_col()
                    .gap(px(2.));
                for (mode, hint) in modes {
                    let active = self.saved.view_mode == mode;
                    menu = menu.child(
                        div()
                            .id(("view-opt", mode as usize))
                            .h(px(28.))
                            .px(px(10.))
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .when(active, |d| d.bg(rgba(p.selection_bg)))
                            .hover(move |s| s.bg(rgb(p.canvas)))
                            .font_family("IBM Plex Mono")
                            .text_size(px(12.))
                            .text_color(rgb(if active { p.signal_fg } else { p.text }))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(icon(mode.icon(), if active { p.signal_fg } else { p.muted }, 13.5))
                                    .child(mode.label()),
                            )
                            .child(div().text_size(px(10.5)).text_color(rgb(p.muted)).child(hint))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_view_mode(mode, cx);
                            })),
                    );
                }
                menu = menu.child(div().my(px(4.)).h(px(1.)).bg(rgb(p.line))).child(
                    div()
                        .id("view-split-toggle")
                        .h(px(28.))
                        .px(px(10.))
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgb(p.canvas)))
                        .font_family("IBM Plex Mono")
                        .text_size(px(12.))
                        .text_color(rgb(p.text))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(icon("split", if is_split { p.signal_fg } else { p.muted }, 13.5))
                                .child("Split pane"),
                        )
                        .child(if is_split { "On" } else { "Off" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.overlay = None;
                            this.toggle_split(cx);
                        })),
                );

                Some(
                    div()
                        .id("view-backdrop")
                        .absolute()
                        .inset_0()
                        .flex()
                        .justify_end()
                        .items_start()
                        .pt(px(46.))
                        .pr(px(48.))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.overlay = None;
                                cx.notify();
                            }),
                        )
                        .child(menu.on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })),
                )
            }

            Overlay::SortMenu => {
                let mut menu = div()
                    .id("sort-popover")
                    .w(px(210.))
                    .p(px(6.))
                    .rounded(px(6.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .flex_col()
                    .gap(px(2.));
                for field in SortField::ALL {
                    let active = self.sort_field == field;
                    let arrow = if active {
                        if self.sort_order == SortOrder::Asc {
                            "↑"
                        } else {
                            "↓"
                        }
                    } else {
                        ""
                    };
                    menu = menu.child(
                        div()
                            .id(("sort-opt", field as usize))
                            .h(px(28.))
                            .px(px(10.))
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .when(active, |d| d.bg(rgba(p.selection_bg)))
                            .hover(move |s| s.bg(rgb(p.canvas)))
                            .font_family("IBM Plex Mono")
                            .text_size(px(12.))
                            .text_color(rgb(if active { p.signal_fg } else { p.text }))
                            .child(field.label())
                            .child(arrow)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_sort(field, cx);
                                this.overlay = None;
                                cx.notify();
                            })),
                    );
                }
                menu = menu.child(div().my(px(4.)).h(px(1.)).bg(rgb(p.line))).child(
                    div()
                        .id("sort-hidden-toggle")
                        .h(px(28.))
                        .px(px(10.))
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgb(p.canvas)))
                        .font_family("IBM Plex Mono")
                        .text_size(px(12.))
                        .text_color(rgb(p.text))
                        .child("Show hidden")
                        .child(if self.saved.show_hidden { "On" } else { "Off" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.toggle_hidden(cx);
                            this.overlay = None;
                            cx.notify();
                        })),
                );

                Some(
                    div()
                        .id("sort-backdrop")
                        .absolute()
                        .inset_0()
                        .flex()
                        .justify_end()
                        .items_start()
                        .pt(px(46.))
                        .pr(px(16.))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.overlay = None;
                                cx.notify();
                            }),
                        )
                        .child(menu.on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })),
                )
            }

            Overlay::ContextMenu(entry) => {
                let entry = entry.clone();
                let in_trash = self.pane().location.is_trash();
                let is_pinned = self.saved.pinned.iter().any(|x| x == &entry.path);
                let mut menu = div()
                    .w(px(230.))
                    .p(px(6.))
                    .rounded(px(6.))
                    .bg(rgb(p.surface))
                    .border_1()
                    .border_color(rgb(p.line))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .px(px(10.))
                            .py(px(6.))
                            .border_b_1()
                            .border_color(rgb(p.line))
                            .mb(px(4.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .font_family("IBM Plex Mono")
                            .text_size(px(11.5))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(p.signal_fg))
                            .child(entry.name.clone()),
                    );

                let row = |id: &'static str, label: &'static str, hint: &'static str| {
                    div()
                        .id(id)
                        .h(px(28.))
                        .px(px(10.))
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .cursor_pointer()
                        .hover(move |s| s.bg(rgb(p.canvas)))
                        .font_family("IBM Plex Mono")
                        .text_size(px(12.))
                        .text_color(rgb(p.text))
                        .child(label)
                        .child(div().text_size(px(10.5)).text_color(rgb(p.muted)).child(hint))
                };

                if in_trash {
                    let e1 = entry.clone();
                    let e2 = entry.clone();
                    menu = menu
                        .child(
                            row("ctx-restore", "Restore", "Enter").on_click(cx.listener(move |this, _, _, cx| {
                                this.overlay = None;
                                this.restore_entry(&e1, cx);
                            })),
                        )
                        .child(row("ctx-del-perm", "Delete permanently", "Del").on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.overlay = Some(Overlay::ConfirmDelete(e2.clone()));
                                cx.notify();
                            },
                        )));
                } else {
                    let e_open = entry.clone();
                    let e_pin = entry.clone();
                    menu = menu
                        .child(
                            row("ctx-open", if entry.is_dir { "Open folder" } else { "Open" }, "Enter").on_click(
                                cx.listener(move |this, _, w, cx| {
                                    this.overlay = None;
                                    this.open_entry(e_open.clone(), w, cx);
                                }),
                            ),
                        )
                        .child(
                            row("ctx-preview", "Quick preview", "Space").on_click(cx.listener(|this, _, _, cx| {
                                this.overlay = None;
                                this.open_quick_preview(cx);
                            })),
                        )
                        .child(div().my(px(4.)).h(px(1.)).bg(rgb(p.line)))
                        .child(
                            row("ctx-rename", "Rename", "F2").on_click(cx.listener(|this, _, w, cx| {
                                this.overlay = None;
                                this.on_rename(&RenameItem, w, cx);
                            })),
                        )
                        .child(
                            row("ctx-copy", "Copy", "Ctrl+C").on_click(cx.listener(|this, _, w, cx| {
                                this.overlay = None;
                                this.on_copy(&CopyItem, w, cx);
                            })),
                        )
                        .child(row("ctx-cut", "Cut", "Ctrl+X").on_click(cx.listener(|this, _, w, cx| {
                            this.overlay = None;
                            this.on_cut(&CutItem, w, cx);
                        })))
                        .child(
                            row("ctx-dup", "Duplicate", "Ctrl+D").on_click(cx.listener(|this, _, w, cx| {
                                this.overlay = None;
                                this.on_duplicate(&DuplicateItem, w, cx);
                            })),
                        )
                        .when(self.clipboard.is_some(), |d| {
                            d.child(
                                row("ctx-paste", "Paste here", "Ctrl+V").on_click(cx.listener(|this, _, _, cx| {
                                    this.overlay = None;
                                    this.paste_into_active(cx);
                                })),
                            )
                        })
                        .when(entry.is_dir, |d| {
                            d.child(
                                row(
                                    "ctx-pin",
                                    if is_pinned { "Unpin folder" } else { "Pin folder" },
                                    "Ctrl+P",
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.overlay = None;
                                    this.toggle_pin_current(Some(e_pin.path.clone()), cx);
                                })),
                            )
                        })
                        .child(div().my(px(4.)).h(px(1.)).bg(rgb(p.line)))
                        .child(
                            row("ctx-term", "Open terminal here", "Ctrl+Shift+T").on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.overlay = None;
                                    this.open_terminal_here(cx);
                                },
                            )),
                        )
                        .child(
                            row("ctx-trash", "Move to Trash", "Del").on_click(cx.listener(|this, _, _, cx| {
                                this.overlay = None;
                                this.trash_selected(cx);
                            })),
                        );
                }

                Some(
                    div()
                        .id("ctx-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(rgba(0x00000055))
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.overlay = None;
                                cx.notify();
                            }),
                        )
                        .child(menu.on_mouse_down(MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        })),
                )
            }

            Overlay::ConfirmDelete(entry) => {
                let target = entry.clone();
                Some(
                    div()
                        .id("del-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(rgba(0x00000080))
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.overlay = None;
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .w(px(420.))
                                .p(px(20.))
                                .rounded(px(8.))
                                .bg(rgb(p.surface))
                                .border_1()
                                .border_color(rgb(p.line))
                                .flex()
                                .flex_col()
                                .gap(px(12.))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .child(
                                    div()
                                        .font_weight(FontWeight::BOLD)
                                        .text_size(px(15.))
                                        .text_color(rgb(p.text))
                                        .child("Delete permanently?"),
                                )
                                .child(
                                    div()
                                        .font_family("IBM Plex Mono")
                                        .text_size(px(12.))
                                        .text_color(rgb(p.muted))
                                        .child(format!(
                                            "“{}” will be permanently removed and cannot be undone.",
                                            entry.name
                                        )),
                                )
                                .child(
                                    div()
                                        .mt(px(6.))
                                        .flex()
                                        .justify_end()
                                        .gap(px(8.))
                                        .child(
                                            div()
                                                .id("del-cancel")
                                                .px(px(14.))
                                                .py(px(7.))
                                                .rounded(px(4.))
                                                .bg(rgb(p.canvas))
                                                .border_1()
                                                .border_color(rgb(p.line))
                                                .text_size(px(12.))
                                                .text_color(rgb(p.text))
                                                .cursor_pointer()
                                                .child("Cancel")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.overlay = None;
                                                    cx.notify();
                                                })),
                                        )
                                        .child(
                                            div()
                                                .id("del-confirm")
                                                .px(px(14.))
                                                .py(px(7.))
                                                .rounded(px(4.))
                                                .bg(rgb(p.accent))
                                                .text_size(px(12.))
                                                .font_weight(FontWeight::SEMIBOLD)
                                                .text_color(rgb(p.on_accent))
                                                .cursor_pointer()
                                                .child("Delete permanently")
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.confirm_delete_permanent(&target, cx);
                                                })),
                                        ),
                                ),
                        ),
                )
            }

            Overlay::QuickPreview(entry, text_opt) => {
                let body: AnyElement = if entry.is_previewable_image() {
                    div()
                        .flex_1()
                        .min_h(px(0.))
                        .p(px(16.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            img(entry.path.clone())
                                .max_w(px(640.))
                                .max_h(px(420.))
                                .object_fit(ObjectFit::Contain)
                                .rounded(px(4.)),
                        )
                        .into_any_element()
                } else if let Some(text) = text_opt {
                    div()
                        .id("preview-text-scroll")
                        .flex_1()
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .p(px(16.))
                        .font_family("IBM Plex Mono")
                        .text_size(px(12.))
                        .text_color(rgb(p.text))
                        .child(text.clone())
                        .into_any_element()
                } else {
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .items_center()
                        .justify_center()
                        .gap(px(12.))
                        .child(icon(entry.icon_name(), p.signal_fg, 56.))
                        .child(
                            div()
                                .font_family("IBM Plex Mono")
                                .text_size(px(12.5))
                                .text_color(rgb(p.muted))
                                .child(entry.subtitle()),
                        )
                        .into_any_element()
                };

                Some(
                    div()
                        .id("preview-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(rgba(0x00000099))
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.overlay = None;
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .w(px(700.))
                                .h(px(520.))
                                .rounded(px(8.))
                                .bg(rgb(p.canvas))
                                .border_1()
                                .border_color(rgb(p.line))
                                .flex()
                                .flex_col()
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .child(
                                    div()
                                        .h(px(40.))
                                        .px(px(16.))
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .border_b_1()
                                        .border_color(rgb(p.line))
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap(px(10.))
                                                .font_family("IBM Plex Mono")
                                                .text_size(px(12.5))
                                                .text_color(rgb(p.text))
                                                .child(entry.name.clone())
                                                .child(
                                                    div()
                                                        .text_size(px(11.5))
                                                        .text_color(rgb(p.muted))
                                                        .child(entry.subtitle()),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id("close-preview")
                                                .size(px(24.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded(px(4.))
                                                .cursor_pointer()
                                                .hover(move |s| s.bg(rgb(p.surface)))
                                                .child(icon("close", p.muted, 14.))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.overlay = None;
                                                    cx.notify();
                                                })),
                                        ),
                                )
                                .child(body),
                        ),
                )
            }

            Overlay::Help => {
                let shortcuts: [(&'static str, &'static str); 18] = [
                    ("Arrows, Enter", "Move and open"),
                    ("Space", "Quick preview"),
                    ("Backspace", "Parent folder"),
                    ("Alt+Left / Alt+Right", "Back / Forward"),
                    ("Ctrl+1 / 2 / 3", "Columns / List / Grid"),
                    ("Ctrl+L", "Type a path"),
                    ("/", "Search here and below"),
                    ("Ctrl+C / X / V", "Copy, cut, paste"),
                    ("Ctrl+D", "Duplicate item"),
                    ("F2", "Rename"),
                    ("Delete / Shift+Del", "Trash / Delete permanently"),
                    ("Ctrl+Shift+N", "New folder"),
                    ("Ctrl+P", "Pin or unpin folder"),
                    ("Ctrl+H", "Show or hide dotfiles"),
                    ("Ctrl+I", "Toggle inspector pane"),
                    ("Ctrl+Alt+N / Tab", "Split pane / Switch pane"),
                    ("Alt+Enter", "Context menu"),
                    ("Ctrl+Shift+T / Ctrl+Q", "Open terminal / Quit"),
                ];
                let mut rows = div().flex().flex_col().gap(px(6.));
                for (keys, desc) in shortcuts {
                    rows = rows.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap(px(16.))
                            .font_family("IBM Plex Mono")
                            .text_size(px(12.))
                            .child(div().text_color(rgb(p.signal_fg)).child(keys))
                            .child(div().text_color(rgb(p.text)).child(desc)),
                    );
                }
                Some(
                    div()
                        .id("help-backdrop")
                        .absolute()
                        .inset_0()
                        .bg(rgba(0x00000090))
                        .flex()
                        .items_center()
                        .justify_center()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.overlay = None;
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .w(px(500.))
                                .p(px(20.))
                                .rounded(px(8.))
                                .bg(rgb(p.canvas))
                                .border_1()
                                .border_color(rgb(p.line))
                                .flex()
                                .flex_col()
                                .gap(px(14.))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .font_weight(FontWeight::BOLD)
                                                .text_size(px(15.))
                                                .text_color(rgb(p.text))
                                                .child("Keyboard Shortcuts"),
                                        )
                                        .child(
                                            div()
                                                .id("close-help")
                                                .cursor_pointer()
                                                .child(icon("close", p.muted, 14.))
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.overlay = None;
                                                    cx.notify();
                                                })),
                                        ),
                                )
                                .child(rows),
                        ),
                )
            }
        }
    }
}

impl Render for FilesApp {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_appearance_if_changed(cx);
        let p = self.palette();
        let center_bar = self.render_center_bars(p, cx);
        let footer_notice = self.render_footer_notice(p, cx);
        let overlay = self.render_overlay(p, cx);

        let mut panes_row = div().flex_1().min_h(px(0.)).flex();
        for idx in 0..self.panes.len() {
            panes_row = panes_row.child(self.render_pane(idx, p, cx));
        }

        div()
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(p.canvas))
            .text_color(rgb(p.text))
            .font_family("Manrope")
            .key_context("FilesBrowser")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::on_move_up))
            .on_action(cx.listener(Self::on_move_down))
            .on_action(cx.listener(Self::on_move_left))
            .on_action(cx.listener(Self::on_move_right))
            .on_action(cx.listener(Self::on_select_first))
            .on_action(cx.listener(Self::on_select_last))
            .on_action(cx.listener(Self::on_open_selected))
            .on_action(cx.listener(|this, _: &QuickPreview, _, cx| this.open_quick_preview(cx)))
            .on_action(cx.listener(Self::on_go_up))
            .on_action(cx.listener(Self::on_go_back))
            .on_action(cx.listener(Self::on_go_forward))
            .on_action(cx.listener(|this, _: &ViewColumns, _, cx| this.set_view_mode(ViewMode::Columns, cx)))
            .on_action(cx.listener(|this, _: &ViewList, _, cx| this.set_view_mode(ViewMode::List, cx)))
            .on_action(cx.listener(|this, _: &ViewGrid, _, cx| this.set_view_mode(ViewMode::Grid, cx)))
            .on_action(cx.listener(Self::on_focus_path))
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_cut))
            .on_action(cx.listener(|this, _: &PasteItem, _, cx| this.paste_into_active(cx)))
            .on_action(cx.listener(Self::on_duplicate))
            .on_action(cx.listener(Self::on_rename))
            .on_action(cx.listener(|this, _: &TrashItem, _, cx| this.trash_selected(cx)))
            .on_action(cx.listener(|this, _: &DeletePermanent, _, cx| {
                if let Some(entry) = this.pane().selected_entry().cloned() {
                    this.overlay = Some(Overlay::ConfirmDelete(entry));
                    cx.notify();
                }
            }))
            .on_action(cx.listener(Self::on_new_folder))
            .on_action(cx.listener(|this, _: &TogglePin, _, cx| this.toggle_pin_current(None, cx)))
            .on_action(cx.listener(|this, _: &ToggleHidden, _, cx| this.toggle_hidden(cx)))
            .on_action(cx.listener(|this, _: &ToggleInspector, _, cx| this.toggle_inspector(cx)))
            .on_action(cx.listener(|this, _: &ToggleSplit, _, cx| this.toggle_split(cx)))
            .on_action(cx.listener(|this, _: &SwitchPane, w, cx| {
                if this.panes.len() > 1 {
                    this.active_pane = (this.active_pane + 1) % this.panes.len();
                    this.refresh_inspector_preview();
                    this.update_title(w);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &OpenTerminal, _, cx| this.open_terminal_here(cx)))
            .on_action(cx.listener(|this, _: &OpenContextMenu, _, cx| {
                if let Some(entry) = this.pane().selected_entry().cloned() {
                    this.overlay = Some(Overlay::ContextMenu(entry));
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleHelp, _, cx| {
                this.overlay = if matches!(this.overlay, Some(Overlay::Help)) {
                    None
                } else {
                    Some(Overlay::Help)
                };
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.reload_all(None, cx)))
            .on_action(cx.listener(Self::on_cancel_overlay))
            .on_action(cx.listener(|_, _: &Quit, _, cx| cx.quit()))
            .child(self.render_header(p, cx))
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .flex()
                    .child(self.render_sidebar(p, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .flex()
                            .flex_col()
                            .when_some(center_bar, |d, bar| d.child(bar))
                            .child(panes_row)
                            .when_some(footer_notice, |d, bar| d.child(bar)),
                    )
                    .when(self.saved.show_inspector, |d| d.child(self.render_inspector(p, cx))),
            )
            .when_some(overlay, |d, ov| d.child(ov))
    }
}

enum CliAction {
    ExitOk,
    Launch(PathBuf),
}

fn parse_cli(args: &[String], home: &Path) -> Result<CliAction, String> {
    match args {
        [] => Ok(CliAction::Launch(home.to_path_buf())),
        [arg] if arg == "--version" || arg == "-V" => {
            println!("crowbaros-files {} (GPUI 0.2.2; Wayland)", env!("CARGO_PKG_VERSION"));
            Ok(CliAction::ExitOk)
        }
        [arg] if arg == "--help" || arg == "-h" => {
            println!("Usage: crowbaros-files [--version] [PATH|file://URI]");
            Ok(CliAction::ExitOk)
        }
        [arg] if !arg.starts_with('-') => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| home.to_path_buf());
            Ok(CliAction::Launch(fs_model::expand_path(arg, home, &cwd)))
        }
        _ => Err("Usage: crowbaros-files [--version] [PATH]".into()),
    }
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let home = fs_model::home_dir();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let initial_dir = match parse_cli(&args, &home) {
        Ok(CliAction::ExitOk) => return,
        Ok(CliAction::Launch(path)) => path,
        Err(e) => {
            eprintln!("Files: {e}");
            std::process::exit(1);
        }
    };

    Application::new().with_assets(Assets).run(move |cx: &mut App| {
        cx.text_system()
            .add_fonts(vec![
                Cow::Borrowed(include_bytes!("../../shell/fonts/Manrope-Regular.ttf")),
                Cow::Borrowed(include_bytes!("../../shell/fonts/Manrope-SemiBold.ttf")),
                Cow::Borrowed(include_bytes!("../../shell/fonts/Manrope-Bold.ttf")),
                Cow::Borrowed(include_bytes!("../../shell/fonts/IBMPlexMono-Regular.ttf")),
            ])
            .expect("bundled fonts must load");

        input::bind_keys(cx);
        cx.bind_keys([
            KeyBinding::new("enter", PromptSubmit, Some("FilesPrompt")),
            KeyBinding::new("escape", PromptCancel, Some("FilesPrompt")),
            KeyBinding::new("up", MoveUp, Some("FilesBrowser")),
            KeyBinding::new("down", MoveDown, Some("FilesBrowser")),
            KeyBinding::new("left", MoveLeft, Some("FilesBrowser")),
            KeyBinding::new("right", MoveRight, Some("FilesBrowser")),
            KeyBinding::new("home", SelectFirst, Some("FilesBrowser")),
            KeyBinding::new("end", SelectLast, Some("FilesBrowser")),
            KeyBinding::new("enter", OpenSelected, Some("FilesBrowser")),
            KeyBinding::new("space", QuickPreview, Some("FilesBrowser")),
            KeyBinding::new("backspace", GoUp, Some("FilesBrowser")),
            KeyBinding::new("alt-left", GoBack, Some("FilesBrowser")),
            KeyBinding::new("alt-right", GoForward, Some("FilesBrowser")),
            KeyBinding::new("ctrl-1", ViewColumns, Some("FilesBrowser")),
            KeyBinding::new("ctrl-2", ViewList, Some("FilesBrowser")),
            KeyBinding::new("ctrl-3", ViewGrid, Some("FilesBrowser")),
            KeyBinding::new("ctrl-l", FocusPath, Some("FilesBrowser")),
            KeyBinding::new("/", FocusSearch, Some("FilesBrowser")),
            KeyBinding::new("ctrl-c", CopyItem, Some("FilesBrowser")),
            KeyBinding::new("ctrl-x", CutItem, Some("FilesBrowser")),
            KeyBinding::new("ctrl-v", PasteItem, Some("FilesBrowser")),
            KeyBinding::new("ctrl-d", DuplicateItem, Some("FilesBrowser")),
            KeyBinding::new("f2", RenameItem, Some("FilesBrowser")),
            KeyBinding::new("delete", TrashItem, Some("FilesBrowser")),
            KeyBinding::new("shift-delete", DeletePermanent, Some("FilesBrowser")),
            KeyBinding::new("ctrl-shift-n", NewFolder, Some("FilesBrowser")),
            KeyBinding::new("ctrl-p", TogglePin, Some("FilesBrowser")),
            KeyBinding::new("ctrl-h", ToggleHidden, Some("FilesBrowser")),
            KeyBinding::new("ctrl-i", ToggleInspector, Some("FilesBrowser")),
            KeyBinding::new("ctrl-alt-n", ToggleSplit, Some("FilesBrowser")),
            KeyBinding::new("tab", SwitchPane, Some("FilesBrowser")),
            KeyBinding::new("ctrl-shift-t", OpenTerminal, Some("FilesBrowser")),
            KeyBinding::new("alt-enter", OpenContextMenu, Some("FilesBrowser")),
            KeyBinding::new("?", ToggleHelp, Some("FilesBrowser")),
            KeyBinding::new("f5", Refresh, Some("FilesBrowser")),
            KeyBinding::new("ctrl-r", Refresh, Some("FilesBrowser")),
            KeyBinding::new("escape", CancelOverlay, Some("FilesBrowser")),
            KeyBinding::new("ctrl-q", Quit, Some("FilesBrowser")),
        ]);

        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1160.), px(740.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(780.), px(520.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Files".into()),
                    ..Default::default()
                }),
                app_id: Some("com.crowbaros.Files".into()),
                ..Default::default()
            },
            |w, cx| cx.new(|cx| FilesApp::new(initial_dir, w, cx)),
        )
        .expect("open Files window");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_version_and_paths() {
        let home = Path::new("/home/crowbar");
        assert!(matches!(
            parse_cli(&[], home).unwrap(),
            CliAction::Launch(p) if p == home
        ));
        assert!(matches!(
            parse_cli(&["--version".into()], home).unwrap(),
            CliAction::ExitOk
        ));
        assert!(matches!(
            parse_cli(&["file:///tmp/demo".into()], home).unwrap(),
            CliAction::Launch(p) if p == Path::new("/tmp/demo")
        ));
        assert!(parse_cli(&["--bogus".into()], home).is_err());
    }
}
