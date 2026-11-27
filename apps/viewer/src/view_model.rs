// SPDX-License-Identifier: MIT
//! Small state transitions shared by Viewer input and asynchronous page work.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::ops::Range;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

/// One bounded batch belongs to a document generation, never to whichever
/// document happens to be visible when a worker finishes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRequest {
    generation: u64,
    serial: u64,
    pub pages: Vec<usize>,
}

#[derive(Default)]
pub struct PageCache<T> {
    generation: u64,
    serial: u64,
    pending: Option<PageRequest>,
    wanted: Range<usize>,
    retained: Range<usize>,
    entries: BTreeMap<usize, T>,
    failed: BTreeSet<usize>,
}

impl<T> PageCache<T> {
    pub fn new() -> Self {
        Self {
            generation: 0,
            serial: 0,
            pending: None,
            wanted: 0..0,
            retained: 0..0,
            entries: BTreeMap::new(),
            failed: BTreeSet::new(),
        }
    }

    pub fn reset(&mut self) -> Vec<T> {
        self.generation = self.generation.wrapping_add(1);
        self.wanted = 0..0;
        self.retained = 0..0;
        self.failed.clear();
        // Keep the old worker occupying its slot until it returns. Repeated
        // document changes must not create an unbounded set of Poppler jobs.
        std::mem::take(&mut self.entries).into_values().collect()
    }

    pub fn set_window(&mut self, wanted: Range<usize>, retained: Range<usize>) -> Vec<T> {
        self.wanted = wanted;
        self.retained = retained;
        self.failed.retain(|page| self.retained.contains(page));
        let removed: Vec<_> = self
            .entries
            .keys()
            .copied()
            .filter(|page| !self.retained.contains(page))
            .collect();
        removed
            .into_iter()
            .filter_map(|page| self.entries.remove(&page))
            .collect()
    }

    pub fn request(&mut self, batch_size: usize) -> Option<PageRequest> {
        if self.pending.is_some() {
            return None;
        }
        let pages: Vec<_> = self
            .wanted
            .clone()
            .filter(|page| !self.entries.contains_key(page) && !self.failed.contains(page))
            .take(batch_size)
            .collect();
        if pages.is_empty() {
            return None;
        }
        self.serial = self.serial.wrapping_add(1);
        let request = PageRequest {
            generation: self.generation,
            serial: self.serial,
            pages,
        };
        self.pending = Some(request.clone());
        Some(request)
    }

    /// Release only this worker's slot. An old or duplicate completion cannot
    /// clear a newer request, and a previous document cannot populate this one.
    pub fn finish(&mut self, request: &PageRequest) -> bool {
        if self.pending.as_ref() != Some(request) {
            return false;
        }
        self.pending = None;
        request.generation == self.generation
    }

    /// Return the displaced or unwanted raster so its GPU allocation can be
    /// released as well as its CPU pixels.
    pub fn insert(&mut self, page: usize, value: T) -> Option<T> {
        if self.retained.contains(&page) {
            self.entries.insert(page, value)
        } else {
            Some(value)
        }
    }
    pub fn fail(&mut self, page: usize) {
        if self.retained.contains(&page) {
            self.failed.insert(page);
        }
    }
    pub fn failed(&self, page: usize) -> bool {
        self.failed.contains(&page)
    }
    pub fn entries(&self) -> &BTreeMap<usize, T> {
        &self.entries
    }
}

/// Thumbnail work follows the rail's viewport independently of the document
/// page. At most fourteen small rasters survive, even for a huge window.
pub fn thumbnail_windows(top: usize, count: usize, viewport_height: f32) -> (Range<usize>, Range<usize>) {
    if count == 0 {
        return (0..0, 0..0);
    }
    let top = top.min(count - 1);
    let visible = (viewport_height.max(0.0) / 160.0).ceil() as usize;
    let end = top.saturating_add(visible.saturating_add(1).clamp(3, 11)).min(count);
    (
        top.saturating_sub(1)..end,
        top.saturating_sub(2)..end.saturating_add(1).min(count),
    )
}

/// The logical page is the anchor across a changed row height. Convert back
/// from the list's pixel offset only for a real scroll, not for a zoom/layout.
pub fn page_from_scroll(offset_y: f32, row_height: f32, count: usize) -> usize {
    if !offset_y.is_finite() || !row_height.is_finite() || row_height <= 0.0 {
        return 0;
    }
    ((-offset_y / row_height).floor().max(0.0) as usize).min(count.saturating_sub(1))
}

pub fn pdf_layout(viewport_width: f32, zoom: f32, thumbnails: bool, text: bool) -> (f32, f32) {
    let width =
        (viewport_width - if thumbnails { 192.0 } else { 40.0 } - if text { 320.0 } else { 0.0 }).max(200.0) * zoom;
    (width, width * 1.414 + 32.0)
}

