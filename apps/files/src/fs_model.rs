// SPDX-License-Identifier: MIT
//! Filesystem browsing, metadata formatting, search, and safe non-overwriting file operations.
//! Pure Rust with no GPUI dependency so every operation is unit-tested directly.
use chrono::{DateTime, Datelike, Local, Timelike};
use std::{
    cmp::Ordering,
    collections::VecDeque,
    env,
    ffi::{CString, OsStr, OsString},
    fs::{self, Metadata},
    io::{self, Write},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileCategory {
    Folder,
    Image,
    Text,
    Code,
    Audio,
    Video,
    Archive,
    Pdf,
    Binary,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortField {
    #[default]
    Name,
    Modified,
    Size,
    Kind,
}

impl SortField {
    pub const ALL: [Self; 4] = [Self::Name, Self::Modified, Self::Size, Self::Kind];

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Modified => "Modified",
            Self::Size => "Size",
            Self::Kind => "Type",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
}

impl SortOrder {
    pub fn toggle(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub symlink_target: Option<PathBuf>,
    pub size: u64,
    pub child_count: Option<usize>,
    pub modified: Option<SystemTime>,
    pub mode: u32,
    pub hidden: bool,
    pub category: FileCategory,
    pub kind_label: String,
    pub trash_original: Option<PathBuf>,
    /// An image's width times height, read from its header. Decoding it takes four bytes a pixel.
    pub pixels: Option<u64>,
}

/// The most pixels one image may have for Files to decode it for a preview: 40 MP, 160 MB decoded.
pub const PREVIEW_MAX_PIXELS: u64 = 40_000_000;
/// The most pixels a pane's Grid thumbnails may hold decoded together: 64 MP, 256 MB.
pub const THUMBNAIL_BUDGET_PIXELS: u64 = 64_000_000;

/// Which entries get a Grid thumbnail: still images, in order, while their decoded pixels fit the
/// budget. A GIF decodes every frame, so it never gets one.
pub fn thumbnail_choices(entries: &[Entry], budget: u64) -> Vec<bool> {
    let mut left = budget;
    entries
        .iter()
        .map(|entry| {
            let still = entry.is_previewable_image() && extension_lower(&entry.path).as_deref() != Some("gif");
            match entry.pixels {
                Some(pixels) if still && pixels <= left => {
                    left -= pixels;
                    true
                }
                _ => false,
            }
        })
        .collect()
}

impl Entry {
    pub fn icon_name(&self) -> &'static str {
        if self.is_dir {
            return special_folder_icon(&self.name).unwrap_or("folder");
        }
        match self.category {
            FileCategory::Folder => "folder",
            FileCategory::Image => "file-image",
            FileCategory::Text | FileCategory::Pdf => "file-text",
            FileCategory::Code => "file-code",
            FileCategory::Audio => "file-audio",
            FileCategory::Video => "file-video",
            FileCategory::Archive => "file-archive",
            FileCategory::Binary => "file",
        }
    }

    pub fn is_previewable_image(&self) -> bool {
        if self.is_dir || self.category != FileCategory::Image {
            return false;
        }
        matches!(
            extension_lower(&self.path).as_deref(),
            Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp")
        )
    }

    /// Whether the inspector or quick preview may decode this image: one whose size is known and
    /// not too large.
    pub fn can_preview_image(&self) -> bool {
        self.is_previewable_image() && self.pixels.is_some_and(|pixels| pixels <= PREVIEW_MAX_PIXELS)
    }

    pub fn is_previewable_text(&self) -> bool {
        !self.is_dir && matches!(self.category, FileCategory::Text | FileCategory::Code)
    }

    pub fn subtitle(&self) -> String {
        if self.is_dir {
            format!("Folder · {}", format_item_count(self.child_count))
        } else {
            format!("{} · {}", self.kind_label, format_size(self.size))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarEntry {
    pub label: String,
    pub icon: &'static str,
    pub path: PathBuf,
}

pub fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn trash_root() -> PathBuf {
    let data = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home_dir().join(".local/share"));
    data.join("Trash")
}

pub fn special_folder_icon(name: &str) -> Option<&'static str> {
    match name {
        "Documents" => Some("documents"),
        "Downloads" => Some("downloads"),
        "Music" => Some("music"),
        "Pictures" => Some("pictures"),
        "Videos" => Some("videos"),
        _ => None,
    }
}

fn extension_lower(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
}

pub fn classify(path: &Path, is_dir: bool) -> (FileCategory, String) {
    if is_dir {
        return (FileCategory::Folder, "Folder".into());
    }
    let ext = extension_lower(path);
    match ext.as_deref() {
        Some("png") => (FileCategory::Image, "PNG image".into()),
        Some("jpg" | "jpeg") => (FileCategory::Image, "JPEG image".into()),
        Some("webp") => (FileCategory::Image, "WebP image".into()),
        Some("gif") => (FileCategory::Image, "GIF image".into()),
        Some("bmp") => (FileCategory::Image, "BMP image".into()),
        Some("svg") => (FileCategory::Image, "SVG image".into()),
        Some("ico" | "avif" | "heic" | "jxl" | "tiff" | "tif") => (FileCategory::Image, "Image".into()),
        Some("pdf") => (FileCategory::Pdf, "PDF document".into()),
        Some("md" | "markdown") => (FileCategory::Text, "Markdown".into()),
        Some("txt") => (FileCategory::Text, "Plain text".into()),
        Some("log") => (FileCategory::Text, "Log file".into()),
        Some("patch" | "diff") => (FileCategory::Text, "Patch".into()),
        Some("csv" | "tsv") => (FileCategory::Text, "Table".into()),
        Some("ini" | "cfg" | "conf" | "env" | "desktop" | "list") => (FileCategory::Text, "Configuration".into()),
        Some("rs") => (FileCategory::Code, "Rust source".into()),
        Some("py") => (FileCategory::Code, "Python script".into()),
        Some("js" | "mjs" | "cjs") => (FileCategory::Code, "JavaScript".into()),
        Some("ts" | "tsx") => (FileCategory::Code, "TypeScript".into()),
        Some("jsx") => (FileCategory::Code, "JSX".into()),
        Some("c" | "h") => (FileCategory::Code, "C source".into()),
        Some("cc" | "cpp" | "cxx" | "hpp") => (FileCategory::Code, "C++ source".into()),
        Some("go") => (FileCategory::Code, "Go source".into()),
        Some("sh" | "bash" | "zsh" | "fish") => (FileCategory::Code, "Shell script".into()),
        Some("json") => (FileCategory::Code, "JSON".into()),
        Some("toml") => (FileCategory::Code, "TOML".into()),
        Some("yaml" | "yml") => (FileCategory::Code, "YAML".into()),
        Some("xml" | "html" | "htm") => (FileCategory::Code, "Markup".into()),
        Some("css" | "scss") => (FileCategory::Code, "Stylesheet".into()),
        Some("sql" | "lua" | "rb" | "java" | "kt" | "swift" | "zig" | "nix") => {
            (FileCategory::Code, "Source code".into())
        }
        Some("mp3" | "flac" | "wav" | "ogg" | "opus" | "m4a" | "aac") => (FileCategory::Audio, "Audio".into()),
        Some("mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v") => (FileCategory::Video, "Video".into()),
        Some("zip" | "tar" | "gz" | "xz" | "zst" | "bz2" | "7z" | "rar" | "tgz") => {
            (FileCategory::Archive, "Archive".into())
        }
        _ => {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if matches!(
                name,
                "Makefile"
                    | "Dockerfile"
                    | "LICENSE"
                    | "README"
                    | "Cargo.lock"
                    | ".gitignore"
                    | ".bashrc"
                    | ".zshrc"
                    | ".profile"
            ) {
                (FileCategory::Text, "Plain text".into())
            } else {
                (FileCategory::Binary, "File".into())
            }
        }
    }
}

pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    const TB: f64 = 1_000_000_000_000.0;

    let b = bytes as f64;
    if bytes < 1_000 {
        format!("{bytes} B")
    } else if b < MB {
        format!("{:.1} KB", b / KB)
    } else if b < GB {
        format!("{:.1} MB", b / MB)
    } else if b < TB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.1} TB", b / TB)
    }
}

