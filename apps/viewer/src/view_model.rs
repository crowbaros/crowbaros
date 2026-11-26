// SPDX-License-Identifier: MIT
//! Small state transitions shared by Viewer input and asynchronous page work.

use std::ffi::{OsStr, OsString};
use std::ops::Range;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

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
