// SPDX-License-Identifier: MIT
//! Default Applications management via `$XDG_CONFIG_HOME/mimeapps.list`.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AppCategory {
    Browser,
    Terminal,
    FileManager,
    TextEditor,
    Viewer,
    MediaPlayer,
}

impl AppCategory {
    pub const ALL: [Self; 6] = [
        Self::Browser,
        Self::Terminal,
        Self::FileManager,
        Self::TextEditor,
        Self::Viewer,
        Self::MediaPlayer,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Browser => "Web Browser",
            Self::Terminal => "Terminal",
            Self::FileManager => "File Manager",
            Self::TextEditor => "Text Editor",
            Self::Viewer => "Image & PDF Viewer",
            Self::MediaPlayer => "Media Player",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Browser => "Opens web links and HTML documents",
            Self::Terminal => "Default terminal emulator",
            Self::FileManager => "Opens folders and directories",
            Self::TextEditor => "Opens plain text and Markdown files",
            Self::Viewer => "Opens images and PDF documents",
            Self::MediaPlayer => "Plays video and audio files",
        }
    }

    pub fn mimes(self) -> &'static [&'static str] {
        match self {
            Self::Browser => &[
                "x-scheme-handler/http",
                "x-scheme-handler/https",
                "text/html",
                "application/xhtml+xml",
            ],
            Self::Terminal => &["x-scheme-handler/terminal"],
            Self::FileManager => &["inode/directory"],
            Self::TextEditor => &["text/plain", "text/markdown"],
            Self::Viewer => &["image/png", "image/jpeg", "image/webp", "application/pdf"],
            Self::MediaPlayer => &[
                "video/mp4",
                "video/x-matroska",
                "video/webm",
                "audio/mpeg",
                "audio/flac",
                "audio/ogg",
                "audio/wav",
            ],
        }
    }

    pub fn desktop_category_hint(self) -> &'static str {
        match self {
            Self::Browser => "WebBrowser",
            Self::Terminal => "TerminalEmulator",
            Self::FileManager => "FileManager",
            Self::TextEditor => "TextEditor",
            Self::Viewer => "Viewer",
            Self::MediaPlayer => "AudioVideo",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppCandidate {
    pub desktop_id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CategoryState {
    pub category: AppCategory,
    pub current: Option<String>,
    pub candidates: Vec<AppCandidate>,
}

/// Lossless INI representation of `mimeapps.list` preserving section order and
/// unrelated sections/keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MimeApps {
    sections: Vec<(String, Vec<(String, String)>)>,
}

impl MimeApps {
    pub fn parse(text: &str) -> Self {
        let mut sections: Vec<(String, Vec<(String, String)>)> = Vec::new();
        for raw_line in text.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                let name = line[1..line.len() - 1].trim().to_string();
                if !sections.iter().any(|(s, _)| s == &name) {
                    sections.push((name, Vec::new()));
                }
            } else if let Some((k, v)) = line.split_once('=') {
                let key = k.trim().to_string();
                let val = v.trim().to_string();
                if sections.is_empty() {
                    sections.push(("Default Applications".into(), Vec::new()));
                }
                if let Some((_, entries)) = sections.last_mut() {
                    if let Some(existing) = entries.iter_mut().find(|(ek, _)| ek == &key) {
                        existing.1 = val;
                    } else {
                        entries.push((key, val));
                    }
                }
            }
        }
        Self { sections }
    }

    pub fn get_default(&self, mime: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|(s, _)| s == "Default Applications")
            .and_then(|(_, entries)| entries.iter().find(|(k, _)| k == mime))
            .map(|(_, v)| v.split(';').next().unwrap_or(v).trim())
            .filter(|v| !v.is_empty())
    }

    pub fn set_default(&mut self, mime: &str, desktop_id: &str) {
        if let Some((_, entries)) = self
            .sections
            .iter_mut()
            .find(|(s, _)| s == "Default Applications")
        {
            if let Some(existing) = entries.iter_mut().find(|(k, _)| k == mime) {
                existing.1 = desktop_id.to_string();
            } else {
                entries.push((mime.to_string(), desktop_id.to_string()));
            }
        } else {
            self.sections.insert(
                0,
                (
                    "Default Applications".into(),
                    vec![(mime.to_string(), desktop_id.to_string())],
                ),
            );
        }
    }

    pub fn serialize(&self) -> String {
        let mut out = String::new();
        for (idx, (section, entries)) in self.sections.iter().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            out.push('[');
            out.push_str(section);
            out.push_str("]\n");
            for (k, v) in entries {
                out.push_str(k);
                out.push('=');
                out.push_str(v);
                out.push('\n');
            }
        }
        out
    }
}

pub fn user_mimeapps_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| PathBuf::from(p).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .ok_or("Cannot find your configuration directory.")?;
    Ok(base.join("mimeapps.list"))
}

pub fn save_mimeapps_atomic(path: &Path, mimeapps: &MimeApps) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid mimeapps.list path.")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|e| format!("Could not create config directory: {e}"))?;
    let content = mimeapps.serialize();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = parent.join(format!(".mimeapps.list.tmp.{}.{stamp}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .open(&tmp)
        .map_err(|e| format!("Could not stage mimeapps.list: {e}"))?;
    file.write_all(content.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("Could not write mimeapps.list: {e}")
        })?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Could not save mimeapps.list: {e}")
    })
}

struct DesktopInfo {
    desktop_id: String,
    name: String,
    mimes: Vec<String>,
    categories: Vec<String>,
}