/// Pages worth requesting around the page in view. This always contains at
/// most four entries, even for an invalid index or a document with no pages.
pub fn desired_pdf_pages(page: usize, count: usize) -> Range<usize> {
    page_range(page, count, 1, 2)
}

/// Retain a little more than the request range so a short reverse scroll does
/// not immediately rerender pages. The raster cache is bounded to six pages.
pub fn retained_pdf_pages(page: usize, count: usize) -> Range<usize> {
    page_range(page, count, 2, 3)
}

fn page_range(page: usize, count: usize, before: usize, after: usize) -> Range<usize> {
    if count == 0 {
        return 0..0;
    }
    let page = page.min(count - 1);
    page.saturating_sub(before)..page.saturating_add(after).saturating_add(1).min(count)
}

/// Load every visible page plus neighbors without an unbounded raster cache.
pub fn visible_pdf_pages(page: usize, count: usize, viewport_height: f32, row_height: f32) -> Range<usize> {
    let visible = if viewport_height.is_finite() && row_height.is_finite() && row_height > 0.0 {
        (viewport_height.max(0.0) / row_height).ceil() as usize
    } else {
        1
    };
    let span = visible.saturating_add(1).clamp(3, 20);
    let base = desired_pdf_pages(page, count);
    base.start..base.end.max(page.saturating_add(span).min(count))
}

/// Return a zero-based page for the one-based value the person enters.
pub fn parse_page_number(value: &str, count: usize) -> Result<usize, String> {
    let page = value.trim().parse::<usize>().ok();
    match page {
        Some(page) if page > 0 && page <= count => Ok(page - 1),
        _ if count == 0 => Err("Open a PDF before choosing a page.".into()),
        _ => Err(format!("Enter a page number from 1 to {count}.")),
    }
}

/// Move through a finite search result list, wrapping at either end. A fresh
/// search starts at the first match (or last when moving backwards).
pub fn next_search_match(current: Option<usize>, count: usize, backwards: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    match current.filter(|index| *index < count) {
        None => Some(if backwards { count - 1 } else { 0 }),
        Some(0) if backwards => Some(count - 1),
        Some(index) if backwards => Some(index - 1),
        Some(index) if index == count - 1 => Some(0),
        Some(index) => Some(index + 1),
    }
}

