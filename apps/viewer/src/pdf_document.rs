// SPDX-License-Identifier: MIT
//! PDF operations run on a worker, using the installed Poppler tools. Each
//! document owns an immutable private snapshot; each invocation owns and reaps
//! its child, with limits on memory, CPU time, wall time, and output bytes.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::NamedTempFile;

const MAX_PDF_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PAGES: usize = 2_000;
const MAX_TEXT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_EDGE: u32 = 4096;
const MAX_SEARCH_HITS: usize = 1000;
const TOOL_TIMEOUT: Duration = Duration::from_secs(12);

#[derive(Debug)]
pub struct PdfDocument {
    snapshot: NamedTempFile,
    pages: usize,
}

#[derive(Debug)]
pub struct RenderedPage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    /// Zero-based page index.
    pub page: usize,
    /// UTF-8 byte offset in this page's extracted text.
    pub offset: usize,
    pub excerpt: String,
}

impl PdfDocument {
    pub fn open(path: &Path) -> Result<Self, String> {
        // Check before open for ordinary special files, and again on the open
        // descriptor to handle replacements between the two operations.
        let metadata = fs::metadata(path).map_err(io_error)?;
        if !metadata.is_file() || metadata.len() > MAX_PDF_BYTES {
            return Err("Choose a regular PDF file up to 64 MiB.".into());
        }
        let input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(io_error)?;
        let metadata = input.metadata().map_err(io_error)?;
        if !metadata.is_file() || metadata.len() > MAX_PDF_BYTES {
            return Err("Choose a regular PDF file up to 64 MiB.".into());
        }
        let mut snapshot = NamedTempFile::new().map_err(io_error)?;
        let copied = std::io::copy(&mut input.take(MAX_PDF_BYTES + 1), &mut snapshot).map_err(io_error)?;
        if copied > MAX_PDF_BYTES {
            return Err("This PDF grew beyond the 64 MiB limit while opening.".into());
        }
        snapshot.seek(SeekFrom::Start(0)).map_err(io_error)?;
        let mut header = [0; 1024];
        let length = snapshot.read(&mut header).map_err(io_error)?;
        if !header[..length].windows(5).any(|bytes| bytes == b"%PDF-") {
            return Err("This file does not contain a PDF header.".into());
        }
        let info = run_poppler("pdfinfo", &[snapshot.path().as_os_str().into()], 1024 * 1024)?;
        let pages = parse_page_count(&String::from_utf8_lossy(&info))?;
        Ok(Self { snapshot, pages })
    }

    pub fn page_count(&self) -> usize {
        self.pages
    }

    /// Media-box size in PDF points, with the page's rotation applied.
    pub fn page_size(&self, index: usize) -> Result<(f32, f32), String> {
        let page = page_number(index, self.pages)?;
        let output = run_poppler(
            "pdfinfo",
            &[
                "-f".into(),
                page.clone().into(),
                "-l".into(),
                page.into(),
                self.snapshot.path().as_os_str().into(),
            ],
            1024 * 1024,
        )?;
        parse_page_size(&String::from_utf8_lossy(&output))
    }

