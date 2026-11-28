// Adapted from GPUI 0.2.2 examples/input.rs; Copyright Zed Industries, Apache-2.0.
//! The message box of the Agent's surface: text of many lines, wrapped to
//! its width. It grows with what it holds, up to a limit, and then scrolls.
//!
//! Enter is not the box's: whoever holds it decides what Enter does.
//! Shift+Enter begins a new line. While a list is open above the box
//! (`list_open`), Up, Down, Tab, and Escape pass through to whoever holds it,
//! and Left and Right too while the list has a rail (`rail_open`).
use std::ops::Range;

use gpui::{prelude::*, *};
use unicode_segmentation::*;

actions!(
    composer,
    [
        Backspace,
        Delete,
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        SelectAll,
        Home,
        End,
        Newline,
        Paste,
        Cut,
        Copy,
        Take,
        Dismiss,
    ]
);

/// The size of the box's text, and the height of each of its lines.
const TEXT_SIZE: f32 = 13.5;
const LINE_HEIGHT: f32 = 20.;
/// The most lines the box grows to before it scrolls.
pub const MAX_LINES: usize = 8;

/// Where the box's text was last laid out: each line the person typed, as
/// the rows it wrapped to.
#[derive(Default)]
struct Laid {
    lines: Vec<WrappedLine>,
    /// Where each typed line starts in the text.
    starts: Vec<usize>,
    bounds: Option<Bounds<Pixels>>,
}

/// A row of the laid out text: the typed line it is of, and the part of
/// that line it shows.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Row {
    line: usize,
    start: usize,
    end: usize,
}

impl Laid {
    /// Where a typed line's rows begin within it.
    fn breaks(line: &WrappedLine) -> Vec<usize> {
        let mut breaks = vec![0];
        for boundary in line.wrap_boundaries() {
            if let Some(glyph) = line
                .unwrapped_layout
                .runs
                .get(boundary.run_ix)
                .and_then(|run| run.glyphs.get(boundary.glyph_ix))
            {
                breaks.push(glyph.index);
            }
        }
        breaks
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (line, laid) in self.lines.iter().enumerate() {
            let breaks = Self::breaks(laid);
            for (at, start) in breaks.iter().enumerate() {
                let end = breaks.get(at + 1).copied().unwrap_or(laid.len());
                rows.push(Row {
                    line,
                    start: *start,
                    end,
                });
            }
        }
        rows
    }

    /// The row an offset in the text is on, and how far along it.
    fn place(&self, offset: usize) -> Option<(usize, Pixels)> {
        let rows = self.rows();
        let line = self.starts.iter().rposition(|start| *start <= offset)?;
        let local = (offset - self.starts[line]).min(self.lines.get(line)?.len());
        // An offset where a line wraps is at the start of the row after.
        let row = rows
            .iter()
            .rposition(|row| row.line == line && row.start <= local)?;
        let layout = &self.lines[line].unwrapped_layout;
        Some((row, layout.x_for_index(local) - layout.x_for_index(rows[row].start)))
    }

    /// The offset in the text nearest a place on a row.
    fn offset(&self, row: usize, x: Pixels) -> usize {
        let rows = self.rows();
        let Some(row) = rows.get(row.min(rows.len().saturating_sub(1))) else {
            return 0;
        };
        let layout = &self.lines[row.line].unwrapped_layout;
        let local = layout
            .closest_index_for_x(x + layout.x_for_index(row.start))
            .clamp(row.start, row.end);
        self.starts[row.line] + local
    }
}

pub struct Composer {
    pub palette: crate::appearance::Palette,
    /// A list is open above the box, and takes Up, Down, Tab, and Escape.
    pub list_open: bool,
    /// The list has a rail beside it, which takes Left and Right.
    pub rail_open: bool,
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    laid: Laid,
    /// How many rows the text wrapped to, when it was last laid out.
    rows: usize,
    scroll_y: Pixels,
    /// True after mouse-wheel scrolling until the caret moves or text changes.
    manual_scroll: bool,
    /// Where Up and Down keep the caret, across rows of other lengths.
    goal_x: Option<Pixels>,
    is_selecting: bool,
    max_len: usize,
}

impl Composer {
    pub fn new(placeholder: impl Into<SharedString>, max_len: usize, cx: &mut Context<Self>) -> Self {
        Self {
            palette: crate::appearance::Palette::default(),
            list_open: false,
            rail_open: false,
            focus_handle: cx.focus_handle(),
            content: "".into(),
            placeholder: placeholder.into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            laid: Laid::default(),
            rows: 1,
            scroll_y: px(0.),
            manual_scroll: false,
            goal_x: None,
            is_selecting: false,
            max_len,
        }
    }