fn parse_desktop_entry(desktop_id: String, text: &str) -> Option<DesktopInfo> {
    let mut in_main = false;
    let mut name = None;
    let mut mimes = Vec::new();
    let mut categories = Vec::new();
    let mut hidden = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_main = line == "[Desktop Entry]";
            continue;
        }
        if !in_main || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "Name" if name.is_none() => name = Some(v.trim().to_string()),
                "MimeType" => {
                    mimes.extend(
                        v.split(';')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                    );
                }
                "Categories" => {
                    categories.extend(
                        v.split(';')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                    );
                }
                "Hidden" | "NoDisplay" if v.trim().eq_ignore_ascii_case("true") => {
                    hidden = true;
                }
                _ => {}
            }
        }
    }
    if hidden {
        return None;
    }
    Some(DesktopInfo {
        name: name.unwrap_or_else(|| desktop_id.trim_end_matches(".desktop").to_string()),
        desktop_id,
        mimes,
        categories,
    })
}

fn scan_desktop_dirs(dirs: &[PathBuf]) -> Vec<DesktopInfo> {
    let mut by_id: BTreeMap<String, DesktopInfo> = BTreeMap::new();
    for dir in dirs {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "desktop") {
                continue;
            }
            let Some(id) = path.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
                continue;
            };
            if by_id.contains_key(&id) {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path)
                && let Some(info) = parse_desktop_entry(id.clone(), &text)
            {
                by_id.insert(id, info);
            }
        }
    }
    by_id.into_values().collect()
}

pub fn load_default_apps() -> Vec<CategoryState> {
    let user_mime = user_mimeapps_path()
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .map(|t| MimeApps::parse(&t))
        .unwrap_or_default();
    let system_mime = fs::read_to_string("/etc/xdg/mimeapps.list")
        .map(|t| MimeApps::parse(&t))
        .unwrap_or_default();

    let mut dirs = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".local/share/applications"));
    }
    dirs.push(PathBuf::from("/usr/local/share/applications"));
    dirs.push(PathBuf::from("/usr/share/applications"));
    let installed = scan_desktop_dirs(&dirs);

    build_category_states(&user_mime, &system_mime, &installed)
}

fn build_category_states(
    user_mime: &MimeApps,
    system_mime: &MimeApps,
    installed: &[DesktopInfo],
) -> Vec<CategoryState> {
    let names_by_id: HashMap<&str, &str> = installed
        .iter()
        .map(|d| (d.desktop_id.as_str(), d.name.as_str()))
        .collect();

    AppCategory::ALL
        .into_iter()
        .map(|category| {
            let primary_mime = category.mimes()[0];
            let current = user_mime
                .get_default(primary_mime)
                .or_else(|| system_mime.get_default(primary_mime))
                .map(str::to_string);

            let mut candidates: Vec<AppCandidate> = installed
                .iter()
                .filter(|d| {
                    category
                        .mimes()
                        .iter()
                        .any(|m| d.mimes.iter().any(|dm| dm == m))
                        || d.categories
                            .iter()
                            .any(|c| c == category.desktop_category_hint())
                })
                .map(|d| AppCandidate {
                    desktop_id: d.desktop_id.clone(),
                    name: d.name.clone(),
                })
                .collect();

            if let Some(cur) = &current
                && !candidates.iter().any(|c| &c.desktop_id == cur)
            {
                candidates.push(AppCandidate {
                    desktop_id: cur.clone(),
                    name: names_by_id
                        .get(cur.as_str())
                        .copied()
                        .unwrap_or_else(|| cur.trim_end_matches(".desktop"))
                        .to_string(),
                });
            }
            candidates.sort_by_key(|a| a.name.to_lowercase());
            CategoryState {
                category,
                current,
                candidates,
            }
        })
        .collect()
}

pub fn set_category_default(category: AppCategory, desktop_id: &str) -> Result<(), String> {
    let path = user_mimeapps_path()?;
    set_category_default_at(&path, category, desktop_id)
}

pub fn set_category_default_at(
    path: &Path,
    category: AppCategory,
    desktop_id: &str,
) -> Result<(), String> {
    let mut mimeapps = fs::read_to_string(path)
        .map(|t| MimeApps::parse(&t))
        .unwrap_or_default();
    for mime in category.mimes() {
        mimeapps.set_default(mime, desktop_id);
    }
    save_mimeapps_atomic(path, &mimeapps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mimeapps_roundtrip_preserves_unrelated_sections_and_keys() {
        let initial = "\
[Default Applications]
inode/directory=com.crowbaros.Files.desktop
x-scheme-handler/https=chromium.desktop

[Added Associations]
text/plain=foot.desktop
";
        let mut parsed = MimeApps::parse(initial);
        assert_eq!(
            parsed.get_default("inode/directory"),
            Some("com.crowbaros.Files.desktop")
        );
        parsed.set_default("text/plain", "custom-editor.desktop");
        let serialized = parsed.serialize();
        let reparsed = MimeApps::parse(&serialized);
        assert_eq!(
            reparsed.get_default("inode/directory"),
            Some("com.crowbaros.Files.desktop")
        );
        assert_eq!(
            reparsed.get_default("text/plain"),
            Some("custom-editor.desktop")
        );
        assert!(serialized.contains("[Added Associations]\ntext/plain=foot.desktop\n"));
    }

    #[test]
    fn atomic_category_update_writes_all_category_mimes() {
        let dir = std::env::temp_dir().join(format!("crowbar-mime-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("mimeapps.list");
        set_category_default_at(&path, AppCategory::Browser, "firefox.desktop").unwrap();
        let parsed = MimeApps::parse(&fs::read_to_string(&path).unwrap());
        assert_eq!(parsed.get_default("x-scheme-handler/http"), Some("firefox.desktop"));
        assert_eq!(parsed.get_default("x-scheme-handler/https"), Some("firefox.desktop"));
        assert_eq!(parsed.get_default("text/html"), Some("firefox.desktop"));
        let _ = fs::remove_dir_all(&dir);
    }
}