pub fn format_item_count(count: Option<usize>) -> String {
    match count {
        None => "—".into(),
        Some(0) => "No items".into(),
        Some(1) => "1 item".into(),
        Some(n) => format!("{n} items"),
    }
}

pub fn format_short_date(time: Option<SystemTime>) -> String {
    let Some(time) = time else {
        return "—".into();
    };
    let dt: DateTime<Local> = time.into();
    format!("{}/{}/{:02}", dt.month(), dt.day(), dt.year().rem_euclid(100))
}

pub fn format_inspector_date(time: Option<SystemTime>) -> String {
    let Some(time) = time else {
        return "—".into();
    };
    let dt: DateTime<Local> = time.into();
    let (is_pm, hour12) = dt.hour12();
    let suffix = if is_pm { "PM" } else { "AM" };
    format!(
        "{}/{}/{:02} {}:{:02}{}",
        dt.month(),
        dt.day(),
        dt.year().rem_euclid(100),
        hour12,
        dt.minute(),
        suffix
    )
}

pub fn format_permissions(mode: u32) -> String {
    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];
    bits.iter()
        .map(|(mask, ch)| if mode & mask != 0 { *ch } else { '-' })
        .collect()
}

pub fn breadcrumbs(path: &Path, home: &Path) -> Vec<(String, PathBuf)> {
    if let Ok(rel) = path.strip_prefix(home) {
        let mut out = vec![("~".into(), home.to_path_buf())];
        let mut acc = home.to_path_buf();
        for comp in rel.components() {
            if let Component::Normal(seg) = comp {
                acc.push(seg);
                out.push((seg.to_string_lossy().into_owned(), acc.clone()));
            }
        }
        return out;
    }

    let mut out = vec![("/".into(), PathBuf::from("/"))];
    let mut acc = PathBuf::from("/");
    for comp in path.components() {
        if let Component::Normal(seg) = comp {
            acc.push(seg);
            out.push((seg.to_string_lossy().into_owned(), acc.clone()));
        }
    }
    out
}

pub fn display_path(path: &Path, home: &Path) -> String {
    if path == home {
        "~".into()
    } else if let Ok(rel) = path.strip_prefix(home) {
        format!("~/{}", rel.display())
    } else {
        path.display().to_string()
    }
}

fn decode_percent_bytes(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(val) = u8::from_str_radix(hex, 16)
        {
            out.push(val);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Decodes a percent-encoded path byte for byte, so names that are not UTF-8 survive.
fn decode_percent_path(input: &str) -> PathBuf {
    PathBuf::from(OsString::from_vec(decode_percent_bytes(input)))
}

pub fn encode_percent_path(path: &Path) -> String {
    let mut out = String::new();
    for b in path.as_os_str().as_encoded_bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~');
        if keep {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn expand_path(raw: &str, home: &Path, current: &Path) -> PathBuf {
    let trimmed = raw.trim();
    if let Some(uri) = trimmed.strip_prefix("file://") {
        // file:///path, file://localhost/path, or file://host/path: the path starts at the first slash.
        let path = uri.find('/').map_or("/", |at| &uri[at..]);
        return normalize_path(&decode_percent_path(path));
    }
    if trimmed.is_empty() || trimmed == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return normalize_path(&home.join(rest));
    }
    let p = Path::new(trimmed);
    if p.is_absolute() {
        normalize_path(p)
    } else {
        normalize_path(&current.join(p))
    }
}

pub fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(p) => out.push(p.as_os_str()),
            Component::RootDir => out.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                // `link/..` is the parent of the link's target, not the folder holding the link.
                if fs::symlink_metadata(&out).is_ok_and(|m| m.file_type().is_symlink())
                    && let Ok(real) = fs::canonicalize(&out)
                {
                    out = real;
                }
                out.pop();
            }
            Component::Normal(seg) => out.push(seg),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        out
    }
}

fn shallow_child_count(dir: &Path, show_hidden: bool) -> Option<usize> {
    let rd = fs::read_dir(dir).ok()?;
    let mut count = 0usize;
    for item in rd.flatten() {
        let hidden = item.file_name().to_str().is_some_and(|s| s.starts_with('.'));
        if show_hidden || !hidden {
            count += 1;
        }
    }
    Some(count)
}

fn build_entry(path: PathBuf, show_hidden_for_count: bool) -> Option<Entry> {
    let name = path.file_name()?.to_string_lossy().into_owned();
    let sym_meta = fs::symlink_metadata(&path).ok()?;
    let is_symlink = sym_meta.file_type().is_symlink();
    let symlink_target = if is_symlink { fs::read_link(&path).ok() } else { None };
    let target_meta: Metadata = if is_symlink {
        fs::metadata(&path).unwrap_or(sym_meta)
    } else {
        sym_meta
    };
    let is_dir = target_meta.is_dir();
    let is_special = !is_dir && !target_meta.is_file();
    let hidden = name.starts_with('.');
    let child_count = if is_dir {
        shallow_child_count(&path, show_hidden_for_count)
    } else {
        None
    };
    // Pipes, sockets, and devices are never previewed or opened for reading.
    let (category, kind_label) = if is_special {
        (FileCategory::Binary, "Special file".into())
    } else {
        classify(&path, is_dir)
    };
    Some(
        Entry {
            name,
            path,
            is_dir,
            is_symlink,
            symlink_target,
            size: target_meta.len(),
            child_count,
            modified: target_meta.modified().ok(),
            mode: target_meta.permissions().mode(),
            hidden,
            category,
            kind_label,
            trash_original: None,
            pixels: None,
        }
        .with_pixels(),
    )
}

impl Entry {
    fn with_pixels(mut self) -> Self {
        if self.is_previewable_image() {
            self.pixels = image::image_dimensions(&self.path)
                .ok()
                .map(|(w, h)| u64::from(w) * u64::from(h));
        }
        self
    }
}

