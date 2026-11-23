// SPDX-License-Identifier: MIT
//! Desktop-file discovery and ranking. Execution belongs to GIO, never a shell.
use serde::Serialize;
use std::os::unix::fs::PermissionsExt;
use std::{
    collections::{BTreeMap, HashSet},
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub wm_class: String,
    pub keywords: String,
}

impl AppEntry {
    pub fn icon(&self) -> &'static str {
        if self.id.contains("StudioSettings") {
            "settings"
        } else if self.id.contains("Files") {
            "files"
        } else if self.id.contains("chromium") || self.id.contains("browser") {
            "browser"
        } else if self.id.contains("foot")
            || self.id.contains("kitty")
            || self.id.contains("ghostty")
        {
            "terminal"
        } else {
            "application"
        }
    }
    pub fn matches_class(&self, class: &str) -> bool {
        !class.is_empty()
            && ((!self.wm_class.is_empty() && self.wm_class.eq_ignore_ascii_case(class))
                || self
                    .id
                    .trim_end_matches(".desktop")
                    .eq_ignore_ascii_case(class)
                || (self.id.eq_ignore_ascii_case("chromium.desktop")
                    && class.eq_ignore_ascii_case("chromium-browser")))
    }
}

fn unescape(s: &str) -> String {
    let mut chars = s.chars();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => out.push(' '),
            Some('n') | Some('r') | Some('t') => out.push(' '),
            Some('\\') => out.push('\\'),
            Some(c) => {
                out.push('\\');
                out.push(c);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn executable(value: &str) -> bool {
    let valid = |p: &Path| {
        fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if value.contains('/') {
        return valid(Path::new(value));
    }
    env::split_paths(&env::var_os("PATH").unwrap_or_default()).any(|p| valid(&p.join(value)))
}

fn parse(id: String, path: PathBuf, text: &str, locale: &str) -> Option<AppEntry> {
    let mut main = false;
    let mut fields = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            main = line == "[Desktop Entry]";
            continue;
        }
        if main && !line.starts_with('#') {
            if let Some((key, value)) = line.split_once('=') {
                fields.insert(key.trim(), value.trim());
            }
        }
    }
    let get = |key: &str| fields.get(key).copied().unwrap_or("");
    if get("Type") != "Application" || get("Hidden") == "true" || get("NoDisplay") == "true" {
        return None;
    }
    if !get("OnlyShowIn").is_empty() && !get("OnlyShowIn").split(';').any(|x| x == "Hyprland") {
        return None;
    }
    if get("NotShowIn").split(';').any(|x| x == "Hyprland") {
        return None;
    }
    if !get("TryExec").is_empty() && !executable(get("TryExec")) {
        return None;
    }
    if get("Exec").is_empty() && get("DBusActivatable") != "true" {
        return None;
    }
    let localized = |key: &str| {
        let lang = locale.split('_').next().unwrap_or(locale);
        [
            format!("{key}[{locale}]"),
            format!("{key}[{lang}]"),
            key.to_string(),
        ]
        .iter()
        .find_map(|key| fields.get(key.as_str()).map(|s| unescape(s)))
        .unwrap_or_default()
    };
    let name = localized("Name");
    if name.is_empty() {
        return None;
    }
    Some(AppEntry {
        id,
        path,
        name,
        description: localized("Comment"),
        wm_class: unescape(get("StartupWMClass")),
        keywords: localized("Keywords"),
    })
}

fn files(root: &Path, dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    if depth > 8 || out.len() > 10000 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            files(root, &p, depth + 1, out);
        } else if p.extension().is_some_and(|e| e == "desktop") {
            if let Ok(rel) = p.strip_prefix(root) {
                out.push((rel.to_string_lossy().replace('/', "-"), p));
            }
        }
    }
}