    /// Render only one page. `width` is the desired pixel width; very tall or
    /// wide pages are reduced to fit a 4096-by-4096 pixel box. No raster cache
    /// is retained here, so the UI can bound the pages it keeps alive.
    pub fn render_page(&self, index: usize, width: u32) -> Result<RenderedPage, String> {
        let page = page_number(index, self.pages)?;
        let (page_width, page_height) = self.page_size(index)?;
        let desired_edge = f64::from(width.clamp(1, MAX_EDGE)) * f64::from(page_width.max(page_height) / page_width);
        let edge = desired_edge.ceil().clamp(1.0, f64::from(MAX_EDGE)) as u32;
        let bytes = run_poppler(
            "pdftoppm",
            &[
                "-f".into(),
                page.clone().into(),
                "-l".into(),
                page.into(),
                "-singlefile".into(),
                "-png".into(),
                "-scale-to".into(),
                edge.to_string().into(),
                self.snapshot.path().as_os_str().into(),
            ],
            MAX_PIXELS * 4 + 1024 * 1024,
        )?;
        let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&bytes), image::ImageFormat::Png);
        let dimensions = image::ImageReader::with_format(std::io::Cursor::new(&bytes), image::ImageFormat::Png)
            .into_dimensions()
            .map_err(|error| format!("Cannot read this PDF page: {error}"))?;
        if dimensions.0 == 0
            || dimensions.1 == 0
            || dimensions.0 > MAX_EDGE
            || dimensions.1 > MAX_EDGE
            || u64::from(dimensions.0) * u64::from(dimensions.1) > MAX_PIXELS
        {
            return Err("This PDF page exceeds the rendering limit.".into());
        }
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_EDGE);
        limits.max_image_height = Some(MAX_EDGE);
        limits.max_alloc = Some(MAX_PIXELS * 8);
        reader.limits(limits);
        let rgba = reader
            .decode()
            .map_err(|error| format!("Cannot render this PDF page: {error}"))?
            .into_rgba8();
        Ok(RenderedPage {
            width: rgba.width(),
            height: rgba.height(),
            rgba: rgba.into_raw(),
        })
    }

    pub fn page_text(&self, index: usize) -> Result<String, String> {
        let page = page_number(index, self.pages)?;
        let bytes = run_poppler(
            "pdftotext",
            &[
                "-f".into(),
                page.clone().into(),
                "-l".into(),
                page.into(),
                "-enc".into(),
                "UTF-8".into(),
                "-layout".into(),
                "-nopgbrk".into(),
                self.snapshot.path().as_os_str().into(),
                "-".into(),
            ],
            MAX_TEXT_BYTES,
        )?;
        String::from_utf8(bytes).map_err(|_| "The PDF text is not valid UTF-8.".into())
    }

    /// Case-insensitive search, returning at most 1000 matches in page order.
    /// Text extraction is limited to 16 MiB and one bounded Poppler invocation.
    pub fn search(&self, query: &str) -> Result<Vec<SearchHit>, String> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        if query.len() > 4096 {
            return Err("Use a search phrase shorter than 4096 bytes.".into());
        }
        let bytes = run_poppler(
            "pdftotext",
            &[
                "-enc".into(),
                "UTF-8".into(),
                "-layout".into(),
                self.snapshot.path().as_os_str().into(),
                "-".into(),
            ],
            MAX_TEXT_BYTES,
        )?;
        let text = String::from_utf8(bytes).map_err(|_| "The PDF text is not valid UTF-8.".to_string())?;
        Ok(search_pages(&text, query, self.pages))
    }
}

fn io_error(error: std::io::Error) -> String {
    format!("Cannot read this PDF: {error}")
}

fn page_number(index: usize, count: usize) -> Result<String, String> {
    if index >= count {
        return Err(format!("Choose a page from 1 to {count}."));
    }
    Ok((index + 1).to_string())
}

fn parse_page_count(info: &str) -> Result<usize, String> {
    let count = info
        .lines()
        .find_map(|line| line.strip_prefix("Pages:")?.trim().parse::<usize>().ok());
    match count {
        Some(count) if (1..=MAX_PAGES).contains(&count) => Ok(count),
        _ => Err(format!("Choose a PDF with 1 to {MAX_PAGES} pages.")),
    }
}