pub fn sort_entries(entries: &mut [Entry], field: SortField, order: SortOrder) {
    entries.sort_by(|a, b| {
        // Folders always come first.
        match (a.is_dir, b.is_dir) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            _ => {}
        }
        let cmp = match field {
            SortField::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortField::Modified => a.modified.cmp(&b.modified),
            SortField::Size => {
                if a.is_dir && b.is_dir {
                    a.child_count.cmp(&b.child_count)
                } else {
                    a.size.cmp(&b.size)
                }
            }
            SortField::Kind => a.kind_label.to_lowercase().cmp(&b.kind_label.to_lowercase()),
        };
        let ordered = match order {
            SortOrder::Asc => cmp,
            SortOrder::Desc => cmp.reverse(),
        };
        if ordered == Ordering::Equal {
            a.name.to_lowercase().cmp(&b.name.to_lowercase())
        } else {
            ordered
        }
    });
}

pub fn read_directory(
    dir: &Path,
    show_hidden: bool,
    sort_field: SortField,
    sort_order: SortOrder,
) -> Result<Vec<Entry>, String> {
    let rd = fs::read_dir(dir).map_err(|e| format!("Cannot open {}: {e}", dir.display()))?;
    let mut entries = Vec::new();
    for item in rd.flatten() {
        let p = item.path();
        if let Some(entry) = build_entry(p, show_hidden)
            && (show_hidden || !entry.hidden)
        {
            entries.push(entry);
        }
    }
    sort_entries(&mut entries, sort_field, sort_order);
    Ok(entries)
}

pub fn home_folders(home: &Path) -> Vec<SidebarEntry> {
    let Ok(rd) = fs::read_dir(home) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for item in rd.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = item.path();
        if fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
            let icon = special_folder_icon(&name).unwrap_or("folder");
            dirs.push(SidebarEntry {
                label: name,
                icon,
                path,
            });
        }
    }
    dirs.sort_by_key(|a| a.label.to_lowercase());
    dirs
}

pub fn drives() -> Vec<SidebarEntry> {
    let mut out = vec![SidebarEntry {
        label: "System".into(),
        icon: "drive",
        path: PathBuf::from("/"),
    }];
    let Ok(mounts) = fs::read_to_string("/proc/mounts") else {
        return out;
    };
    for line in mounts.lines() {
        let mut parts = line.split_whitespace();
        let Some(_dev) = parts.next() else { continue };
        let Some(target) = parts.next() else { continue };
        let decoded = target.replace("\\040", " ");
        if decoded.starts_with("/media/") || decoded.starts_with("/mnt/") || decoded.starts_with("/run/media/") {
            let p = PathBuf::from(&decoded);
            if p.is_dir() && !out.iter().any(|e| e.path == p) {
                let label = p.file_name().and_then(|n| n.to_str()).unwrap_or(&decoded).to_string();
                out.push(SidebarEntry {
                    label,
                    icon: "drive",
                    path: p,
                });
            }
        }
    }
    out
}

pub fn read_recents(home: &Path, max_items: usize) -> Vec<Entry> {
    let mut found = Vec::new();
    let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
    queue.push_back((home.to_path_buf(), 0));
    let mut visited_dirs = 0usize;

    while let Some((dir, depth)) = queue.pop_front() {
        if visited_dirs >= 80 {
            break;
        }
        visited_dirs += 1;
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for item in rd.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let path = item.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                if depth < 2 {
                    queue.push_back((path, depth + 1));
                }
            } else if let Some(entry) = build_entry(path, false) {
                found.push(entry);
            }
        }
    }
    found.sort_by_key(|a| std::cmp::Reverse(a.modified));
    found.truncate(max_items);
    found
}

fn trashinfo_name(name: &OsStr) -> OsString {
    let mut info = name.to_os_string();
    info.push(".trashinfo");
    info
}

/// Reads the original location from a `.trashinfo` file. A relative path is relative to the
/// folder that holds the Trash, as the FreeDesktop Trash specification says.
fn parse_trashinfo_path(info_path: &Path, trash: &Path) -> Option<PathBuf> {
    let text = fs::read(info_path).ok()?;
    for line in text.split(|b| *b == b'\n') {
        if let Some(rest) = line.strip_prefix(b"Path=") {
            let path = decode_percent_path(std::str::from_utf8(rest).ok()?.trim());
            if path.is_absolute() {
                return Some(path);
            }
            return Some(trash.parent().unwrap_or(Path::new("/")).join(path));
        }
    }
    None
}

pub fn read_trash() -> Vec<Entry> {
    let root = trash_root();
    let files_dir = root.join("files");
    let info_dir = root.join("info");
    let Ok(rd) = fs::read_dir(&files_dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for item in rd.flatten() {
        let path = item.path();
        let info_path = info_dir.join(trashinfo_name(&item.file_name()));
        if let Some(mut entry) = build_entry(path, true) {
            entry.trash_original = parse_trashinfo_path(&info_path, &root);
            entries.push(entry);
        }
    }
    entries.sort_by_key(|a| std::cmp::Reverse(a.modified));
    entries
}

pub fn search_below(root: &Path, query: &str, show_hidden: bool, max_results: usize) -> Vec<Entry> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let terms: Vec<&str> = q.split_whitespace().collect();
    let mut results = Vec::new();
    let mut queue = VecDeque::new();
    queue.push_back((root.to_path_buf(), 0usize));
    let mut visited_dirs = 0usize;

    while let Some((dir, depth)) = queue.pop_front() {
        if visited_dirs >= 1500 || results.len() >= max_results {
            break;
        }
        visited_dirs += 1;
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for item in rd.flatten() {
            if results.len() >= max_results {
                break;
            }
            let name = item.file_name().to_string_lossy().into_owned();
            if !show_hidden && name.starts_with('.') {
                continue;
            }
            let path = item.path();
            let lower = name.to_lowercase();
            if terms.iter().all(|t| lower.contains(t))
                && let Some(entry) = build_entry(path.clone(), show_hidden)
            {
                results.push(entry);
            }
            if depth < 6
                && let Ok(meta) = fs::symlink_metadata(&path)
                && meta.is_dir()
                && !meta.file_type().is_symlink()
            {
                queue.push_back((path, depth + 1));
            }
        }
    }
    sort_entries(&mut results, SortField::Name, SortOrder::Asc);
    results
}

pub fn read_text_preview(path: &Path, max_bytes: usize, max_lines: usize) -> Option<String> {
    // Opening without blocking, then checking the kind, keeps a pipe named like a text file
    // from freezing the window.
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut buf = Vec::new();
    use std::io::Read;
    file.take(max_bytes as u64).read_to_end(&mut buf).ok()?;
    // Reject binary files containing NUL bytes.
    if buf.contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().take(max_lines).collect();
    if text.lines().count() > max_lines {
        lines.push("…");
    }
    Some(lines.join("\n"))
}