/// Accept a native pathname or a local `file:` URL from a desktop launcher.
/// Percent-decoding is limited to URLs: a literal `%20` in a normal filename
/// stays literal. Native non-UTF-8 names and percent-encoded Unix bytes survive.
/// Remote hosts, other URL schemes, URL queries/fragments, and NULs are refused.
pub fn parse_open_path(value: &OsStr) -> Result<PathBuf, String> {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        return Err("Enter an image or PDF path.".into());
    }
    if bytes.contains(&0) {
        return Err("A file path cannot contain a NUL byte.".into());
    }
    let Some(colon) = bytes.iter().position(|byte| *byte == b':') else {
        return Ok(PathBuf::from(value));
    };
    let scheme = &bytes[..colon];
    let is_scheme = scheme.first().is_some_and(u8::is_ascii_alphabetic)
        && scheme
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(byte));
    if !is_scheme {
        return Ok(PathBuf::from(value));
    }
    if !scheme.eq_ignore_ascii_case(b"file") {
        // A colon can occur in a normal Unix filename. Interpret it as a URL
        // only when its syntax is unmistakable or it is a common remote URL.
        if bytes[colon + 1..].starts_with(b"//")
            || scheme.eq_ignore_ascii_case(b"http")
            || scheme.eq_ignore_ascii_case(b"https")
        {
            return Err("Viewer opens local files. Save this document first.".into());
        }
        return Ok(PathBuf::from(value));
    }
    let mut path = &bytes[colon + 1..];
    if let Some(rest) = path.strip_prefix(b"//") {
        let slash = rest
            .iter()
            .position(|byte| *byte == b'/')
            .ok_or_else(|| "A file URL needs an absolute path.".to_string())?;
        let host = &rest[..slash];
        if !host.is_empty() && !host.eq_ignore_ascii_case(b"localhost") {
            return Err("Viewer opens file URLs on this machine only.".into());
        }
        path = &rest[slash..];
    }
    if !path.starts_with(b"/") {
        return Err("A file URL needs an absolute path.".into());
    }
    if path.contains(&b'?') || path.contains(&b'#') {
        return Err("A file URL must not contain a query or fragment.".into());
    }
    let mut decoded = Vec::with_capacity(path.len());
    let mut index = 0;
    while index < path.len() {
        let byte = if path[index] == b'%' {
            let pair = path
                .get(index + 1..index + 3)
                .ok_or_else(|| "The file URL contains an incomplete escape.".to_string())?;
            let high = hex_digit(pair[0]);
            let low = hex_digit(pair[1]);
            let (Some(high), Some(low)) = (high, low) else {
                return Err("The file URL contains an invalid escape.".into());
            };
            index += 3;
            high * 16 + low
        } else {
            let byte = path[index];
            index += 1;
            byte
        };
        if byte == 0 {
            return Err("A file path cannot contain a NUL byte.".into());
        }
        decoded.push(byte);
    }
    Ok(PathBuf::from(OsString::from_vec(decoded)))
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_requests_follow_the_rail_without_loading_document_pages() {
        let mut cache = PageCache::new();
        let (wanted, retained) = thumbnail_windows(30, 2000, 720.0);
        cache.set_window(wanted, retained);
        let first = cache.request(2).unwrap();
        assert_eq!(first.pages, vec![29, 30]);
        assert!(cache.request(2).is_none(), "only one worker may render thumbnails");
        assert!(cache.finish(&first));
        for page in first.pages {
            cache.insert(page, page);
        }
        assert_eq!(cache.request(2).unwrap().pages, vec![31, 32]);
    }

    #[test]
    fn switching_documents_rejects_late_thumbnails_without_releasing_a_new_worker() {
        let mut cache = PageCache::new();
        cache.set_window(0..3, 0..4);
        let old = cache.request(2).unwrap();
        cache.reset();
        cache.set_window(40..43, 39..44);
        assert!(cache.request(2).is_none(), "old worker still owns the concurrency slot");
        assert!(
            !cache.finish(&old),
            "pixels from the previous document must be rejected"
        );
        let new = cache.request(2).unwrap();
        assert_eq!(new.pages, vec![40, 41]);
        assert!(
            !cache.finish(&old),
            "duplicate completion must not release the new worker"
        );
        assert!(cache.request(2).is_none());
        assert!(cache.finish(&new));
        cache.insert(40, "new document");
        assert_eq!(cache.entries().get(&40), Some(&"new document"));
    }

    #[test]
    fn scrolling_while_a_batch_runs_discards_unwanted_pixels_and_bounds_memory() {
        let mut cache = PageCache::new();
        cache.set_window(0..5, 0..6);
        let old = cache.request(2).unwrap();
        let (wanted, retained) = thumbnail_windows(1500, 2000, 10000.0);
        cache.set_window(wanted, retained);
        assert!(cache.finish(&old));
        for page in old.pages {
            assert_eq!(cache.insert(page, page), Some(page));
        }
        assert!(cache.entries().is_empty());
        for top in [1500, 1499, 30, 0, 1999] {
            let (wanted, retained) = thumbnail_windows(top, 2000, 10000.0);
            assert!(retained.len() <= 14);
            cache.set_window(wanted, retained);
            while let Some(request) = cache.request(2) {
                assert!(cache.finish(&request));
                for page in request.pages {
                    cache.insert(page, page);
                }
            }
            assert!(cache.entries().len() <= 14);
            assert!(cache.entries().contains_key(&top));
        }
    }

    #[test]
    fn failed_thumbnails_do_not_retry_forever_or_block_other_pages() {
        let mut cache = PageCache::<usize>::new();
        cache.set_window(0..3, 0..4);
        let request = cache.request(2).unwrap();
        assert!(cache.finish(&request));
        cache.fail(0);
        cache.insert(1, 1);
        assert_eq!(cache.request(2).unwrap().pages, vec![2]);
        assert!(cache.failed(0));
        cache.reset();
        assert!(!cache.failed(0));
    }

    #[test]
    fn page_anchor_survives_zoom_and_sidebar_layouts() {
        let page = 17;
        for zoom in [0.1, 0.5, 1.0, 3.0] {
            for thumbnails in [false, true] {
                for text in [false, true] {
                    let (_, height) = pdf_layout(1000.0, zoom, thumbnails, text);
                    // GPUI's strict item scroll resolves this logical page in
                    // the new layout, rather than retaining the old pixels.
                    let anchored_offset = -(page as f32 * height);
                    assert_eq!(page_from_scroll(anchored_offset, height, 100), page);
                }
            }
        }
        assert_eq!(page_from_scroll(-100000.0, 160.0, 12), 11);
        assert_eq!(page_from_scroll(f32::NAN, 160.0, 12), 0);
        assert_eq!(page_from_scroll(-100.0, 0.0, 12), 0);
    }

    #[test]
    fn zoomed_out_pages_are_loaded_for_the_whole_viewport() {
        assert_eq!(visible_pdf_pages(10, 100, 900.0, 100.0), 9..20);
        assert_eq!(visible_pdf_pages(0, 100, 10000.0, 1.0), 0..20);
        assert_eq!(visible_pdf_pages(0, 2, 900.0, 100.0), 0..2);
        assert_eq!(visible_pdf_pages(0, 100, f32::NAN, 0.0), 0..3);
    }
    #[test]
    fn pdf_ranges_stay_in_document_and_bound_live_rasters() {
        assert_eq!(desired_pdf_pages(0, 100), 0..3);
        assert_eq!(desired_pdf_pages(50, 100), 49..53);
        assert_eq!(retained_pdf_pages(50, 100), 48..54);
        assert_eq!(desired_pdf_pages(99, 100), 98..100);
        assert_eq!(desired_pdf_pages(usize::MAX, 2), 0..2);
        assert_eq!(desired_pdf_pages(usize::MAX, 0), 0..0);
        for count in [0, 1, 2, 100, usize::MAX] {
            for page in [0, 1, 50, 99, usize::MAX] {
                let desired = desired_pdf_pages(page, count);
                let retained = retained_pdf_pages(page, count);
                assert!(desired.len() <= 4);
                assert!(retained.len() <= 6);
                assert!(desired.end <= count);
                assert!(retained.end <= count);
                assert!(desired.start >= retained.start && desired.end <= retained.end);
                if count != 0 {
                    assert!(desired.contains(&page.min(count - 1)));
                }
            }
        }
    }

    #[test]
    fn page_jump_rejects_zero_out_of_range_and_overflow() {
        assert_eq!(parse_page_number(" 2 ", 3).unwrap(), 1);
        for value in ["0", "4", "-1", "1.5", "page 2", "99999999999999999999999999999"] {
            assert!(parse_page_number(value, 3).is_err());
        }
        assert!(parse_page_number("1", 0).is_err());
    }

    #[test]
    fn search_navigation_wraps_and_resets_after_results_change() {
        assert_eq!(next_search_match(None, 3, false), Some(0));
        assert_eq!(next_search_match(None, 3, true), Some(2));
        assert_eq!(next_search_match(Some(2), 3, false), Some(0));
        assert_eq!(next_search_match(Some(0), 3, true), Some(2));
        assert_eq!(next_search_match(Some(1), 3, false), Some(2));
        assert_eq!(next_search_match(Some(1), 3, true), Some(0));
        assert_eq!(next_search_match(Some(15), 2, false), Some(0));
        assert_eq!(next_search_match(Some(0), 1, false), Some(0));
        assert_eq!(next_search_match(None, 0, false), None);
        assert_eq!(next_search_match(Some(usize::MAX - 1), usize::MAX, false), Some(0));
    }

    #[test]
    fn local_file_urls_decode_spaces_unicode_and_native_bytes() {
        assert_eq!(
            parse_open_path(OsStr::new("file:///tmp/a%20b.png")).unwrap(),
            PathBuf::from("/tmp/a b.png")
        );
        assert_eq!(
            parse_open_path(OsStr::new("FILE://localhost/tmp/%C3%A9.pdf")).unwrap(),
            PathBuf::from("/tmp/é.pdf")
        );
        assert_eq!(
            parse_open_path(OsStr::new("file:/tmp/a%23b%3Fc.png")).unwrap(),
            PathBuf::from("/tmp/a#b?c.png")
        );
        assert_eq!(
            parse_open_path(OsStr::new("file:///tmp/a+b.png")).unwrap(),
            PathBuf::from("/tmp/a+b.png")
        );
        assert_eq!(
            parse_open_path(OsStr::new("file:///tmp/%FF.png"))
                .unwrap()
                .as_os_str()
                .as_bytes(),
            b"/tmp/\xff.png"
        );
    }

    #[test]
    fn native_paths_are_not_url_decoded() {
        for value in ["./a%20b.png", "/tmp/a#b?.png", "notes:one.pdf"] {
            assert_eq!(parse_open_path(OsStr::new(value)).unwrap(), PathBuf::from(value));
        }
        let bytes = OsStr::from_bytes(b"/tmp/\xff.png");
        assert_eq!(parse_open_path(bytes).unwrap().as_os_str(), bytes);
    }

    #[test]
    fn file_urls_reject_remote_hosts_malformed_escapes_and_nuls() {
        for value in [
            "",
            "https://example.org/a.pdf",
            "http:a.pdf",
            "sftp://example.org/a.pdf",
            "file://example.org/a.pdf",
            "file://localhost",
            "file:relative.pdf",
            "file:///tmp/a%",
            "file:///tmp/a%2",
            "file:///tmp/a%gg",
            "file:///tmp/%00.pdf",
            "file:///tmp/a.pdf#page=3",
            "file:///tmp/a.pdf?x=1",
            "/tmp/a\0.png",
        ] {
            assert!(parse_open_path(OsStr::new(value)).is_err(), "accepted {value:?}");
        }
    }
}
