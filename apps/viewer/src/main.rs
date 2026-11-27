// SPDX-License-Identifier: MIT
use crowbar_ui::{FONT_MONO, FONT_UI, ThemeWatcher, UiPalette, load_fonts, spawn_theme_watcher};
use crowbaros_viewer::view_model;
use crowbaros_viewer::{
    image_document::{self, LoadedImage, ViewState},
    pdf_document::{PdfDocument, SearchHit},
};
use gpui::{
    Action, AnyElement, AnyWindowHandle, App, Application, Bounds, ClipboardItem, Context, Entity, FocusHandle, Image,
    ImageFormat, KeyBinding, MouseButton, MouseDownEvent, MouseMoveEvent, ObjectFit, Pixels, Point, RenderImage,
    ScrollHandle, ScrollStrategy, Size, Subscription, UniformListScrollHandle, Window, WindowBounds, WindowOptions,
    actions, div, img, prelude::*, px, rgb, rgba, size, uniform_list,
};
use gpui_component::{
    Root,
    input::{Input, InputState, RopeExt},
};
use std::{
    collections::BTreeMap,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
};

actions!(
    viewer,
    [
        Open,
        Quit,
        ZoomIn,
        ZoomOut,
        Fit,
        Actual,
        Rotate,
        Inspector,
        Previous,
        Next,
        Find,
        GoTo,
        Thumbnails,
        Text,
        CopyImage,
        SetWallpaper,
        RestoreWallpaper,
        CopyPageText,
        NextMatch,
        PreviousMatch,
        Confirm,
        Dismiss
    ]
);