pub fn rename_stem_end(name: &str, is_dir: bool) -> usize {
    if is_dir {
        return name.len();
    }
    let Some(dot) = name.rfind('.') else {
        return name.len();
    };
    if dot == 0 {
        // Hidden file without an extension (e.g. ".bashrc")
        name.len()
    } else {
        dot
    }
}

pub fn validate_entry_name(name: &str) -> Result<String, String> {
    // The name is kept as typed: spaces at either end are part of it.
    if name.trim().is_empty() {
        return Err("Name cannot be empty.".into());
    }
    if name == "." || name == ".." {
        return Err("Invalid name.".into());
    }
    if name.contains('/') || name.contains('\0') {
        return Err("Name cannot contain slashes.".into());
    }
    Ok(name.to_string())
}

fn file_name_of(path: &Path) -> Result<&OsStr, String> {
    path.file_name().ok_or_else(|| "This item has no name.".to_string())
}

fn is_cross_device(err: &io::Error) -> bool {
    err.raw_os_error() == Some(libc::EXDEV)
}

/// Renames `src` to `dst` and never replaces what is at `dst`, even if it appears after a check:
/// an existing target fails with `AlreadyExists`.
fn rename_noreplace(src: &Path, dst: &Path) -> io::Result<()> {
    let from = CString::new(src.as_os_str().as_bytes())?;
    let to = CString::new(dst.as_os_str().as_bytes())?;
    // SAFETY: both paths are NUL-terminated C strings that outlive the call.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // A filesystem without RENAME_NOREPLACE, such as NFS, answers EINVAL.
    if err.raw_os_error() == Some(libc::EINVAL) {
        return rename_noreplace_fallback(src, dst);
    }
    Err(err)
}

/// For a filesystem without RENAME_NOREPLACE. A file or link is linked to its new name, which
/// fails if the name is taken, then unlinked from the old one. A folder is renamed: rename(2) puts
/// a folder only over an empty folder, never over a file, a link, or a folder with contents.
fn rename_noreplace_fallback(src: &Path, dst: &Path) -> io::Result<()> {
    if fs::symlink_metadata(src)?.is_dir() {
        if fs::symlink_metadata(dst).is_ok() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        return fs::rename(src, dst);
    }
    fs::hard_link(src, dst)?;
    fs::remove_file(src)
}

/// The name to try on attempt `idx`: `name`, then `stem (copy).ext`, `stem (copy 2).ext`, …
/// A folder's name is never split at a dot.
fn candidate_name(name: &OsStr, idx: usize, is_folder: bool) -> OsString {
    if idx == 0 {
        return name.to_os_string();
    }
    let bytes = name.as_bytes();
    let split = match bytes.iter().rposition(|b| *b == b'.') {
        _ if is_folder => bytes.len(),
        Some(0) | None => bytes.len(),
        Some(dot) => dot,
    };
    let (stem, ext) = bytes.split_at(split);
    let mut out = stem.to_vec();
    if idx == 1 {
        out.extend_from_slice(b" (copy)");
    } else {
        out.extend_from_slice(format!(" (copy {idx})").as_bytes());
    }
    out.extend_from_slice(ext);
    OsString::from_vec(out)
}

/// Calls `place` with each free-looking name in `dir` until it succeeds. `place` must create its
/// target atomically and fail with `AlreadyExists` only when that name is taken, so nothing is ever
/// overwritten.
fn place_new(
    dir: &Path,
    name: &OsStr,
    is_folder: bool,
    mut place: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    for idx in 0..10_000 {
        let target = dir.join(candidate_name(name, idx, is_folder));
        match place(&target) {
            Ok(()) => return Ok(target),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("every free name is taken"))
}

/// A folder itself, not a link to one.
fn is_real_folder(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

pub fn create_folder(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let valid = validate_entry_name(name)?;
    let target = dir.join(&valid);
    match fs::create_dir(&target) {
        Ok(()) => Ok(target),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(format!("“{valid}” already exists.")),
        Err(e) => Err(format!("Could not create folder: {e}")),
    }
}

pub fn rename_entry(src: &Path, new_name: &str) -> Result<PathBuf, String> {
    let valid = validate_entry_name(new_name)?;
    let parent = src
        .parent()
        .ok_or_else(|| "Cannot rename the root directory.".to_string())?;
    let target = parent.join(&valid);
    if target == src {
        return Ok(target);
    }
    match rename_noreplace(src, &target) {
        Ok(()) => Ok(target),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(format!("“{valid}” already exists.")),
        Err(e) => Err(format!("Could not rename: {e}")),
    }
}

/// Copies `src` to `dst`, which must not exist. On failure it removes what it made, and only that:
/// nothing another program put in a new folder meanwhile.
///
/// It fails with `AlreadyExists` only when `dst` itself is taken. A clash further in, as on a drive
/// that ignores case, is a failure of this copy, not a reason to try another name.
fn copy_tree(src: &Path, dst: &Path) -> io::Result<()> {
    let mut made = Vec::new();
    let Err(err) = copy_node(src, dst, &mut made) else {
        return Ok(());
    };
    remove_made(&made);
    Err(copy_failure(err, !made.is_empty()))
}

/// What a failed copy reports. `AlreadyExists` survives only if the copy made nothing, which means
/// its own target was taken and `place_new` should try another name.
fn copy_failure(err: io::Error, made_anything: bool) -> io::Error {
    if err.kind() == io::ErrorKind::AlreadyExists && made_anything {
        io::Error::other(format!("two names inside clash on the destination: {err}"))
    } else {
        err
    }
}

fn copy_node(src: &Path, dst: &Path, made: &mut Vec<PathBuf>) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)?;
        made.push(dst.to_path_buf());
        return Ok(());
    }
    if kind.is_dir() {
        fs::DirBuilder::new().mode(0o700).create(dst)?;
        made.push(dst.to_path_buf());
        for item in fs::read_dir(src)? {
            let item = item?;
            copy_node(&item.path(), &dst.join(item.file_name()), made)?;
        }
        if let Ok(modified) = meta.modified() {
            let _ = fs::File::open(dst).and_then(|d| d.set_modified(modified));
        }
        // Last, so a read-only folder can still be filled.
        return fs::set_permissions(dst, meta.permissions());
    }
    // Opened without blocking and checked once open, so a pipe swapped in for a file never blocks.
    let mut input = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(src)?;
    let before = input.metadata()?;
    if !before.is_file() {
        return Err(io::Error::other(format!(
            "{} is a pipe, socket, or device, which Files does not copy",
            src.display()
        )));
    }
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dst)?;
    made.push(dst.to_path_buf());
    io::copy(&mut input, &mut output)?;
    if changed_since(&before, &input.metadata()?) {
        return Err(io::Error::other(format!(
            "{} changed while it was being copied",
            src.display()
        )));
    }
    output.set_modified(before.modified()?)?;
    output.set_permissions(before.permissions())
}

