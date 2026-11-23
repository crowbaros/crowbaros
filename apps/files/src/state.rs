// SPDX-License-Identifier: MIT
//! Navigation state, view modes, pane history, clipboard, and pinned folders.
use crate::fs_model::{self, Entry, SortField, SortOrder};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewMode {
    Columns,
    #[default]
    List,
    Grid,
}

impl ViewMode {
    pub fn icon(self) -> &'static str {
        match self {
            Self::Columns => "view-columns",
            Self::List => "view-list",
            Self::Grid => "view-grid",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Columns => "Columns",
            Self::List => "List",
            Self::Grid => "Grid",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    Directory(PathBuf),
    Recents,
    Trash,
    Search { root: PathBuf, query: String },
}

impl Location {
    pub fn directory(&self) -> Option<&Path> {
        match self {
            Self::Directory(p) => Some(p.as_path()),
            Self::Search { root, .. } => Some(root.as_path()),
            Self::Recents | Self::Trash => None,
        }
    }

    pub fn is_trash(&self) -> bool {
        matches!(self, Self::Trash)
    }

    pub fn title(&self, home: &Path) -> String {
        match self {
            Self::Directory(p) => fs_model::display_path(p, home),
            Self::Recents => "Recents".into(),
            Self::Trash => "Trash".into(),
            Self::Search { query, .. } => format!("Search: {query}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardOp {
    Copy,
    Cut,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileClipboard {
    pub op: ClipboardOp,
    pub path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Pane {
    pub location: Location,
    pub entries: Vec<Entry>,
    pub selected: Option<usize>,
    pub expanded: HashSet<PathBuf>,
    pub back: Vec<Location>,
    pub forward: Vec<Location>,
    pub error: Option<String>,
}

impl Pane {
    pub fn new(initial: PathBuf, show_hidden: bool, sort_field: SortField, sort_order: SortOrder) -> Self {
        let mut pane = Self {
            location: Location::Directory(initial),
            entries: Vec::new(),
            selected: None,
            expanded: HashSet::new(),
            back: Vec::new(),
            forward: Vec::new(),
            error: None,
        };
        pane.reload(show_hidden, sort_field, sort_order, None);
        pane
    }

    pub fn selected_entry(&self) -> Option<&Entry> {
        self.selected.and_then(|i| self.entries.get(i))
    }

    pub fn navigate(
        &mut self,
        target: Location,
        show_hidden: bool,
        sort_field: SortField,
        sort_order: SortOrder,
        select_path: Option<&Path>,
    ) {
        if self.location != target {
            self.back.push(self.location.clone());
            if self.back.len() > 64 {
                self.back.remove(0);
            }
            self.forward.clear();
            self.location = target;
            self.expanded.clear();
        }
        self.reload(show_hidden, sort_field, sort_order, select_path);
    }

    pub fn go_back(&mut self, show_hidden: bool, sort_field: SortField, sort_order: SortOrder) -> bool {
        let Some(prev) = self.back.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.location, prev);
        self.forward.push(current);
        self.expanded.clear();
        self.reload(show_hidden, sort_field, sort_order, None);
        true
    }

    pub fn go_forward(&mut self, show_hidden: bool, sort_field: SortField, sort_order: SortOrder) -> bool {
        let Some(next) = self.forward.pop() else {
            return false;
        };
        let current = std::mem::replace(&mut self.location, next);
        self.back.push(current);
        self.expanded.clear();
        self.reload(show_hidden, sort_field, sort_order, None);
        true
    }

    pub fn go_up(&mut self, show_hidden: bool, sort_field: SortField, sort_order: SortOrder) -> bool {
        let current_dir = match &self.location {
            Location::Directory(p) => p.clone(),
            Location::Search { root, .. } => {
                let r = root.clone();
                self.navigate(Location::Directory(r), show_hidden, sort_field, sort_order, None);
                return true;
            }
            Location::Recents | Location::Trash => return false,
        };
        let Some(parent) = current_dir.parent().map(Path::to_path_buf) else {
            return false;
        };
        if parent == current_dir {
            return false;
        }
        self.navigate(
            Location::Directory(parent),
            show_hidden,
            sort_field,
            sort_order,
            Some(&current_dir),
        );
        true
    }

    pub fn reload(
        &mut self,
        show_hidden: bool,
        sort_field: SortField,
        sort_order: SortOrder,
        prefer_select: Option<&Path>,
    ) {
        let prev_path = prefer_select
            .map(Path::to_path_buf)
            .or_else(|| self.selected_entry().map(|e| e.path.clone()));
        self.error = None;
        match &self.location {
            Location::Directory(dir) => match fs_model::read_directory(dir, show_hidden, sort_field, sort_order) {
                Ok(entries) => self.entries = entries,
                Err(e) => {
                    self.entries.clear();
                    self.error = Some(e);
                }
            },
            Location::Recents => {
                self.entries = fs_model::read_recents(&fs_model::home_dir(), 100);
            }
            Location::Trash => {
                self.entries = fs_model::read_trash();
            }
            Location::Search { root, query } => {
                self.entries = fs_model::search_below(root, query, show_hidden, 200);
            }
        }
        self.selected = if self.entries.is_empty() {
            None
        } else if let Some(prev) = prev_path {
            self.entries.iter().position(|e| e.path == prev).or(Some(0))
        } else {
            Some(0)
        };
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            self.selected = None;
            return;
        }
        let len = self.entries.len() as isize;
        let current = self.selected.map(|i| i as isize).unwrap_or(0);
        let next = (current + delta).clamp(0, len - 1) as usize;
        self.selected = Some(next);
    }

    pub fn select_first(&mut self) {
        self.selected = (!self.entries.is_empty()).then_some(0);
    }

    pub fn select_last(&mut self) {
        self.selected = self.entries.len().checked_sub(1);
    }
}

/// Persistent user state for Files (pinned folders, view mode, inspector visibility).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedState {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub view_mode: ViewMode,
    #[serde(default = "default_true")]
    pub show_inspector: bool,
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default)]
    pub pinned: Vec<PathBuf>,
}

fn default_schema() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

impl SavedState {
    pub fn config_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("crowbaros/files.json"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::config_path() else {
            return Self {
                schema_version: 1,
                show_inspector: true,
                ..Default::default()
            };
        };
        let Ok(bytes) = fs::read(path) else {
            return Self {
                schema_version: 1,
                show_inspector: true,
                ..Default::default()
            };
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|_| Self {
            schema_version: 1,
            show_inspector: true,
            ..Default::default()
        })
    }