fn parse_page_size(info: &str) -> Result<(f32, f32), String> {
    let dimensions = info.lines().find_map(|line| {
        if !line.starts_with("Page") {
            return None;
        }
        let (_, size) = line.split_once("size:")?;
        let mut values = size.split_whitespace();
        let width = values.next()?.parse::<f32>().ok()?;
        if values.next()? != "x" {
            return None;
        }
        let height = values.next()?.parse::<f32>().ok()?;
        Some((width, height))
    });
    let Some((width, height)) = dimensions else {
        return Err("This PDF has no readable page dimensions.".into());
    };
    if !width.is_finite()
        || !height.is_finite()
        || width <= 0.0
        || height <= 0.0
        || width > 1_000_000.0
        || height > 1_000_000.0
    {
        return Err("This PDF has invalid page dimensions.".into());
    }
    let rotation = info
        .lines()
        .find_map(|line| {
            if !line.starts_with("Page") {
                return None;
            }
            line.split_once("rot:")?.1.trim().parse::<i32>().ok()
        })
        .unwrap_or(0);
    if rotation.rem_euclid(180) == 90 {
        Ok((height, width))
    } else {
        Ok((width, height))
    }
}

fn search_pages(text: &str, query: &str, page_count: usize) -> Vec<SearchHit> {
    let query = query.to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for (page, text) in text.split('\u{c}').take(page_count).enumerate() {
        // Lowercasing can change UTF-8 lengths (İ, for example). Preserve the
        // original byte position for each lowered byte instead of slicing the
        // source with an offset into the lowered string.
        let mut folded = String::new();
        let mut original_offsets = Vec::new();
        for (offset, character) in text.char_indices() {
            for lower in character.to_lowercase() {
                folded.push(lower);
                original_offsets.extend(std::iter::repeat_n(offset, lower.len_utf8()));
            }
        }
        for (start, _) in folded.match_indices(&query) {
            let offset = original_offsets[start];
            let begin = text[..offset].char_indices().rev().nth(32).map_or(0, |(i, _)| i);
            let end = text[offset..]
                .char_indices()
                .nth(96)
                .map_or(text.len(), |(i, _)| offset + i);
            let excerpt = text[begin..end].split_whitespace().collect::<Vec<_>>().join(" ");
            hits.push(SearchHit { page, offset, excerpt });
            if hits.len() == MAX_SEARCH_HITS {
                return hits;
            }
        }
    }
    hits
}