/// Whether a file was written to between two looks at it.
fn changed_since(before: &Metadata, after: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    (
        before.size(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.size(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    )
}

/// Removes what a failed copy made, newest first, read-only folders included. A folder that now
/// holds something the copy did not make stays.
fn remove_made(made: &[PathBuf]) {
    for path in made {
        if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        }
    }
    for path in made.iter().rev() {
        let _ = match fs::symlink_metadata(path) {
            Ok(m) if m.is_dir() => fs::remove_dir(path),
            Ok(_) => fs::remove_file(path),
            Err(_) => Ok(()),
        };
    }
}

/// The device and inode of what is at `path`, without following a link.
fn identity(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let meta = fs::symlink_metadata(path)?;
    Ok((meta.dev(), meta.ino()))
}

/// Removes the original after a copy between filesystems, unless something else now stands at its
/// path, which then stays.
fn remove_original(src: &Path, was: (u64, u64)) -> io::Result<()> {
    if identity(src)? != was {
        return Err(io::Error::other(
            "it was replaced while it was being copied, so it was kept",
        ));
    }
    if is_real_folder(src) {
        fs::remove_dir_all(src)
    } else {
        fs::remove_file(src)
    }
}

/// Moves `src` to `dst` without replacing anything. Only between filesystems does it copy, and then
/// it reports whether it did, so the caller removes the original last.
fn move_noreplace(src: &Path, dst: &Path, copied: &mut bool) -> io::Result<()> {
    match rename_noreplace(src, dst) {
        Err(e) if is_cross_device(&e) => {
            copy_tree(src, dst)?;
            *copied = true;
            Ok(())
        }
        other => other,
    }
}

fn is_ancestor(src: &Path, dst_dir: &Path) -> bool {
    let src_canon = fs::canonicalize(src).unwrap_or_else(|_| src.to_path_buf());
    let dst_canon = fs::canonicalize(dst_dir).unwrap_or_else(|_| dst_dir.to_path_buf());
    dst_canon == src_canon || dst_canon.starts_with(&src_canon)
}

fn same_folder(a: &Path, b: &Path) -> bool {
    a == b || matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

pub fn copy_entry(src: &Path, dst_dir: &Path) -> Result<PathBuf, String> {
    let is_folder = is_real_folder(src);
    if is_folder && is_ancestor(src, dst_dir) {
        return Err("Cannot copy a folder into itself.".into());
    }
    let name = file_name_of(src)?;
    place_new(dst_dir, name, is_folder, |dst| copy_tree(src, dst))
        .map_err(|e| format!("Could not copy “{}”: {e}", name.to_string_lossy()))
}

pub fn move_entry(src: &Path, dst_dir: &Path) -> Result<PathBuf, String> {
    let is_folder = is_real_folder(src);
    if is_folder && is_ancestor(src, dst_dir) {
        return Err("Cannot move a folder into itself.".into());
    }
    let name = file_name_of(src)?;
    if src.parent().is_some_and(|parent| same_folder(parent, dst_dir)) {
        return Ok(src.to_path_buf());
    }
    let was = identity(src).map_err(|e| format!("Cannot read {}: {e}", src.display()))?;
    let mut copied = false;
    let dst = place_new(dst_dir, name, is_folder, |dst| move_noreplace(src, dst, &mut copied))
        .map_err(|e| format!("Could not move “{}”: {e}", name.to_string_lossy()))?;
    if copied {
        remove_original(src, was).map_err(|e| {
            format!(
                "Copied “{}” to {}, but did not remove the original: {e}",
                name.to_string_lossy(),
                dst.display()
            )
        })?;
    }
    Ok(dst)
}

pub fn duplicate_entry(src: &Path) -> Result<PathBuf, String> {
    let parent = src.parent().ok_or_else(|| "Cannot duplicate root.".to_string())?;
    copy_entry(src, parent)
}

pub fn move_to_trash_in(src: &Path, trash: &Path) -> Result<PathBuf, String> {
    let files_dir = trash.join("files");
    let info_dir = trash.join("info");
    fs::create_dir_all(&files_dir).map_err(|e| format!("Cannot create Trash/files: {e}"))?;
    fs::create_dir_all(&info_dir).map_err(|e| format!("Cannot create Trash/info: {e}"))?;
    let is_folder = is_real_folder(src);
    if is_folder && is_ancestor(src, trash) {
        return Err("This folder holds the Trash, so it cannot go in the Trash.".into());
    }

    let name = file_name_of(src)?;
    let was = identity(src).map_err(|e| format!("Cannot read {}: {e}", src.display()))?;
    let original = std::path::absolute(src).map_err(|e| format!("Cannot resolve {}: {e}", src.display()))?;
    let now: DateTime<Local> = SystemTime::now().into();
    let info_body = format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        encode_percent_path(&original),
        now.format("%Y-%m-%dT%H:%M:%S")
    );

    let mut copied = false;
    let dst = place_new(&files_dir, name, is_folder, |dst| {
        // Reserve the name with its .trashinfo first, atomically, as the Trash specification asks.
        let info_path = info_dir.join(trashinfo_name(file_name_of(dst).map_err(io::Error::other)?));
        let mut info = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&info_path)?;
        let placed = info
            .write_all(info_body.as_bytes())
            .and_then(|()| move_noreplace(src, dst, &mut copied));
        if placed.is_err() {
            let _ = fs::remove_file(&info_path);
        }
        placed
    })
    .map_err(|e| format!("Could not move “{}” to Trash: {e}", name.to_string_lossy()))?;

    if copied {
        // The Trash holds a full copy. Keep it, whatever happens to the original.
        remove_original(src, was).map_err(|e| {
            format!(
                "A copy of “{}” is in the Trash, but the original was not fully removed: {e}",
                name.to_string_lossy()
            )
        })?;
    }
    Ok(dst)
}

pub fn move_to_trash(src: &Path) -> Result<PathBuf, String> {
    move_to_trash_in(src, &trash_root())
}

pub fn restore_from_trash_in(trash_file: &Path, trash: &Path) -> Result<PathBuf, String> {
    let name = file_name_of(trash_file)?;
    let info_path = trash.join("info").join(trashinfo_name(name));
    let original = parse_trashinfo_path(&info_path, trash)
        .ok_or_else(|| "Original location is unknown for this Trash item.".to_string())?;
    let parent = original
        .parent()
        .ok_or_else(|| "Invalid original directory.".to_string())?;
    fs::create_dir_all(parent).map_err(|e| format!("Cannot recreate parent folder: {e}"))?;
    let orig_name = original.file_name().unwrap_or(name);
    let was = identity(trash_file).map_err(|e| format!("Cannot read {}: {e}", trash_file.display()))?;
    let mut copied = false;
    let dst = place_new(parent, orig_name, is_real_folder(trash_file), |dst| {
        move_noreplace(trash_file, dst, &mut copied)
    })
    .map_err(|e| format!("Could not restore “{}”: {e}", orig_name.to_string_lossy()))?;
    if copied {
        remove_original(trash_file, was).map_err(|e| {
            format!(
                "Restored a copy to {}, but did not remove it from the Trash: {e}",
                dst.display()
            )
        })?;
    }
    let _ = fs::remove_file(&info_path);
    Ok(dst)
}