    pub fn save(&self) {
        let Some(path) = Self::config_path() else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        let _ = fs::create_dir_all(parent);
        let Ok(json) = serde_json::to_vec_pretty(self) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if let Ok(mut f) = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
        {
            use std::io::Write;
            if f.write_all(&json).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
        }
    }

    pub fn toggle_pin(&mut self, dir: &Path) -> bool {
        if let Some(pos) = self.pinned.iter().position(|p| p == dir) {
            self.pinned.remove(pos);
            false
        } else {
            self.pinned.push(dir.to_path_buf());
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("crowbaros-files-state-{}-{id}-{label}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn pane_navigation_back_forward_and_up() {
        let root = temp_dir("nav");
        let child = root.join("sub");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("a.txt"), "a").unwrap();

        let mut pane = Pane::new(root.clone(), false, SortField::Name, SortOrder::Asc);
        assert_eq!(pane.entries.len(), 1);
        assert_eq!(pane.selected, Some(0));

        pane.navigate(
            Location::Directory(child.clone()),
            false,
            SortField::Name,
            SortOrder::Asc,
            None,
        );
        assert_eq!(pane.location, Location::Directory(child.clone()));
        assert_eq!(pane.entries.len(), 1);
        assert_eq!(pane.entries[0].name, "a.txt");

        assert!(pane.go_back(false, SortField::Name, SortOrder::Asc));
        assert_eq!(pane.location, Location::Directory(root.clone()));

        assert!(pane.go_forward(false, SortField::Name, SortOrder::Asc));
        assert_eq!(pane.location, Location::Directory(child.clone()));

        assert!(pane.go_up(false, SortField::Name, SortOrder::Asc));
        assert_eq!(pane.location, Location::Directory(root.clone()));
        // Going up selects the child folder we just came from!
        assert_eq!(pane.selected_entry().map(|e| e.path.as_path()), Some(child.as_path()));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn view_mode_labels_and_pins_toggle() {
        assert_eq!(ViewMode::List.label(), "List");
        assert_eq!(ViewMode::Columns.label(), "Columns");
        assert_eq!(ViewMode::Grid.label(), "Grid");

        let mut saved = SavedState::default();
        let p = PathBuf::from("/tmp/demo");
        assert!(saved.toggle_pin(&p));
        assert_eq!(saved.pinned, vec![p.clone()]);
        assert!(!saved.toggle_pin(&p));
        assert!(saved.pinned.is_empty());
    }
}