struct Raster {
    image: Arc<RenderImage>,
    width: u32,
    height: u32,
}
impl Raster {
    fn new(width: u32, height: u32, mut rgba: Vec<u8>) -> Self {
        for p in rgba.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
        }
        let buffer = image::RgbaImage::from_raw(width, height, rgba).expect("validated raster dimensions");
        Self {
            image: Arc::new(RenderImage::new([image::Frame::new(buffer)])),
            width,
            height,
        }
    }
}
enum Document {
    Image(Arc<LoadedImage>, Vec<PathBuf>),
    Pdf(Arc<PdfDocument>),
}
#[derive(Clone, Copy)]
enum Prompt {
    Open,
    Find,
    Page,
}
struct Viewer {
    focus: FocusHandle,
    field: Entity<InputState>,
    text_field: Entity<InputState>,
    window: AnyWindowHandle,
    palette: UiPalette,
    path: Option<PathBuf>,
    document: Option<Document>,
    raster: Option<Raster>,
    retired: Vec<Arc<RenderImage>>,
    pages: BTreeMap<usize, Raster>,
    page_failures: BTreeMap<usize, String>,
    thumbnail_cache: view_model::PageCache<Raster>,
    thumbnail_scroll: UniformListScrollHandle,
    thumbnail_top: usize,
    view: ViewState,
    page: usize,
    thumbnails: bool,
    inspector: bool,
    text: bool,
    page_text: String,
    text_page: Option<usize>,
    text_focus_pending: bool,
    matches: Vec<SearchHit>,
    match_index: usize,
    prompt: Option<Prompt>,
    status: String,
    busy: bool,
    generation: u64,
    pdf_busy: bool,
    viewport: Size<Pixels>,
    _bounds: Subscription,
    scroll: UniformListScrollHandle,
    image_scroll: ScrollHandle,
    drag: Option<(Point<Pixels>, Point<Pixels>)>,
}
impl Viewer {
    fn new(path: Option<PathBuf>, w: &mut Window, cx: &mut Context<Self>) -> Self {
        let palette = ThemeWatcher::for_user()
            .ok()
            .map(|t| UiPalette::from_active(&t.current()))
            .unwrap_or_default();
        apply_theme(palette, cx);
        spawn_theme_watcher(cx, |s, a, cx| {
            s.palette = UiPalette::from_active(&a);
            apply_theme(s.palette, cx);
            cx.notify();
        });
        let mut s = Self {
            focus: cx.focus_handle(),
            field: cx.new(|cx| InputState::new(w, cx)),
            text_field: cx.new(|cx| InputState::new(w, cx).multi_line(true)),
            window: w.window_handle(),
            palette,
            path: None,
            document: None,
            raster: None,
            retired: Vec::new(),
            pages: BTreeMap::new(),
            page_failures: BTreeMap::new(),
            thumbnail_cache: view_model::PageCache::new(),
            thumbnail_scroll: UniformListScrollHandle::new(),
            thumbnail_top: 0,
            view: ViewState::default(),
            page: 0,
            thumbnails: true,
            inspector: false,
            text: false,
            page_text: "".into(),
            text_page: None,
            text_focus_pending: false,
            matches: Vec::new(),
            match_index: 0,
            prompt: None,
            status: "Open an image or PDF to begin.".into(),
            busy: false,
            generation: 0,
            pdf_busy: false,
            viewport: w.viewport_size(),
            _bounds: cx.observe_window_bounds(w, |s, w, cx| {
                s.viewport = w.viewport_size();
                s.refresh_pdf_layout(cx);
                cx.notify();
            }),
            scroll: UniformListScrollHandle::new(),
            image_scroll: ScrollHandle::new(),
            drag: None,
        };
        s.focus.focus(w);
        if let Some(path) = path {
            s.open(path, cx);
        }
        s
    }
    fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let path = match view_model::parse_open_path(path.as_os_str())
            .and_then(|p| std::path::absolute(p).map_err(|e| e.to_string()))
        {
            Ok(p) => p,
            Err(e) => {
                self.status = e;
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.generation += 1;
        let generation = self.generation;
        self.status = "Opening…".into();
        self.path = Some(path.clone());
        self.text_page = None;
        self.text_focus_pending = false;
        self.matches.clear();
        self.scroll = UniformListScrollHandle::new();
        self.image_scroll = ScrollHandle::new();
        self.thumbnail_scroll = UniformListScrollHandle::new();
        self.thumbnail_top = 0;
        for raster in self.thumbnail_cache.reset() {
            self.retired.push(raster.image);
        }
        self.document = None;
        self.page_failures.clear();
        if let Some(raster) = self.raster.take() {
            self.retired.push(raster.image);
        }
        for (_, raster) in std::mem::take(&mut self.pages) {
            self.retired.push(raster.image);
        }
        self.page = 0;
        self.page_text = "".into();
        self.view = ViewState::default();
        let task = cx.background_executor().spawn(async move {
            if is_pdf(&path) {
                let doc = Arc::new(PdfDocument::open(&path)?);
                let page = doc.render_page(0, 1000)?;
                let text = readable_pdf_text(doc.page_text(0));
                Ok::<_, String>((
                    Document::Pdf(doc),
                    Raster::new(page.width, page.height, page.rgba),
                    text,
                ))
            } else {
                let loaded = image_document::load_image(&path)?;
                let raster = Raster::new(loaded.width, loaded.height, loaded.rgba.clone());
                let siblings = image_document::sibling_images(&path).unwrap_or_default();
                Ok((Document::Image(Arc::new(loaded), siblings), raster, String::new()))
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if generation != s.generation {
                    return;
                }
                s.busy = false;
                match result {
                    Ok((doc, raster, text)) => {
                        let pdf = matches!(doc, Document::Pdf(_));
                        if pdf {
                            s.pages.insert(0, raster);
                        } else {
                            s.raster = Some(raster);
                        }
                        s.document = Some(doc);
                        s.page_text = text;
                        s.text_page = Some(s.page);
                        s.update_text_field(cx);
                        s.status = String::new();
                        eprintln!("VIEWER loaded kind={}", if pdf { "pdf" } else { "image" });
                        s.fetch_pages(cx);
                        s.fetch_thumbnails(cx);
                    }
                    Err(e) => s.status = format!("Could not open this file. {e}"),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn fetch_pages(&mut self, cx: &mut Context<Self>) {
        let Some(Document::Pdf(doc)) = &self.document else {
            return;
        };
        if self.pdf_busy {
            return;
        }
        let (_, height) = self.pdf_layout();
        let missing: Vec<_> =
            view_model::visible_pdf_pages(self.page, doc.page_count(), f32::from(self.viewport.height), height)
                .filter(|i| !self.pages.contains_key(i) && !self.page_failures.contains_key(i))
                .collect();
        if missing.is_empty() && self.text_page == Some(self.page) {
            return;
        }
        self.pdf_busy = true;
        let doc = doc.clone();
        let generation = self.generation;
        let page = self.page;
        let needs_text = self.text_page != Some(page);
        let task = cx.background_executor().spawn(async move {
            let pages = doc.render_pages(&missing, 1000);
            let text = needs_text.then(|| readable_pdf_text(doc.page_text(page)));
            (pages, text)
        });
        cx.spawn(async move |this, cx| {
            let (pages, text) = task.await;
            let _ = this.update(cx, |s, cx| {
                s.pdf_busy = false;
                if generation != s.generation {
                    s.fetch_pages(cx);
                    return;
                }
                for (i, result) in pages {
                    match result {
                        Ok(raster) => {
                            let raster = Raster::new(raster.width, raster.height, raster.rgba);
                            if let Some(old) = s.pages.insert(i, raster) {
                                cx.drop_image(old.image, None);
                            }
                        }
                        Err(error) => {
                            s.page_failures.insert(i, error);
                        }
                    }
                }
                if page == s.page
                    && let Some(text) = text
                {
                    s.page_text = text;
                    s.text_page = Some(s.page);
                    s.update_text_field(cx);
                }
                if let Some(Document::Pdf(doc)) = &s.document {
                    let (_, height) = s.pdf_layout();
                    let span = ((f32::from(s.viewport.height) / height).ceil() as usize + 2).clamp(4, 21);
                    let keep = s.page.saturating_sub(2)..s.page.saturating_add(span).min(doc.page_count());
                    s.page_failures.retain(|i, _| keep.contains(i));
                    s.pages.retain(|i, r| {
                        if keep.contains(i) {
                            true
                        } else {
                            cx.drop_image(r.image.clone(), None);
                            false
                        }
                    });
                }
                eprintln!("VIEWER page={}", page + 1);
                s.fetch_pages(cx);
                cx.notify();
            });
        })
        .detach();
    }
    fn fetch_thumbnails(&mut self, cx: &mut Context<Self>) {
        let Some(Document::Pdf(doc)) = &self.document else {
            return;
        };
        let (wanted, retained) = if self.thumbnails {
            view_model::thumbnail_windows(self.thumbnail_top, doc.page_count(), f32::from(self.viewport.height))
        } else {
            (0..0, 0..0)
        };
        for raster in self.thumbnail_cache.set_window(wanted, retained) {
            self.retired.push(raster.image);
        }
        let Some(request) = self.thumbnail_cache.request(2) else {
            return;
        };
        let doc = doc.clone();
        let pages = request.pages.clone();
        let task = cx.background_executor().spawn(async move {
            pages
                .into_iter()
                .map(|page| {
                    let raster = doc
                        .render_page(page, 120)
                        .map(|r| Raster::new(r.width, r.height, r.rgba));
                    (page, raster)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let results = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.thumbnail_cache.finish(&request) {
                    for (page, result) in results {
                        match result {
                            Ok(raster) => {
                                if let Some(old) = s.thumbnail_cache.insert(page, raster) {
                                    s.retired.push(old.image);
                                }
                                if s.thumbnail_cache.entries().contains_key(&page) {
                                    eprintln!("VIEWER thumbnail page={}", page + 1);
                                }
                            }
                            Err(_) => s.thumbnail_cache.fail(page),
                        }
                    }
                }
                s.fetch_thumbnails(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn update_text_field(&mut self, cx: &mut Context<Self>) {
        let field = self.text_field.clone();
        let text = self.page_text.clone();
        let offset = self
            .text_focus_pending
            .then(|| self.matches.get(self.match_index))
            .flatten()
            .filter(|hit| hit.page == self.page)
            .map(|hit| hit.offset);
        let focus = self.text_focus_pending && self.text;
        self.text_focus_pending = false;
        // This runs only from an asynchronous completion, with the window
        // back in App. The field is read-only but keeps normal selection keys.
        let _ = self.window.update(cx, |_, w, cx| {
            field.update(cx, |field, cx| {
                field.set_value(text, w, cx);
                if let Some(offset) = offset {
                    let position = field.text().offset_to_position(offset.min(field.text().len()));
                    field.set_cursor_position(position, w, cx);
                } else if focus {
                    field.focus(w, cx);
                }
            });
        });
    }

    fn focus_search_hit(&mut self, w: &mut Window, cx: &mut Context<Self>) {
        let Some(hit) = self
            .matches
            .get(self.match_index)
            .filter(|hit| Some(hit.page) == self.text_page)
        else {
            return;
        };
        let offset = hit.offset;
        self.text_field.update(cx, |field, cx| {
            let position = field.text().offset_to_position(offset.min(field.text().len()));
            field.set_cursor_position(position, w, cx);
        });
        self.text_focus_pending = false;
    }

    fn show_match(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(hit) = self.matches.get(index) else {
            return;
        };
        let page = hit.page;
        self.match_index = index;
        self.text = true;
        self.text_focus_pending = true;
        self.status = format!(
            "Match {} / {} · F3 next · Shift+F3 previous",
            index + 1,
            self.matches.len()
        );
        eprintln!(
            "VIEWER search match={} page={} offset={}",
            index + 1,
            page + 1,
            hit.offset
        );
        self.select_page(page, cx);
        self.refresh_pdf_layout(cx);
    }

    fn move_match(&mut self, backwards: bool, w: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = view_model::next_search_match(Some(self.match_index), self.matches.len(), backwards) {
            self.show_match(index, cx);
            self.focus_search_hit(w, cx);
        }
    }

    fn ask(&mut self, prompt: Prompt, w: &mut Window, cx: &mut Context<Self>) {
        if !matches!(prompt, Prompt::Open) && !matches!(self.document, Some(Document::Pdf(_))) {
            return;
        }
        self.prompt = Some(prompt);
        self.field.update(cx, |f, cx| {
            f.set_value("", w, cx);
            f.focus(w, cx);
        });
        cx.notify();
    }
    fn confirm(&mut self, _: &Confirm, w: &mut Window, cx: &mut Context<Self>) {
        let value = self.field.read(cx).value().to_string();
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        self.focus.focus(w);
        match prompt {
            Prompt::Open => self.open(PathBuf::from(value), cx),
            Prompt::Page => {
                if let Some(Document::Pdf(doc)) = &self.document {
                    match view_model::parse_page_number(&value, doc.page_count()) {
                        Ok(page) => self.select_page(page, cx),
                        Err(e) => self.status = e,
                    }
                }
            }
            Prompt::Find => {
                let Some(Document::Pdf(doc)) = &self.document else {
                    return;
                };
                if self.busy {
                    return;
                }
                self.busy = true;
                self.status = "Searching…".into();
                let doc = doc.clone();
                let generation = self.generation;
                let task = cx.background_executor().spawn(async move { doc.search(&value) });
                cx.spawn(async move |this, cx| {
                    let result = task.await;
                    let _ = this.update(cx, |s, cx| {
                        if generation != s.generation {
                            return;
                        }
                        s.busy = false;
                        match result {
                            Ok(hits) => {
                                eprintln!("VIEWER search matches={}", hits.len());
                                s.matches = hits;
                                s.match_index = 0;
                                if s.matches.is_empty() {
                                    s.status = "No matches in this document.".into();
                                } else {
                                    s.show_match(0, cx);
                                    if s.text_page == Some(s.page) {
                                        s.update_text_field(cx);
                                    }
                                }
                            }
                            Err(e) => s.status = e,
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }
    fn select_page(&mut self, page: usize, cx: &mut Context<Self>) {
        if let Some(Document::Pdf(doc)) = &self.document {
            let page = page.min(doc.page_count() - 1);
            if self.page != page {
                self.text_page = None;
                self.page_text.clear();
            }
            self.page = page;
            self.scroll.scroll_to_item_strict(self.page, ScrollStrategy::Top);
            self.thumbnail_scroll
                .scroll_to_item_strict(self.page, ScrollStrategy::Top);
            self.thumbnail_top = self.page;
            self.fetch_pages(cx);
            self.fetch_thumbnails(cx);
            cx.notify();
        }
    }
    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        match &self.document {
            Some(Document::Pdf(_)) => self.select_page(
                if forward {
                    self.page + 1
                } else {
                    self.page.saturating_sub(1)
                },
                cx,
            ),
            Some(Document::Image(_, siblings)) => {
                if let Some(i) = siblings.iter().position(|p| Some(p) == self.path.as_ref()) {
                    let next = if forward { i + 1 } else { i.saturating_sub(1) };
                    if let Some(path) = siblings.get(next) {
                        self.open(path.clone(), cx);
                    }
                }
            }
            None => {}
        }
    }
    fn rotate(&mut self, cx: &mut Context<Self>) {
        let Some(Document::Image(loaded, _)) = &self.document else {
            return;
        };
        if self.busy {
            return;
        }
        self.view.rotate_clockwise();
        let turns = self.view.quarter_turns;
        let loaded = loaded.clone();
        self.busy = true;
        let task = cx.background_executor().spawn(async move {
            let data = rotated_pixels(&loaded, turns);
            Raster::new(data.width(), data.height(), data.into_raw())
        });
        cx.spawn(async move |this, cx| {
            let raster = task.await;
            let _ = this.update(cx, |s, cx| {
                if let Some(old) = s.raster.replace(raster) {
                    cx.drop_image(old.image, None);
                }
                s.busy = false;
                cx.notify();
            });
        })
        .detach();
    }
    fn copy(&mut self, cx: &mut Context<Self>) {
        let Some(Document::Image(loaded, _)) = &self.document else {
            return;
        };
        if self.busy {
            return;
        }
        self.busy = true;
        let loaded = loaded.clone();
        let turns = self.view.quarter_turns;
        let task = cx.background_executor().spawn(async move {
            let data = rotated_pixels(&loaded, turns);
            let mut bytes = Cursor::new(Vec::new());
            image::write_buffer_with_format(
                &mut bytes,
                data.as_raw(),
                data.width(),
                data.height(),
                image::ExtendedColorType::Rgba8,
                image::ImageFormat::Png,
            )
            .map_err(|e| e.to_string())?;
            Ok::<_, String>(bytes.into_inner())
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.busy = false;
                match result {
                    Ok(bytes) => {
                        cx.write_to_clipboard(ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, bytes)));
                        s.status = "Image copied.".into();
                    }
                    Err(e) => s.status = e,
                }
                cx.notify();
            });
        })
        .detach();
    }
    fn pdf_layout(&self) -> (f32, f32) {
        view_model::pdf_layout(
            f32::from(self.viewport.width),
            self.view.zoom,
            self.thumbnails,
            self.text,
        )
    }
    fn refresh_pdf_layout(&mut self, cx: &mut Context<Self>) {
        if matches!(self.document, Some(Document::Pdf(_))) {
            self.scroll.scroll_to_item_strict(self.page, ScrollStrategy::Top);
            self.fetch_pages(cx);
            self.fetch_thumbnails(cx);
        }
    }
    fn zoom(&mut self, factor: f32, cx: &mut Context<Self>) {
        self.view.zoom_by(factor);
        self.refresh_pdf_layout(cx);
        cx.notify();
    }
    fn fit(&mut self, w: &Window, cx: &mut Context<Self>) {
        let bounds = w.viewport_size();
        let (width, height) = self
            .document
            .as_ref()
            .and_then(|d| match d {
                Document::Image(r, _) => Some((r.width, r.height)),
                _ => None,
            })
            .unwrap_or((1000, 1400));
        self.view.fit(
            (width, height),
            (
                f32::from(bounds.width) - if self.inspector { 300. } else { 48. },
                f32::from(bounds.height) - 160.,
            ),
        );
        cx.notify();
    }
    fn wallpaper(&mut self, restore: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(Document::Image(image, _)) = &self.document else {
            return;
        };
        let image = image.clone();
        let turns = self.view.quarter_turns;
        self.busy = true;
        self.status = if restore {
            "Restoring wallpaper…"
        } else {
            "Saving wallpaper…"
        }
        .into();
        let task = cx.background_executor().spawn(async move {
            if restore {
                crowbaros_viewer::wallpaper::restore()
            } else {
                crowbaros_viewer::wallpaper::save(&image, turns)
            }
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.busy = false;
                s.status = match result {
                    Ok(()) => {
                        eprintln!("VIEWER wallpaper={}", if restore { "restored" } else { "saved" });
                        if restore {
                            "Built-in wallpaper restored."
                        } else {
                            "Wallpaper saved."
                        }
                        .into()
                    }
                    Err(error) => error,
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        action: impl Action,
        enabled: bool,
        w: &Window,
    ) -> impl IntoElement {
        crowbar_ui::button(
            id,
            None,
            label,
            self.palette,
            w,
            crowbar_ui::ButtonVariant::Ghost,
            enabled,
        )
        .h(px(32.))
        .px_2()
        .when(enabled, |b| {
            b.on_click(move |_, w, cx| w.dispatch_action(action.boxed_clone(), cx))
        })
    }
}
fn apply_theme(p: UiPalette, cx: &mut App) {
    use gpui_component::{Theme, ThemeMode};
    Theme::change(if p.is_dark { ThemeMode::Dark } else { ThemeMode::Light }, None, cx);
    let t = Theme::global_mut(cx);
    t.font_family = FONT_UI.into();
    t.background = rgb(p.canvas).into();
    t.foreground = rgb(p.text).into();
    t.border = rgb(p.line).into();
    t.selection = rgba(p.selection_bg()).into();
    t.caret = rgb(p.focus).into();
}
impl Render for Viewer {
    fn render(&mut self, w: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for image in self.retired.drain(..) {
            let _ = w.drop_image(image);
        }
        let p = self.palette;
        let has = self.document.is_some();
        let pdf = matches!(self.document, Some(Document::Pdf(_)));
        let title = self
            .path
            .as_deref()
            .and_then(Path::file_name)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "Viewer".into());
        w.set_window_title(&format!("{title} — Viewer"));
        let content: AnyElement = if let Some(Document::Pdf(doc)) = &self.document {
            let count = doc.page_count();
            let (width, height) = self.pdf_layout();
            let pages = self
                .pages
                .iter()
                .map(|(i, r)| (*i, (r.image.clone(), r.width, r.height)))
                .collect::<BTreeMap<_, _>>();
            let failures = self.page_failures.clone();
            let list = uniform_list("pages", count, move |range, _, _| {
                range
                    .map(|i| {
                        div().h(px(height)).w_full().flex().justify_center().p_4().child(
                            if let Some((image, iw, ih)) = pages.get(&i) {
                                img(image.clone())
                                    .w(px(width))
                                    .h(px((width * (*ih as f32) / (*iw as f32)).min(height - 32.)))
                                    .object_fit(ObjectFit::Contain)
                                    .into_any_element()
                            } else {
                                div()
                                    .w(px(width))
                                    .h(px(height - 32.))
                                    .bg(rgb(0xffffff))
                                    .text_color(rgb(0x364957))
                                    .p_4()
                                    .child(format!(
                                        "Page {} · {}",
                                        i + 1,
                                        failures.get(&i).map(String::as_str).unwrap_or("Loading…")
                                    ))
                                    .into_any_element()
                            },
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .track_scroll(self.scroll.clone())
            .h_full()
            .w_full();
            let mut row = div().flex().size_full().min_h_0();
            if self.thumbnails {
                let active = self.page;
                let thumbs = self
                    .thumbnail_cache
                    .entries()
                    .iter()
                    .map(|(i, r)| (*i, r.image.clone()))
                    .collect::<BTreeMap<_, _>>();
                let entity = cx.entity();
                let failed = (0..count)
                    .filter(|page| self.thumbnail_cache.failed(*page))
                    .collect::<std::collections::BTreeSet<_>>();
                row = row.child(
                    div()
                        .id("thumbnail-scroll")
                        .w(px(144.))
                        .h_full()
                        .flex_shrink_0()
                        .bg(rgb(p.surface))
                        .on_scroll_wheel(cx.listener(|_, _, _, cx| {
                            let entity = cx.entity();
                            cx.defer(move |cx| {
                                entity.update(cx, |s, cx| {
                                    let Some(Document::Pdf(doc)) = &s.document else {
                                        return;
                                    };
                                    let scroll = s.thumbnail_scroll.0.borrow();
                                    let top = view_model::page_from_scroll(
                                        f32::from(scroll.base_handle.offset().y),
                                        160.,
                                        doc.page_count(),
                                    );
                                    drop(scroll);
                                    if top != s.thumbnail_top {
                                        s.thumbnail_top = top;
                                        s.fetch_thumbnails(cx);
                                        cx.notify();
                                    }
                                })
                            });
                        }))
                        .child(
                            uniform_list("thumbnails", count, move |range, _, _| {
                                range
                                    .map(|i| {
                                        let entity = entity.clone();
                                        div()
                                            .id(("thumb", i))
                                            .h(px(160.))
                                            .w(px(140.))
                                            .p_2()
                                            .border_2()
                                            .border_color(rgb(if i == active { p.focus } else { p.surface }))
                                            .cursor_pointer()
                                            .child(if let Some(r) = thumbs.get(&i) {
                                                img(r.clone())
                                                    .w(px(100.))
                                                    .h(px(120.))
                                                    .object_fit(ObjectFit::Contain)
                                                    .into_any_element()
                                            } else {
                                                div()
                                                    .w(px(100.))
                                                    .h(px(120.))
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .text_size(px(11.))
                                                    .text_color(rgb(p.muted))
                                                    .child(if failed.contains(&i) {
                                                        "No preview"
                                                    } else {
                                                        "Loading…"
                                                    })
                                                    .into_any_element()
                                            })
                                            .child(div().text_size(px(12.)).child(format!("Page {}", i + 1)))
                                            .on_click(move |_, _, cx| entity.update(cx, |s, cx| s.select_page(i, cx)))
                                    })
                                    .collect::<Vec<_>>()
                            })
                            .track_scroll(self.thumbnail_scroll.clone())
                            .w(px(144.))
                            .h_full()
                            .flex_shrink_0()
                            .bg(rgb(p.surface)),
                        ),
                );
            }
            row = row.child(
                div()
                    .id("page-scroll")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .on_scroll_wheel(cx.listener(|_, _, _, cx| {
                        let entity = cx.entity();
                        cx.defer(move |cx| {
                            entity.update(cx, |s, cx| {
                                let page = {
                                    let scroll = s.scroll.0.borrow();
                                    let count = match &s.document {
                                        Some(Document::Pdf(doc)) => doc.page_count(),
                                        _ => 0,
                                    };
                                    // GPUI's last_item_size holds the viewport,
                                    // not the height of one PDF page row.
                                    view_model::page_from_scroll(
                                        f32::from(scroll.base_handle.offset().y),
                                        s.pdf_layout().1,
                                        count,
                                    )
                                };
                                if page != s.page {
                                    s.page = page;
                                    s.text_page = None;
                                    s.page_text.clear();
                                    s.fetch_pages(cx);
                                    cx.notify();
                                }
                            });
                        });
                    }))
                    .child(list),
            );
            if self.text {
                row = row.child(
                    div()
                        .w(px(320.))
                        .flex_shrink_0()
                        .h_full()
                        .p_4()
                        .bg(rgb(p.surface))
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(div().text_size(px(16.)).child("Page text"))
                                .child(self.button(
                                    "copy-page-text",
                                    "Copy page",
                                    CopyPageText,
                                    self.text_page == Some(self.page) && !self.page_text.trim().is_empty(),
                                    w,
                                )),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(p.muted))
                                .child("Select with Shift + arrows · Ctrl+C copies"),
                        )
                        .child(if self.text_page != Some(self.page) {
                            div().pt_3().child("Loading page text…").into_any_element()
                        } else if self.page_text.trim().is_empty() {
                            div()
                                .pt_3()
                                .child("No selectable text on this page.")
                                .into_any_element()
                        } else {
                            div()
                                .flex_1()
                                .min_h_0()
                                .child(
                                    Input::new(&self.text_field)
                                        .disabled(true)
                                        .appearance(false)
                                        .h_full()
                                        .text_color(rgb(p.text))
                                        .bg(rgb(p.surface)),
                                )
                                .into_any_element()
                        }),
                );
            }
            row.into_any_element()
        } else if let Some(r) = &self.raster {
            div()
                .id("image-scroll")
                .size_full()
                .overflow_scroll()
                .track_scroll(&self.image_scroll)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|s, e: &MouseDownEvent, _, _| s.drag = Some((e.position, s.image_scroll.offset()))),
                )
                .on_mouse_up(MouseButton::Left, cx.listener(|s, _, _, _| s.drag = None))
                .on_mouse_move(cx.listener(|s, e: &MouseMoveEvent, _, cx| {
                    if e.pressed_button == Some(MouseButton::Left)
                        && let Some((start, offset)) = s.drag
                    {
                        s.image_scroll.set_offset(offset + e.position - start);
                        cx.notify();
                    }
                }))
                .child(
                    div()
                        .min_w_full()
                        .min_h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p_4()
                        .child(
                            img(r.image.clone())
                                .w(px(r.width as f32 * self.view.zoom))
                                .h(px(r.height as f32 * self.view.zoom))
                                .flex_shrink_0(),
                        ),
                )
                .into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4()
                .child(div().text_size(px(28.)).child(if self.busy {
                    "Opening…"
                } else {
                    "Images and documents"
                }))
                .child(
                    div()
                        .text_color(rgb(p.muted))
                        .child("Open a file to look closer. Ctrl+O"),
                )
                .child(self.button("empty-open", "Open file", Open, !self.busy, w))
                .into_any_element()
        };
        let mut body = div()
            .flex()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(div().flex_1().min_w_0().h_full().child(content));
        if self.inspector
            && let Some(Document::Image(image, _)) = &self.document
        {
            body = body.child(
                div()
                    .id("metadata")
                    .w(px(240.))
                    .flex_shrink_0()
                    .h_full()
                    .overflow_y_scroll()
                    .bg(rgb(p.surface))
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child("Image information")
                    .child(self.button(
                        "restore-wallpaper",
                        "Restore built-in wallpaper",
                        RestoreWallpaper,
                        !self.busy,
                        w,
                    ))
                    .children(image.metadata.iter().map(|(key, value)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_size(px(12.)).text_color(rgb(p.muted)).child(key.clone()))
                            .child(div().font_family(FONT_MONO).text_size(px(11.)).child(value.clone()))
                    })),
            );
        }
        let mut root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(p.canvas))
            .text_color(rgb(p.text))
            .font_family(FONT_UI)
            .text_size(px(13.))
            .key_context("Viewer")
            .track_focus(&self.focus)
            .on_action(cx.listener(|s, _: &Open, w, cx| s.ask(Prompt::Open, w, cx)))
            .on_action(cx.listener(|_, _: &Quit, _, cx| cx.quit()))
            .on_action(cx.listener(|s, _: &ZoomIn, _, cx| s.zoom(1.2, cx)))
            .on_action(cx.listener(|s, _: &ZoomOut, _, cx| s.zoom(1. / 1.2, cx)))
            .on_action(cx.listener(|s, _: &Fit, w, cx| {
                if matches!(s.document, Some(Document::Pdf(_))) {
                    s.view.actual_size();
                    s.refresh_pdf_layout(cx);
                    cx.notify();
                } else {
                    s.fit(w, cx);
                }
            }))
            .on_action(cx.listener(|s, _: &Actual, _, cx| {
                s.view.actual_size();
                s.refresh_pdf_layout(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &Rotate, _, cx| s.rotate(cx)))
            .on_action(cx.listener(|s, _: &Inspector, _, cx| {
                s.inspector = !s.inspector;
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &Thumbnails, _, cx| {
                s.thumbnails = !s.thumbnails;
                s.refresh_pdf_layout(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &Previous, _, cx| s.step(false, cx)))
            .on_action(cx.listener(|s, _: &Next, _, cx| s.step(true, cx)))
            .on_action(cx.listener(|s, _: &Find, w, cx| s.ask(Prompt::Find, w, cx)))
            .on_action(cx.listener(|s, _: &GoTo, w, cx| s.ask(Prompt::Page, w, cx)))
            .on_action(cx.listener(|s, _: &CopyImage, _, cx| s.copy(cx)))
            .on_action(cx.listener(|s, _: &SetWallpaper, _, cx| s.wallpaper(false, cx)))
            .on_action(cx.listener(|s, _: &RestoreWallpaper, _, cx| s.wallpaper(true, cx)))
            .on_action(cx.listener(|s, _: &CopyPageText, _, cx| {
                if s.text_page == Some(s.page) && !s.page_text.trim().is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(s.page_text.clone()));
                    s.status = "Page text copied.".into();
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|s, _: &Text, w, cx| {
                s.text = !s.text;
                s.refresh_pdf_layout(cx);
                if s.text {
                    s.text_field.update(cx, |field, cx| field.focus(w, cx));
                    s.text_focus_pending = s.text_page != Some(s.page);
                } else {
                    s.focus.focus(w);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|s, _: &NextMatch, w, cx| s.move_match(false, w, cx)))
            .on_action(cx.listener(|s, _: &PreviousMatch, w, cx| s.move_match(true, w, cx)))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(|s, _: &Dismiss, w, cx| {
                s.prompt = None;
                s.focus.focus(w);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(48.))
                    .flex_shrink_0()
                    .px_3()
                    .border_b_1()
                    .border_color(rgb(p.line))
                    .child(self.button("open", "Open", Open, !self.busy, w))
                    .child(div().flex_1().min_w_0().overflow_hidden().child(title))
                    .child(self.button("prev", "Previous", Previous, has && !self.busy, w))
                    .child(self.button("next", "Next", Next, has && !self.busy, w))
                    .child(self.button("minus", "Zoom out", ZoomOut, has, w))
                    .child(self.button("fit", "Fit", Fit, has, w))
                    .child(self.button("plus", "Zoom in", ZoomIn, has, w)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(40.))
                    .flex_shrink_0()
                    .px_3()
                    .bg(rgb(p.surface))
                    .when(pdf, |d| {
                        d.child(self.button("thumbs", "Thumbnails", Thumbnails, true, w))
                            .child(self.button("jump", "Go to page", GoTo, true, w))
                            .child(self.button("find", "Find", Find, !self.busy, w))
                            .child(self.button("text", "Select text", Text, true, w))
                    })
                    .when(!pdf, |d| {
                        d.child(self.button("actual", "Actual size", Actual, has, w))
                            .child(self.button("rotate", "Rotate", Rotate, has && !self.busy, w))
                            .child(self.button("info", "Information", Inspector, has, w))
                            .child(self.button("copy", "Copy image", CopyImage, has && !self.busy, w))
                            .child(self.button("wallpaper", "Set as wallpaper", SetWallpaper, has && !self.busy, w))
                    }),
            );
        if let Some(hit) = self.matches.get(self.match_index) {
            root = root.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(40.))
                    .flex_shrink_0()
                    .px_3()
                    .bg(rgb(p.surface))
                    .border_b_1()
                    .border_color(rgb(p.line))
                    .child(self.button("previous-match", "Previous match", PreviousMatch, true, w))
                    .child(self.button("next-match", "Next match", NextMatch, true, w))
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(12.))
                            .flex_shrink_0()
                            .child(format!("{} / {}", self.match_index + 1, self.matches.len())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_color(rgb(p.muted))
                            .child(hit.excerpt.clone()),
                    ),
            );
        }
        if let Some(prompt) = self.prompt {
            root = root.child(
                div()
                    .key_context("ViewerPrompt")
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_3()
                    .child(match prompt {
                        Prompt::Open => "File path",
                        Prompt::Find => "Find in document",
                        Prompt::Page => "Page number",
                    })
                    .child(Input::new(&self.field))
                    .child(self.button("confirm", "Go", Confirm, true, w))
                    .child(self.button("cancel", "Cancel", Dismiss, true, w)),
            );
        }
        root.child(body).child(
            div()
                .h(px(32.))
                .flex_shrink_0()
                .px_4()
                .flex()
                .items_center()
                .justify_between()
                .border_t_1()
                .border_color(rgb(p.line))
                .text_color(rgb(p.muted))
                .text_size(px(12.))
                .child(div().flex_1().min_w_0().overflow_hidden().child(self.status.clone()))
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .child(if let Some(Document::Pdf(doc)) = &self.document {
                            format!(
                                "Page {} / {} · {:.0}%",
                                self.page + 1,
                                doc.page_count(),
                                self.view.zoom * 100.
                            )
                        } else {
                            format!("{:.0}%", self.view.zoom * 100.)
                        }),
                ),
        )
    }
}
fn rotated_pixels(loaded: &LoadedImage, turns: u8) -> image::RgbaImage {
    let data = image::RgbaImage::from_raw(loaded.width, loaded.height, loaded.rgba.clone()).expect("validated raster");
    match turns % 4 {
        1 => image::imageops::rotate90(&data),
        2 => image::imageops::rotate180(&data),
        3 => image::imageops::rotate270(&data),
        _ => data,
    }
}
fn readable_pdf_text(result: Result<String, String>) -> String {
    result.unwrap_or_else(|error| format!("Text is unavailable for this page. {error}"))
}
fn is_pdf(path: &Path) -> bool {
    path.extension().is_some_and(|s| s.eq_ignore_ascii_case("pdf"))
        || crowbaros_viewer::pdf_document::has_pdf_header(path)
}
fn main() {
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    if first.as_deref() == Some(std::ffi::OsStr::new("--version")) {
        println!("crowbaros-viewer {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if first.as_deref() == Some(std::ffi::OsStr::new("--inspect")) {
        let result = args
            .next()
            .ok_or("A file path is required.".to_string())
            .and_then(|path| {
                let path = view_model::parse_open_path(&path)?;
                if is_pdf(&path) {
                    let d = PdfDocument::open(&path)?;
                    Ok(serde_json::json!({"kind":"pdf","pages":d.page_count()}))
                } else {
                    let d = image_document::load_image(&path)?;
                    Ok(serde_json::json!({"kind":"image","width":d.width,"height":d.height}))
                }
            });
        match result {
            Ok(v) => println!("{v}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let path = first.map(PathBuf::from);
    Application::new()
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx| {
            gpui_component::init(cx);
            load_fonts(cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-o", Open, Some("Viewer")),
                KeyBinding::new("ctrl-q", Quit, Some("Viewer")),
                KeyBinding::new("ctrl-=", ZoomIn, Some("Viewer")),
                KeyBinding::new("ctrl-+", ZoomIn, Some("Viewer")),
                KeyBinding::new("ctrl--", ZoomOut, Some("Viewer")),
                KeyBinding::new("ctrl-0", Fit, Some("Viewer")),
                KeyBinding::new("ctrl-1", Actual, Some("Viewer")),
                KeyBinding::new("ctrl-r", Rotate, Some("Viewer")),
                KeyBinding::new("ctrl-shift-w", SetWallpaper, Some("Viewer")),
                KeyBinding::new("ctrl-alt-w", RestoreWallpaper, Some("Viewer")),
                KeyBinding::new("ctrl-i", Inspector, Some("Viewer")),
                KeyBinding::new("ctrl-b", Thumbnails, Some("Viewer")),
                KeyBinding::new("ctrl-t", Text, Some("Viewer")),
                KeyBinding::new("left", Previous, Some("Viewer && !Input")),
                KeyBinding::new("right", Next, Some("Viewer && !Input")),
                KeyBinding::new("pageup", Previous, Some("Viewer && !Input")),
                KeyBinding::new("pagedown", Next, Some("Viewer && !Input")),
                KeyBinding::new("f3", NextMatch, Some("Viewer")),
                KeyBinding::new("shift-f3", PreviousMatch, Some("Viewer")),
                KeyBinding::new("ctrl-shift-c", CopyPageText, Some("Viewer")),
                KeyBinding::new("ctrl-f", Find, Some("Viewer")),
                KeyBinding::new("ctrl-f", Find, Some("Viewer > Input")),
                KeyBinding::new("ctrl-g", GoTo, Some("Viewer")),
                KeyBinding::new("ctrl-c", CopyImage, Some("Viewer && !Input")),
                KeyBinding::new("enter", Confirm, Some("ViewerPrompt > Input")),
                KeyBinding::new("escape", Dismiss, Some("Viewer")),
            ]);
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let bounds = Bounds::centered(None, size(px(1000.), px(720.)), cx);
            if let Err(e) = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    app_id: Some("com.crowbaros.Viewer".into()),
                    window_min_size: Some(size(px(720.), px(480.))),
                    ..Default::default()
                },
                move |w, cx| {
                    let v = cx.new(|cx| Viewer::new(path, w, cx));
                    cx.new(|cx| Root::new(v, w, cx))
                },
            ) {
                eprintln!("Could not open Viewer: {e}");
                cx.quit();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pdf_routing_uses_contents_when_the_name_has_no_pdf_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("download");
        std::fs::write(&path, b"%PDF-1.4\n").unwrap();
        assert!(is_pdf(&path));
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(!is_pdf(&path));
    }
    #[test]
    fn optional_pdf_text_failure_leaves_a_readable_notice() {
        assert!(readable_pdf_text(Err("Copying is restricted.".into())).contains("Text is unavailable"));
        assert_eq!(readable_pdf_text(Ok("Page content".into())), "Page content");
    }
    #[test]
    fn copying_and_displaying_use_the_same_rotation() {
        let loaded = LoadedImage {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
            metadata: vec![],
        };
        let rotated = rotated_pixels(&loaded, 1);
        assert_eq!(rotated.dimensions(), (1, 2));
        assert_eq!(rotated.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(rotated.get_pixel(0, 1).0, [0, 0, 255, 255]);
        assert_eq!(rotated_pixels(&loaded, 4).into_raw(), loaded.rgba);
    }
}