fn run_poppler(program: &str, args: &[OsString], output_limit: u64) -> Result<Vec<u8>, String> {
    let mut output = NamedTempFile::new().map_err(io_error)?;
    let stdout = output.reopen().map_err(io_error)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::null());
    // SAFETY: The child hook uses only setrlimit and errno reads, with no
    // allocations, locking, or inherited runtime access after fork.
    unsafe {
        command.pre_exec(move || {
            for (resource, value) in [
                (libc::RLIMIT_AS, 1024 * 1024 * 1024),
                (libc::RLIMIT_CPU, 10),
                (libc::RLIMIT_FSIZE, output_limit),
            ] {
                let limit = libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                };
                if libc::setrlimit(resource, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Cannot start {program}; Viewer needs the Poppler tools: {error}"))?;
    let deadline = Instant::now() + TOOL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => {
                return Err(
                    "Cannot read this PDF. It may be damaged, password protected, or exceed a rendering limit.".into(),
                );
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            outcome => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match outcome {
                    Err(error) => format!("Cannot finish reading this PDF: {error}"),
                    _ => "Reading this PDF took too long. Try a simpler document.".into(),
                });
            }
        }
    }
    if output.as_file().metadata().map_err(io_error)?.len() >= output_limit {
        return Err("This PDF exceeds the output limit.".into());
    }
    output.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let mut bytes = Vec::new();
    output
        .take(output_limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > output_limit {
        return Err("This PDF exceeds the output limit.".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn validates_page_count_and_page_navigation() {
        assert_eq!(parse_page_count("Title: Test\nPages: 12\n").unwrap(), 12);
        for info in ["", "Pages: 0", "Pages: -1", "Pages: 2001", "Pages: banana"] {
            assert!(parse_page_count(info).is_err());
        }
        assert_eq!(page_number(0, 3).unwrap(), "1");
        assert_eq!(page_number(2, 3).unwrap(), "3");
        assert!(page_number(3, 3).is_err());
        assert!(page_number(usize::MAX, 3).is_err());
    }

    #[test]
    fn parses_size_and_rotation_and_refuses_invalid_dimensions() {
        assert_eq!(
            parse_page_size("Page    2 size: 612 x 792 pts\nPage    2 rot: 90").unwrap(),
            (792.0, 612.0)
        );
        assert_eq!(
            parse_page_size("Page size: 612 x 792 pts (letter)").unwrap(),
            (612.0, 792.0)
        );
        for info in [
            "Page size: 0 x 20",
            "Page size: NaN x 2",
            "Page size: 10 x inf",
            "Page size: 999999999 x 2",
            "Title: size: 2 x 3",
        ] {
            assert!(parse_page_size(info).is_err());
        }
    }

    #[test]
    fn search_tracks_pages_unicode_offsets_and_caps_results() {
        let hits = search_pages("Été: HELLO\u{c}İstanbul says hello\u{c}", "hello", 2);
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].page, hits[0].offset), (0, "Été: ".len()));
        assert_eq!((hits[1].page, hits[1].offset), (1, "İstanbul says ".len()));
        assert!(hits[1].excerpt.contains("İstanbul"));
        assert_eq!(search_pages(&"a ".repeat(2_000), "a", 1).len(), MAX_SEARCH_HITS);
        assert!(search_pages("text", "", 1).is_empty());
        assert_eq!(search_pages("one\u{c}two", "two", 1), Vec::new());
    }

    #[test]
    fn rejects_nonregular_oversized_and_non_pdf_inputs() {
        let dir = tempfile::tempdir().unwrap();
        assert!(PdfDocument::open(dir.path()).is_err());
        let fifo = dir.path().join("fifo.pdf");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: name is a valid, terminated local pathname; no borrowed
        // memory is retained by mkfifo.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(PdfDocument::open(&fifo).is_err());
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(b"This is not PDF").unwrap();
        assert!(PdfDocument::open(file.path()).is_err());
        file.as_file().set_len(MAX_PDF_BYTES + 1).unwrap();
        assert!(PdfDocument::open(file.path()).is_err());
    }

    fn fixture() -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        let stream = "BT /F1 20 Tf 40 240 Td (Hello Viewer) Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".into(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
        ];
        let mut bytes = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            write!(bytes, "{} 0 obj\n{object}\nendobj\n", index + 1).unwrap();
        }
        let xref = bytes.len();
        write!(bytes, "xref\n0 6\n0000000000 65535 f \n").unwrap();
        for offset in offsets {
            writeln!(bytes, "{offset:010} 00000 n ").unwrap();
        }
        write!(bytes, "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").unwrap();
        file.write_all(&bytes).unwrap();
        file
    }

    #[test]
    fn poppler_opens_renders_extracts_and_searches_an_immutable_snapshot() {
        let file = fixture();
        let document = PdfDocument::open(file.path()).unwrap();
        assert_eq!(document.page_count(), 1);
        assert_eq!(document.page_size(0).unwrap(), (300.0, 300.0));
        // Replacing the source cannot change the document the person is viewing.
        fs::write(file.path(), b"replaced source").unwrap();
        let rendered = document.render_page(0, 240).unwrap();
        assert_eq!((rendered.width, rendered.height), (240, 240));
        assert_eq!(rendered.rgba.len(), 240 * 240 * 4);
        assert!(rendered.rgba.as_chunks::<4>().0.iter().any(|pixel| pixel[0] < 200));
        assert!(document.page_text(0).unwrap().contains("Hello Viewer"));
        assert_eq!(document.search("viewer").unwrap().len(), 1);
        assert!(document.render_page(1, 240).is_err());
        assert!(document.page_text(usize::MAX).is_err());
    }

    #[test]
    fn subprocess_output_is_bounded() {
        let result = run_poppler("printf", &["0123456789".into()], 5);
        assert!(result.is_err());
    }
}