pub fn discover() -> Vec<AppEntry> {
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let data = env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"));
    let mut roots = vec![data];
    roots.extend(
        env::split_paths(
            &env::var_os("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into()),
        )
        .filter(|p| p.is_absolute()),
    );
    let locale = env::var("LC_MESSAGES")
        .or_else(|_| env::var("LANG"))
        .unwrap_or_default();
    let locale = locale.split('.').next().unwrap_or("en_US");
    let mut apps = Vec::new();
    let mut seen = HashSet::new();
    for root in roots {
        let root = root.join("applications");
        let mut entries = Vec::new();
        files(&root, &root, 0, &mut entries);
        entries.sort();
        for (id, path) in entries {
            // A user Hidden=true entry also masks the system entry with this ID.
            if !seen.insert(id.clone()) {
                continue;
            }
            if !fs::metadata(&path).is_ok_and(|m| m.len() <= 65536) {
                continue;
            }
            if let Ok(text) = fs::read_to_string(&path) {
                if let Some(app) = parse(id, path, &text, locale) {
                    apps.push(app);
                }
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

pub fn ranked(apps: &[AppEntry], query: &str) -> Vec<usize> {
    let q = query.trim().to_lowercase();
    let terms: Vec<_> = q.split_whitespace().collect();
    let mut matches: Vec<_> = apps
        .iter()
        .enumerate()
        .filter_map(|(i, a)| {
            let name = a.name.to_lowercase();
            let hay = format!("{} {} {} {}", name, a.description, a.keywords, a.id).to_lowercase();
            if !terms.iter().all(|t| hay.contains(t)) {
                return None;
            }
            Some((
                if name == q {
                    0
                } else if name.starts_with(&q) {
                    1
                } else {
                    2
                },
                name,
                i,
            ))
        })
        .collect();
    matches.sort();
    matches.into_iter().map(|(_, _, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    const BASE: &str = "[Desktop Entry]\nType=Application\nName=Studio Settings\nExec=studio\n";
    #[test]
    fn respects_visibility_and_sections() {
        for extra in [
            "Hidden=true",
            "NoDisplay=true",
            "OnlyShowIn=GNOME;",
            "NotShowIn=Hyprland;",
        ] {
            assert!(parse("a".into(), "a".into(), &format!("{BASE}{extra}\n"), "en_US").is_none());
        }
        let a = parse(
            "a".into(),
            "a".into(),
            &format!("{BASE}[Desktop Action Evil]\nName=Wrong\n"),
            "en_US",
        )
        .unwrap();
        assert_eq!(a.name, "Studio Settings");
    }
    #[test]
    fn localized_names_and_unicode_search() {
        let a = parse(
            "a".into(),
            "a".into(),
            &format!("{BASE}Name[fr]=Réglages\\sStudio\n"),
            "fr_CA",
        )
        .unwrap();
        assert_eq!(a.name, "Réglages Studio");
        assert_eq!(ranked(&[a], "réglages"), vec![0]);
    }
    #[test]
    fn prioritizes_names_and_matches_all_words() {
        let a = parse("a".into(), "a".into(), BASE, "en_US").unwrap();
        let mut b = a.clone();
        b.name = "Other".into();
        b.description = "studio settings".into();
        assert_eq!(ranked(&[b, a], "studio settings"), vec![1, 0]);
        assert!(ranked(&[], "missing").is_empty());
    }
    #[test]
    fn chromium_uses_browser_icon_and_matches_wayland_app_ids() {
        let entry = parse(
            "chromium.desktop".into(),
            "/usr/share/applications/chromium.desktop".into(),
            "[Desktop Entry]\nType=Application\nName=Chromium\nExec=chromium %U\nStartupWMClass=chromium\n",
            "en_US",
        )
        .unwrap();
        assert_eq!(entry.icon(), "browser");
        assert!(entry.matches_class("chromium"));
        assert!(entry.matches_class("Chromium"));
        assert!(entry.matches_class("chromium-browser"));
        assert!(!entry.matches_class("foot"));
    }
    #[test]
    fn files_uses_files_icon_and_matches_wayland_app_id() {
        let entry = parse(
            "com.crowbaros.Files.desktop".into(),
            "/usr/share/applications/com.crowbaros.Files.desktop".into(),
            "[Desktop Entry]\nType=Application\nName=Files\nExec=crowbaros-files %U\nStartupWMClass=com.crowbaros.Files\n",
            "en_US",
        )
        .unwrap();
        assert_eq!(entry.icon(), "files");
        assert!(entry.matches_class("com.crowbaros.Files"));
        assert!(!entry.matches_class("foot"));
    }
}