    pub fn text(&self) -> String {
        self.content.to_string()
    }

    /// Puts `text` in the box, with the caret after it.
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text: SharedString = text.into();
        if self.content == text {
            return;
        }
        let end = text.len();
        self.content = text;
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.goal_x = None;
        self.scroll_y = px(0.);
        self.manual_scroll = false;
        cx.notify();
    }


    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.selection_reversed = false;
        self.manual_scroll = false;
        cx.notify()
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.manual_scroll = false;
        cx.notify()
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    /// The offset a row up or down from the caret, keeping to the place
    /// along the row the caret set out from.
    fn vertical(&mut self, down: bool) -> usize {
        let Some((row, x)) = self.laid.place(self.cursor_offset()) else {
            return self.cursor_offset();
        };
        let x = *self.goal_x.get_or_insert(x);
        let rows = self.laid.rows().len();
        match (down, row) {
            (false, 0) => 0,
            (true, last) if last + 1 >= rows => self.content.len(),
            (false, _) => self.laid.offset(row - 1, x),
            (true, _) => self.laid.offset(row + 1, x),
        }
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.rail_open {
            return cx.propagate();
        }
        self.goal_x = None;
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.rail_open {
            return cx.propagate();
        }
        self.goal_x = None;
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        if self.list_open {
            return cx.propagate();
        }
        let to = self.vertical(false);
        self.move_to(to, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        if self.list_open {
            return cx.propagate();
        }
        let to = self.vertical(true);
        self.move_to(to, cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let to = self.vertical(false);
        self.select_to(to, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let to = self.vertical(true);
        self.select_to(to, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    /// The start and the end of the row the caret is on.
    fn row_ends(&self) -> (usize, usize) {
        let offset = self.cursor_offset();
        let Some((row, _)) = self.laid.place(offset) else {
            return (0, self.content.len());
        };
        let rows = self.laid.rows();
        let start = self.laid.starts[rows[row].line];
        (start + rows[row].start, start + rows[row].end)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(self.row_ends().0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.goal_x = None;
        self.move_to(self.row_ends().1, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.previous_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.select_to(self.next_boundary(self.cursor_offset()), cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn take(&mut self, _: &Take, window: &mut Window, cx: &mut Context<Self>) {
        // Tab takes a row of an open list. With none open it moves on.
        if self.list_open {
            cx.propagate();
        } else {
            window.focus_next();
        }
    }

    fn dismiss(&mut self, _: &Dismiss, _: &mut Window, cx: &mut Context<Self>) {
        cx.propagate();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // What was copied keeps its lines.
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.replace_text_in_range(None, &text, window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        let Some(bounds) = self.laid.bounds else {
            return 0;
        };
        let y = (position.y - bounds.top() + self.scroll_y).max(px(0.));
        let row = (f32::from(y) / LINE_HEIGHT).floor() as usize;
        self.laid.offset(row, (position.x - bounds.left()).max(px(0.)))
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.is_selecting = true;
        self.goal_x = None;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_scroll_wheel(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(LINE_HEIGHT)).y;
        let max_scroll = px((self.rows as f32 - MAX_LINES as f32).max(0.) * LINE_HEIGHT);
        if max_scroll <= px(0.) {
            return;
        }
        let next = (self.scroll_y - delta).clamp(px(0.), max_scroll);
        if next != self.scroll_y {
            self.scroll_y = next;
            self.manual_scroll = true;
            cx.notify();
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn replace(&mut self, range: Range<usize>, new_text: &str) -> Option<usize> {
        let (next, inserted) = replace_bounded(&self.content, range, new_text, self.max_len)?;
        self.content = next.into();
        self.goal_x = None;
        self.manual_scroll = false;
        Some(inserted)
    }
}

/// Replaces `range` in `content` with `new_text`, truncating `new_text` at the
/// largest UTF-8 character boundary that fits within `max_len`. Returns the new
/// string and the actual byte count inserted, or `None` if `new_text` is
/// non-empty and nothing fits.
fn replace_bounded(content: &str, range: Range<usize>, new_text: &str, max_len: usize) -> Option<(String, usize)> {
    let base_len = content.len().saturating_sub(range.len());
    let allowed = max_len.saturating_sub(base_len);
    let insert = if new_text.len() > allowed {
        if allowed == 0 {
            return None;
        }
        let mut end = allowed;
        while end > 0 && !new_text.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            return None;
        }
        &new_text[..end]
    } else {
        new_text
    };
    let next = content[0..range.start].to_owned() + insert + &content[range.end..];
    Some((next, insert.len()))
}

impl EntityInputHandler for Composer {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _window: &mut Window, _cx: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range.as_ref().map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let Some(inserted) = self.replace(range.clone(), new_text) else {
            return;
        };
        let end = range.start + inserted;
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range.take();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        let Some(inserted) = self.replace(range.clone(), new_text) else {
            return;
        };
        let end = range.start + inserted;
        if inserted > 0 {
            self.marked_range = Some(range.start..end);
        } else {
            self.marked_range = None;
        }
        let max = self.content.len();
        self.selected_range = new_selected_range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .map(|new_range| {
                let s = (new_range.start + range.start).min(max);
                let e = (new_range.end + range.start).min(max);
                s..e.max(s)
            })
            .unwrap_or(end..end);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let (row, x) = self.laid.place(range.start)?;
        let top = bounds.top() + px(row as f32 * LINE_HEIGHT) - self.scroll_y;
        Some(Bounds::from_corners(
            point(bounds.left() + x, top),
            point(bounds.left() + x, top + px(LINE_HEIGHT)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.laid.bounds?;
        bounds.localize(&point)?;
        Some(self.offset_to_utf16(self.index_for_mouse_position(point)))
    }
}

struct ComposerElement {
    input: Entity<Composer>,
}

struct PrepaintState {
    lines: Vec<WrappedLine>,
    starts: Vec<usize>,
    rows: usize,
    scroll_y: Pixels,
    cursor: Option<PaintQuad>,
    selection: Vec<PaintQuad>,
}

impl IntoElement for ComposerElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ComposerElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // As tall as the rows the text wrapped to when it was last laid
        // out: a row more or fewer is found as it is painted, and laid out
        // again.
        let rows = self.input.read(cx).rows.clamp(1, MAX_LINES);
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(rows as f32 * LINE_HEIGHT).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let content = input.content.clone();
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let style = window.text_style();
        let line_height = px(LINE_HEIGHT);

        let (display_text, text_color) = if content.is_empty() {
            (input.placeholder.clone(), rgb(input.palette.muted).into())
        } else {
            (content.clone(), style.color)
        };
        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs: Vec<TextRun> = match input.marked_range.as_ref().filter(|_| !content.is_empty()) {
            Some(marked) => vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            None => vec![run],
        };
        let wrap = (bounds.size.width - px(2.)).max(px(20.));
        let lines: Vec<WrappedLine> = window
            .text_system()
            .shape_text(display_text.clone(), px(TEXT_SIZE), &runs, Some(wrap), None)
            .map(|lines| lines.into_iter().collect())
            .unwrap_or_default();
        let mut starts = Vec::with_capacity(lines.len());
        let mut at = 0;
        for line in &lines {
            starts.push(at);
            at += line.len() + 1;
        }
        let laid = Laid {
            lines,
            starts,
            bounds: Some(bounds),
        };
        let all_rows = laid.rows();
        let rows = all_rows.len().max(1);

        // The placeholder is not the text: the caret stays at the start.
        let (caret_row, caret_x) = if content.is_empty() {
            (0, px(0.))
        } else {
            laid.place(cursor).unwrap_or((0, px(0.)))
        };
        let caret_top = px(caret_row as f32 * LINE_HEIGHT);
        let height = bounds.size.height;
        let max_scroll = (px(rows as f32 * LINE_HEIGHT) - height).max(px(0.));
        let scroll_y = if input.manual_scroll {
            input.scroll_y.clamp(px(0.), max_scroll)
        } else {
            input
                .scroll_y
                .min(caret_top)
                .max(caret_top + line_height - height)
                .min(max_scroll)
                .max(px(0.))
        };

        let mut selection = Vec::new();
        let mut caret = None;
        if selected_range.is_empty() || content.is_empty() {
            caret = Some(fill(
                Bounds::new(
                    point(bounds.left() + caret_x, bounds.top() + caret_top - scroll_y),
                    size(px(2.), line_height),
                ),
                rgb(input.palette.focus),
            ));
        } else {
            for (at, row) in all_rows.iter().enumerate() {
                let start = laid.starts[row.line];
                let from = selected_range.start.max(start + row.start);
                let to = selected_range.end.min(start + row.end);
                // A row whose line end is selected shows it past its text.
                let takes_end = selected_range.end > start + row.end && selected_range.start <= start + row.end;
                if from > to || (from == to && !takes_end) {
                    continue;
                }
                let layout = &laid.lines[row.line].unwrapped_layout;
                let origin = layout.x_for_index(row.start);
                let left = layout.x_for_index(from - start) - origin;
                let right = layout.x_for_index(to - start) - origin + if takes_end { px(6.) } else { px(0.) };
                let top = bounds.top() + px(at as f32 * LINE_HEIGHT) - scroll_y;
                selection.push(fill(
                    Bounds::from_corners(
                        point(bounds.left() + left, top),
                        point(bounds.left() + right, top + line_height),
                    ),
                    rgba((input.palette.accent << 8) | 0x50),
                ));
            }
        }
        PrepaintState {
            lines: laid.lines,
            starts: laid.starts,
            rows,
            scroll_y,
            cursor: caret,
            selection,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(&focus_handle, ElementInputHandler::new(bounds, self.input.clone()), cx);
        let line_height = px(LINE_HEIGHT);
        let scroll_y = prepaint.scroll_y;
        let lines = std::mem::take(&mut prepaint.lines);
        let focused = focus_handle.is_focused(window);
        let selection = std::mem::take(&mut prepaint.selection);
        let caret = prepaint.cursor.take();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for quad in selection {
                window.paint_quad(quad);
            }
            let mut top = bounds.top() - scroll_y;
            for line in &lines {
                let _ = line.paint(point(bounds.left(), top), line_height, TextAlign::Left, None, window, cx);
                top += px((line.wrap_boundaries().len() + 1) as f32 * LINE_HEIGHT);
            }
            if focused && let Some(caret) = caret {
                window.paint_quad(caret);
            }
        });

        let rows = prepaint.rows;
        let starts = std::mem::take(&mut prepaint.starts);
        self.input.update(cx, |input, cx| {
            input.scroll_y = scroll_y;
            input.laid = Laid {
                lines,
                starts,
                bounds: Some(bounds),
            };
            if input.rows != rows {
                // The box is as tall as its rows: lay it out again.
                input.rows = rows;
                cx.notify();
            }
        });
    }
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .w_full()
            .key_context("Composer")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::take))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .text_color(rgb(self.palette.text))
            .line_height(px(LINE_HEIGHT))
            .text_size(px(TEXT_SIZE))
            .child(
                div()
                    .w_full()
                    .py(px(3.))
                    .overflow_hidden()
                    .child(ComposerElement { input: cx.entity() }),
            )
    }
}

impl Focusable for Composer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub fn bind(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("Composer")),
        KeyBinding::new("delete", Delete, Some("Composer")),
        KeyBinding::new("left", Left, Some("Composer")),
        KeyBinding::new("right", Right, Some("Composer")),
        KeyBinding::new("up", Up, Some("Composer")),
        KeyBinding::new("down", Down, Some("Composer")),
        KeyBinding::new("shift-left", SelectLeft, Some("Composer")),
        KeyBinding::new("shift-right", SelectRight, Some("Composer")),
        KeyBinding::new("shift-up", SelectUp, Some("Composer")),
        KeyBinding::new("shift-down", SelectDown, Some("Composer")),
        KeyBinding::new("ctrl-a", SelectAll, Some("Composer")),
        KeyBinding::new("ctrl-v", Paste, Some("Composer")),
        KeyBinding::new("ctrl-c", Copy, Some("Composer")),
        KeyBinding::new("ctrl-x", Cut, Some("Composer")),
        KeyBinding::new("home", Home, Some("Composer")),
        KeyBinding::new("end", End, Some("Composer")),
        KeyBinding::new("shift-enter", Newline, Some("Composer")),
        KeyBinding::new("tab", Take, Some("Composer")),
        KeyBinding::new("escape", Dismiss, Some("Composer")),
    ]);
}

#[cfg(test)]
mod tests {
    use super::replace_bounded;

    #[test]
    fn oversized_paste_truncates_on_utf8_boundary_and_reports_inserted_bytes() {
        // ASCII truncation at max_len.
        let (next, inserted) = replace_bounded("ab", 2..2, "cdefgh", 5).unwrap();
        assert_eq!(next, "abcde");
        assert_eq!(inserted, 3);
        assert!(2 + inserted <= next.len());
        assert!(next.is_char_boundary(2 + inserted));

        // Multi-byte UTF-8 truncation never splits a character.
        let (next, inserted) = replace_bounded("a", 1..1, "ééé", 4).unwrap();
        assert_eq!(next, "aé");
        assert_eq!(inserted, "é".len());
        assert_eq!(1 + inserted, next.len());
        assert!(next.is_char_boundary(1 + inserted));

        // Full buffer rejects further insertion while still allowing deletion.
        assert_eq!(replace_bounded("abcde", 5..5, "x", 5), None);
        let (next, inserted) = replace_bounded("abcde", 2..5, "", 5).unwrap();
        assert_eq!(next, "ab");
        assert_eq!(inserted, 0);
    }
}