pub fn restore_from_trash(trash_file: &Path) -> Result<PathBuf, String> {
    restore_from_trash_in(trash_file, &trash_root())
}

pub fn delete_permanently(target: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(target).map_err(|e| format!("Cannot access {}: {e}", target.display()))?;
    if meta.is_dir() && !meta.file_type().is_symlink() {
        fs::remove_dir_all(target).map_err(|e| format!("Cannot remove folder: {e}"))?;
    } else {
        fs::remove_file(target).map_err(|e| format!("Cannot remove file: {e}"))?;
    }
    // Also clean up companion .trashinfo if deleting an entry from Trash/files.
    let trash_files = trash_root().join("files");
    if target.parent() == Some(trash_files.as_path())
        && let Some(name) = target.file_name()
    {
        let _ = fs::remove_file(trash_root().join("info").join(trashinfo_name(name)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    fn temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
        let p = env::temp_dir().join(format!("crowbaros-files-test-{}-{id}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn formats_sizes_and_counts() {
        assert_eq!(format_size(63), "63 B");
        assert_eq!(format_size(11_800), "11.8 KB");
        assert_eq!(format_size(342_800), "342.8 KB");
        assert_eq!(format_size(3_000_000), "3.0 MB");
        assert_eq!(format_item_count(Some(0)), "No items");
        assert_eq!(format_item_count(Some(1)), "1 item");
        assert_eq!(format_item_count(Some(20)), "20 items");
        assert_eq!(format_permissions(0o755), "rwxr-xr-x");
        assert_eq!(format_permissions(0o644), "rw-r--r--");
    }

    #[test]
    fn breadcrumbs_and_path_expansion() {
        let home = Path::new("/home/alex");
        assert_eq!(breadcrumbs(home, home), vec![("~".into(), PathBuf::from("/home/alex"))]);
        assert_eq!(
            breadcrumbs(Path::new("/home/alex/Code/crowbaros"), home),
            vec![
                ("~".into(), PathBuf::from("/home/alex")),
                ("Code".into(), PathBuf::from("/home/alex/Code")),
                ("crowbaros".into(), PathBuf::from("/home/alex/Code/crowbaros")),
            ]
        );
        assert_eq!(
            expand_path("~/Code", home, Path::new("/tmp")),
            PathBuf::from("/home/alex/Code")
        );
        assert_eq!(
            expand_path("file:///home/alex/My%20Docs", home, home),
            PathBuf::from("/home/alex/My Docs")
        );
        assert_eq!(rename_stem_end("report.pdf", false), 6);
        assert_eq!(rename_stem_end(".bashrc", false), 7);
        assert_eq!(rename_stem_end("archive.tar.gz", false), 11);
        assert_eq!(rename_stem_end("v1.0", true), 4);
    }

    #[test]
    fn reads_sorts_and_never_overwrites_on_copy_or_rename() {
        let dir = temp_dir("ops");
        fs::create_dir(dir.join("beta")).unwrap();
        fs::create_dir(dir.join("Alpha")).unwrap();
        fs::write(dir.join("note.txt"), "hello world\nline 2\n").unwrap();
        fs::write(dir.join(".hidden"), "secret").unwrap();

        let entries = read_directory(&dir, false, SortField::Name, SortOrder::Asc).unwrap();
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "beta", "note.txt"]);

        let with_hidden = read_directory(&dir, true, SortField::Name, SortOrder::Asc).unwrap();
        assert_eq!(with_hidden.len(), 4);

        // Copy never overwrites.
        let copy1 = copy_entry(&dir.join("note.txt"), &dir).unwrap();
        assert_eq!(copy1.file_name().unwrap(), "note (copy).txt");
        let copy2 = copy_entry(&dir.join("note.txt"), &dir).unwrap();
        assert_eq!(copy2.file_name().unwrap(), "note (copy 2).txt");

        // Rename rejects collisions and slashes.
        assert!(rename_entry(&copy2, "note.txt").is_err());
        assert!(rename_entry(&copy2, "bad/name").is_err());
        let renamed = rename_entry(&copy2, "renamed.txt").unwrap();
        assert!(renamed.exists());

        // Cannot copy or move a directory into itself.
        assert!(copy_entry(&dir.join("Alpha"), &dir.join("Alpha")).is_err());
        assert!(move_entry(&dir.join("Alpha"), &dir.join("Alpha")).is_err());

        // Text preview reads text and rejects binary NUL bytes.
        assert_eq!(
            read_text_preview(&dir.join("note.txt"), 1024, 10).as_deref(),
            Some("hello world\nline 2")
        );
        fs::write(dir.join("bin.dat"), b"\x00\x01\x02").unwrap();
        assert!(read_text_preview(&dir.join("bin.dat"), 1024, 10).is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn trash_and_restore_round_trip() {
        let root = temp_dir("trash");
        let work = root.join("work");
        let trash = root.join("Trash");
        fs::create_dir_all(&work).unwrap();
        let file = work.join("draft.md");
        fs::write(&file, "# Hello").unwrap();

        let trashed = move_to_trash_in(&file, &trash).unwrap();
        assert!(!file.exists());
        assert!(trashed.exists());

        let restored = restore_from_trash_in(&trashed, &trash).unwrap();
        assert_eq!(restored, file);
        assert_eq!(fs::read_to_string(&restored).unwrap(), "# Hello");
        assert!(!trashed.exists());

        let _ = fs::remove_dir_all(&root);
    }

    /// Root ignores permission bits, so the tests that rely on them say so and pass vacuously there.
    fn permissions_apply() -> bool {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() != 0 }
    }

    fn mkfifo(path: &Path) {
        let c = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: c is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    }

    fn count_entries(dir: &Path) -> usize {
        fs::read_dir(dir).map(|rd| rd.count()).unwrap_or(0)
    }

    #[test]
    fn trash_keeps_everything_when_the_move_is_refused() {
        if !permissions_apply() {
            return;
        }
        // A folder the person owns, inside a parent they cannot write: rename fails with EACCES.
        let root = temp_dir("trash-refused");
        let shared = root.join("shared");
        let mine = shared.join("mine");
        let trash = root.join("Trash");
        fs::create_dir_all(&mine).unwrap();
        for name in ["a", "b", "c"] {
            fs::write(mine.join(name), name).unwrap();
        }
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o555)).unwrap();

        assert!(move_to_trash_in(&mine, &trash).is_err());
        assert_eq!(count_entries(&mine), 3, "the original must be untouched");
        assert_eq!(count_entries(&trash.join("files")), 0);
        assert_eq!(count_entries(&trash.join("info")), 0);

        fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_that_holds_the_trash_is_refused() {
        let root = temp_dir("trash-holder");
        let home = root.join("home");
        let trash = home.join(".local/share/Trash");
        fs::create_dir_all(&trash).unwrap();
        fs::write(home.join("notes.txt"), "keep").unwrap();

        assert!(move_to_trash_in(&home, &trash).is_err());
        assert!(home.join("notes.txt").exists());
        assert_eq!(count_entries(&trash.join("files")), 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn read_only_folders_copy_whole_and_keep_their_permissions() {
        let root = temp_dir("readonly-copy");
        let src = root.join("pkg");
        fs::create_dir_all(src.join("inner")).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        fs::write(src.join("inner/b.txt"), "b").unwrap();
        fs::set_permissions(src.join("inner"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&src, fs::Permissions::from_mode(0o555)).unwrap();
        let dst_dir = root.join("dst");
        fs::create_dir(&dst_dir).unwrap();

        let copy = copy_entry(&src, &dst_dir).unwrap();
        assert_eq!(fs::read_to_string(copy.join("a.txt")).unwrap(), "a");
        assert_eq!(fs::read_to_string(copy.join("inner/b.txt")).unwrap(), "b");
        assert_eq!(fs::metadata(&copy).unwrap().permissions().mode() & 0o777, 0o555);

        for dir in [&src, &src.join("inner"), &copy, &copy.join("inner")] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_failed_copy_leaves_nothing_behind() {
        let root = temp_dir("fifo-copy");
        let src = root.join("project");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a.txt"), "a").unwrap();
        mkfifo(&src.join("pipe"));
        let dst_dir = root.join("dst");
        fs::create_dir(&dst_dir).unwrap();

        assert!(copy_entry(&src, &dst_dir).is_err());
        assert_eq!(count_entries(&dst_dir), 0);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pipe_is_never_read_for_a_preview() {
        let root = temp_dir("fifo-preview");
        let pipe = root.join("notes.txt");
        mkfifo(&pipe);
        // Opening a pipe with no writer would block forever without O_NONBLOCK.
        assert!(read_text_preview(&pipe, 1024, 10).is_none());
        let entry = build_entry(pipe, false).unwrap();
        assert!(!entry.is_previewable_text());
        assert!(!entry.is_previewable_image());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rename_never_replaces_an_existing_target() {
        let root = temp_dir("noreplace");
        fs::write(root.join("a"), "a").unwrap();
        fs::write(root.join("b"), "b").unwrap();

        let err = rename_noreplace(&root.join("a"), &root.join("b")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(root.join("b")).unwrap(), "b");
        assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "a");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn trash_never_overwrites_an_existing_trashinfo() {
        let root = temp_dir("trashinfo");
        let trash = root.join("Trash");
        fs::create_dir_all(trash.join("info")).unwrap();
        fs::create_dir_all(trash.join("files")).unwrap();
        let stray = trash.join("info/draft.md.trashinfo");
        fs::write(&stray, "[Trash Info]\nPath=/elsewhere/draft.md\n").unwrap();
        let file = root.join("draft.md");
        fs::write(&file, "x").unwrap();

        let trashed = move_to_trash_in(&file, &trash).unwrap();
        assert_eq!(trashed.file_name().unwrap(), "draft (copy).md");
        assert_eq!(
            fs::read_to_string(&stray).unwrap(),
            "[Trash Info]\nPath=/elsewhere/draft.md\n"
        );
        assert!(trash.join("info/draft (copy).md.trashinfo").exists());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn names_that_are_not_utf8_copy_trash_and_restore_byte_for_byte() {
        let root = temp_dir("non-utf8");
        let trash = root.join("Trash");
        let name = OsStr::from_bytes(b"caf\xe9.txt");
        let file = root.join(name);
        fs::write(&file, "x").unwrap();

        let copy = copy_entry(&file, &root).unwrap();
        assert_eq!(copy.file_name().unwrap().as_bytes(), b"caf\xe9 (copy).txt");

        let trashed = move_to_trash_in(&file, &trash).unwrap();
        let restored = restore_from_trash_in(&trashed, &trash).unwrap();
        assert_eq!(restored, file);
        assert_eq!(fs::read_to_string(&restored).unwrap(), "x");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_relative_trash_path_is_relative_to_the_folder_holding_the_trash() {
        let root = temp_dir("relative-trash");
        let trash = root.join(".Trash-1000");
        fs::create_dir_all(trash.join("files")).unwrap();
        fs::create_dir_all(trash.join("info")).unwrap();
        fs::write(trash.join("files/photo.png"), "png").unwrap();
        fs::write(
            trash.join("info/photo.png.trashinfo"),
            "[Trash Info]\nPath=Pictures/photo.png\n",
        )
        .unwrap();

        let restored = restore_from_trash_in(&trash.join("files/photo.png"), &trash).unwrap();
        assert_eq!(restored, root.join("Pictures/photo.png"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parent_of_a_link_is_the_parent_of_its_target() {
        let root = temp_dir("link-parent");
        fs::create_dir_all(root.join("a/b")).unwrap();
        std::os::unix::fs::symlink(root.join("a/b"), root.join("link")).unwrap();
        let canon = fs::canonicalize(&root).unwrap();

        assert_eq!(normalize_path(&canon.join("link/..")), canon.join("a"));
        assert_eq!(normalize_path(&canon.join("a/b/..")), canon.join("a"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn file_uris_with_a_host_keep_their_path() {
        let home = Path::new("/home/alex");
        assert_eq!(expand_path("file://laptop/tmp/x", home, home), PathBuf::from("/tmp/x"));
        assert_eq!(
            expand_path("file://localhost/tmp/x", home, home),
            PathBuf::from("/tmp/x")
        );
    }

    #[test]
    fn rename_keeps_spaces_the_person_typed() {
        assert_eq!(validate_entry_name("foo ").unwrap(), "foo ");
        assert!(validate_entry_name("   ").is_err());
    }

    /// Two tmpfs mounts are two filesystems, so a rename between them fails with EXDEV.
    fn second_filesystem(root: &Path) -> Option<PathBuf> {
        use std::os::unix::fs::MetadataExt;
        let shm = Path::new("/dev/shm");
        let dev = |p: &Path| fs::metadata(p).ok().map(|m| m.dev());
        if dev(shm)? == dev(root)? {
            return None;
        }
        let dir = shm.join(root.file_name()?);
        fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    #[test]
    fn moves_between_filesystems_copy_then_remove_the_original() {
        let root = temp_dir("cross-device");
        let Some(other) = second_filesystem(&root) else {
            let _ = fs::remove_dir_all(&root);
            return;
        };
        let src = root.join("album");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("song.flac"), "la").unwrap();

        let then = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        fs::File::options()
            .write(true)
            .open(src.join("song.flac"))
            .unwrap()
            .set_modified(then)
            .unwrap();

        let moved = move_entry(&src, &other).unwrap();
        assert!(!src.exists());
        assert_eq!(fs::read_to_string(moved.join("song.flac")).unwrap(), "la");
        assert_eq!(fs::metadata(moved.join("song.flac")).unwrap().modified().unwrap(), then);

        let trash = root.join("Trash");
        let trashed = move_to_trash_in(&moved, &trash).unwrap();
        assert!(!moved.exists());
        let restored = restore_from_trash_in(&trashed, &trash).unwrap();
        assert_eq!(restored, moved);
        assert_eq!(fs::read_to_string(restored.join("song.flac")).unwrap(), "la");

        let _ = fs::remove_dir_all(&other);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_clash_inside_a_copy_is_a_failure_not_a_taken_name() {
        let clash = || io::Error::from(io::ErrorKind::AlreadyExists);
        assert_eq!(copy_failure(clash(), false).kind(), io::ErrorKind::AlreadyExists);
        assert_ne!(copy_failure(clash(), true).kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn a_failed_copy_spares_what_another_program_put_in_its_folder() {
        let root = temp_dir("spare-foreign");
        let dst = root.join("copy");
        fs::create_dir(&dst).unwrap();
        fs::write(dst.join("a.txt"), "made").unwrap();
        fs::write(dst.join("theirs.txt"), "not ours").unwrap();

        remove_made(&[dst.clone(), dst.join("a.txt")]);
        assert!(!dst.join("a.txt").exists());
        assert_eq!(fs::read_to_string(dst.join("theirs.txt")).unwrap(), "not ours");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_write_between_two_looks_counts_as_a_change() {
        let root = temp_dir("changed");
        let file = root.join("doc.txt");
        fs::write(&file, "one").unwrap();
        let before = fs::metadata(&file).unwrap();
        assert!(!changed_since(&before, &fs::metadata(&file).unwrap()));
        fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(b"two")
            .unwrap();
        assert!(changed_since(&before, &fs::metadata(&file).unwrap()));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_original_replaced_during_a_copy_is_kept() {
        let root = temp_dir("replaced");
        let src = root.join("doc.txt");
        fs::write(&src, "old").unwrap();
        let was = identity(&src).unwrap();
        // Saved the usual way: a new file renamed over the old one.
        fs::write(root.join("doc.txt.new"), "new").unwrap();
        fs::rename(root.join("doc.txt.new"), &src).unwrap();

        assert!(remove_original(&src, was).is_err());
        assert_eq!(fs::read_to_string(&src).unwrap(), "new");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_fallback_rename_never_replaces_either() {
        let root = temp_dir("fallback");
        fs::write(root.join("a"), "a").unwrap();
        fs::write(root.join("b"), "b").unwrap();
        fs::create_dir(root.join("dir")).unwrap();

        let err = rename_noreplace_fallback(&root.join("a"), &root.join("b")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(root.join("a")).unwrap(), "a");
        assert_eq!(fs::read_to_string(root.join("b")).unwrap(), "b");
        assert!(rename_noreplace_fallback(&root.join("dir"), &root.join("b")).is_err());
        assert_eq!(fs::read_to_string(root.join("b")).unwrap(), "b");

        rename_noreplace_fallback(&root.join("a"), &root.join("c")).unwrap();
        assert!(!root.join("a").exists());
        assert_eq!(fs::read_to_string(root.join("c")).unwrap(), "a");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn links_are_judged_as_links_not_as_their_folders() {
        let root = temp_dir("links");
        let home = root.join("home");
        let trash = home.join(".local/share/Trash");
        fs::create_dir_all(&trash).unwrap();
        fs::create_dir(home.join("docs")).unwrap();
        fs::write(home.join("docs/x.txt"), "x").unwrap();
        std::os::unix::fs::symlink(&home, root.join("shortcut")).unwrap();
        std::os::unix::fs::symlink(home.join("docs"), home.join("link")).unwrap();

        // A link to the folder holding the Trash can go in the Trash; the folder stays.
        move_to_trash_in(&root.join("shortcut"), &trash).unwrap();
        assert!(home.join("docs/x.txt").exists());

        // Pasting a cut item into the folder it is already in, by another path, leaves it alone.
        let same = move_entry(&home.join("link/x.txt"), &home.join("docs")).unwrap();
        assert_eq!(same, home.join("link/x.txt"));
        assert!(home.join("docs/x.txt").exists());
        assert!(!home.join("docs/x (copy).txt").exists());

        // A link to a folder copies into that folder as a link.
        let copied = copy_entry(&home.join("link"), &home.join("docs")).unwrap();
        assert!(fs::symlink_metadata(&copied).unwrap().file_type().is_symlink());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_with_a_dot_duplicates_whole_and_files_keep_their_times() {
        let root = temp_dir("dup");
        fs::create_dir(root.join("v1.0")).unwrap();
        let dup = duplicate_entry(&root.join("v1.0")).unwrap();
        assert_eq!(dup.file_name().unwrap(), "v1.0 (copy)");

        let file = root.join("old.txt");
        fs::write(&file, "x").unwrap();
        let then = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(then)
            .unwrap();
        let copy = duplicate_entry(&file).unwrap();
        assert_eq!(fs::metadata(&copy).unwrap().modified().unwrap(), then);

        let _ = fs::remove_dir_all(&root);
    }

    fn image_entry(name: &str, pixels: Option<u64>) -> Entry {
        let path = PathBuf::from(format!("/pics/{name}"));
        let (category, kind_label) = classify(&path, false);
        Entry {
            name: name.into(),
            path,
            is_dir: false,
            is_symlink: false,
            symlink_target: None,
            size: 0,
            child_count: None,
            modified: None,
            mode: 0o644,
            hidden: false,
            category,
            kind_label,
            trash_original: None,
            pixels,
        }
    }

    #[test]
    fn grid_thumbnails_stay_within_their_budget_and_skip_gifs() {
        let mp = 1_000_000;
        let entries = vec![
            image_entry("a.jpg", Some(30 * mp)),
            image_entry("b.gif", Some(mp)),
            image_entry("c.png", Some(30 * mp)),
            image_entry("d.jpg", Some(30 * mp)),
            image_entry("e.png", Some(4 * mp)),
            image_entry("f.png", None),
            image_entry("notes.txt", None),
        ];
        assert_eq!(
            thumbnail_choices(&entries, THUMBNAIL_BUDGET_PIXELS),
            vec![true, false, true, false, true, false, false]
        );
        assert!(image_entry("big.jpg", Some(PREVIEW_MAX_PIXELS)).can_preview_image());
        assert!(!image_entry("huge.jpg", Some(PREVIEW_MAX_PIXELS + 1)).can_preview_image());
        assert!(!image_entry("unknown.jpg", None).can_preview_image());
    }

    #[test]
    fn an_image_size_comes_from_its_header() {
        let root = temp_dir("pixels");
        image::RgbaImage::new(3, 2).save(root.join("tiny.png")).unwrap();
        let entries = read_directory(&root, false, SortField::Name, SortOrder::Asc).unwrap();
        assert_eq!(entries[0].pixels, Some(6));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn search_finds_nested_files() {
        let root = temp_dir("search");
        fs::create_dir_all(root.join("nested/deep")).unwrap();
        fs::write(root.join("nested/deep/stream-test2.png"), b"png").unwrap();
        fs::write(root.join("other.txt"), b"txt").unwrap();

        let hits = search_below(&root, "stream test2", false, 50);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "stream-test2.png");

        let _ = fs::remove_dir_all(&root);
    }
}
