//! Harness-agnostic project folder resolution and creation.
//!
//! A project is a thread (or several) tied to the folder where their work is
//! kept (`GLOSSARY.md`). Making a project resolves the target folder (defaulting
//! to `~/Projects/<name>` unless a place or exact path is given), creates it if
//! missing, and leaves any existing folder completely untouched.

use std::path::{Component, Path, PathBuf};

/// The default parent directory under the person's home for new projects.
pub const DEFAULT_PROJECTS_DIR: &str = "Projects";

/// Validation or filesystem failure while resolving or preparing a project
/// folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectError {
    Invalid(String),
    Filesystem(String),
}

impl ProjectError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid-request",
            Self::Filesystem(_) => "project-error",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Invalid(msg) | Self::Filesystem(msg) => msg,
        }
    }
}

/// Validates a project name: must be non-empty after trimming, contain no path
/// separators or control characters, and not be `.` or `..`.
pub fn validate_name(raw: &str) -> Result<&str, ProjectError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ProjectError::Invalid("Project name must not be empty.".to_string()));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.chars().any(char::is_control) {
        return Err(ProjectError::Invalid(
            "Project name must be a single folder name with no path separators or control characters.".to_string(),
        ));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(ProjectError::Invalid("Project name cannot be '.' or '..'.".to_string()));
    }
    Ok(trimmed)
}

/// Expands a leading `~` or `~/` against `home`, then normalizes `.` and `..`
/// (resolving existing symlinks before traversing `..` so the result matches
/// real filesystem traversal). Requires an absolute path after expansion and
/// rejects control characters.
pub fn expand_absolute_path(raw: &str, home: &Path, label: &str) -> Result<PathBuf, ProjectError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ProjectError::Invalid(format!("{label} must not be empty.")));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(ProjectError::Invalid(format!("{label} contains an invalid character.")));
    }
    let expanded = if trimmed == "~" {
        home.to_path_buf()
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        // Strip repeated slashes after `~/` so `~//Code` stays under `home`
        // instead of `PathBuf::join` replacing `home` with `/Code`.
        let relative = rest.trim_start_matches('/');
        if relative.is_empty() {
            home.to_path_buf()
        } else {
            home.join(relative)
        }
    } else if trimmed.starts_with('~') {
        return Err(ProjectError::Invalid(format!(
            "{label} must be an absolute path or start with ~/."
        )));
    } else {
        PathBuf::from(trimmed)
    };
    if !expanded.is_absolute() {
        return Err(ProjectError::Invalid(format!(
            "{label} must be an absolute path or start with ~/."
        )));
    }
    let normalized = normalize_path(&expanded, label)?;
    if !normalized.is_absolute() {
        return Err(ProjectError::Invalid(format!("{label} must be an absolute path.")));
    }
    Ok(normalized)
}

/// Resolves a project folder from `name` and an optional parent `place`
/// (defaulting to `<home>/Projects/<name>`).
pub fn resolve_project_path(name: &str, place: Option<&str>, home: &Path) -> Result<PathBuf, ProjectError> {
    let valid_name = validate_name(name)?;
    let parent = match place.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => expand_absolute_path(raw, home, "Project place")?,
        None => normalize_path(&home.join(DEFAULT_PROJECTS_DIR), "Project place")?,
    };
    normalize_path(&parent.join(valid_name), "Project folder")
}

/// Resolves an explicit target project folder path (used when the person picks
/// an exact folder in the UI or CLI).
pub fn resolve_exact_path(path: &str, home: &Path) -> Result<PathBuf, ProjectError> {
    expand_absolute_path(path, home, "Project folder")
}

/// Bounded directory listing returned for the drawer's inline folder picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirListing {
    pub path: PathBuf,
    pub parent: Option<PathBuf>,
    pub entries: Vec<(String, PathBuf)>,
}

/// Lists readable, non-hidden subdirectories of `raw` (or `home` when omitted),
/// walking up to the nearest existing ancestor directory when `raw` does not
/// exist yet. Capped at 64 entries so a huge directory cannot flood the UI.
pub fn list_subdirectories(raw: Option<&str>, home: &Path) -> Result<DirListing, ProjectError> {
    let start = match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => expand_absolute_path(p, home, "Folder")?,
        None => normalize_path(home, "Folder")?,
    };
    let mut cur = start;
    while !cur.is_dir() {
        let Some(parent) = cur.parent() else {
            return Err(ProjectError::Filesystem(format!(
                "Cannot browse {}: no readable parent directory exists.",
                cur.display()
            )));
        };
        cur = parent.to_path_buf();
    }
    let read =
        std::fs::read_dir(&cur).map_err(|e| ProjectError::Filesystem(format!("Cannot read {}: {e}", cur.display())))?;
    let mut entries: Vec<(String, PathBuf)> = read
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                return None;
            }
            let path = entry.path();
            path.is_dir().then_some((name, path))
        })
        .collect();
    entries.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()).then_with(|| a.0.cmp(&b.0)));
    entries.truncate(64);
    let parent = cur.parent().filter(|p| *p != cur.as_path()).map(Path::to_path_buf);
    Ok(DirListing {
        path: cur,
        parent,
        entries,
    })
}

/// Ensures `path` exists as a directory.
///
/// - If `path` already exists and is a directory (or symlink to a directory),
///   returns `Ok(false)` without creating, modifying, or removing anything in
///   it.
/// - If `path` exists and is not a directory, returns `Err(Filesystem)`.
/// - If `path` does not exist, creates it (and any missing parents) and
///   returns `Ok(true)`.
pub fn ensure_project_dir(path: &Path) -> Result<bool, ProjectError> {
    match std::fs::metadata(path) {
        Ok(meta) => {
            if meta.is_dir() {
                Ok(false)
            } else {
                Err(ProjectError::Filesystem(format!(
                    "Cannot use {} as a project folder because a file already exists there.",
                    path.display()
                )))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            // Also check symlink_metadata in case `path` is a dangling symlink.
            if std::fs::symlink_metadata(path).is_ok() {
                return Err(ProjectError::Filesystem(format!(
                    "Cannot use {} as a project folder because a broken link exists there.",
                    path.display()
                )));
            }
            std::fs::create_dir_all(path).map_err(|e| {
                ProjectError::Filesystem(format!("Could not create project folder {}: {e}", path.display()))
            })?;
            Ok(true)
        }
        Err(err) => Err(ProjectError::Filesystem(format!(
            "Could not inspect project folder {}: {err}",
            path.display()
        ))),
    }
}

/// Normalizes `.` and `..` while honoring symlinks on disk before popping `..`
/// so the resolved path matches real filesystem traversal. Paths without `..`
/// preserve symlink names so a person's `~/Code` link stays displayed under
/// `~/Code`.
fn normalize_path(path: &Path, label: &str) -> Result<PathBuf, ProjectError> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Before traversing `..`, resolve any existing symlink at `out`
                // and refuse traversal through an existing non-directory.
                match std::fs::metadata(&out) {
                    Ok(meta) if !meta.is_dir() => {
                        return Err(ProjectError::Filesystem(format!(
                            "{label} traverses {}, which is not a directory.",
                            out.display()
                        )));
                    }
                    Ok(_) => {
                        if let Ok(canon) = std::fs::canonicalize(&out) {
                            out = canon;
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        if std::fs::symlink_metadata(&out).is_ok() {
                            return Err(ProjectError::Filesystem(format!(
                                "{label} traverses {}, which is a broken link.",
                                out.display()
                            )));
                        }
                    }
                    Err(_) => {}
                }
                // Never pop past root.
                if out.components().count() > 1 {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn resolves_default_under_home_projects() {
        let home = Path::new("/home/person");
        let path = resolve_project_path("demo-app", None, home).unwrap();
        assert_eq!(path, PathBuf::from("/home/person/Projects/demo-app"));
    }

    #[test]
    fn resolves_named_place_with_tilde() {
        let home = Path::new("/home/person");
        let path = resolve_project_path("notes", Some("~/Code"), home).unwrap();
        assert_eq!(path, PathBuf::from("/home/person/Code/notes"));

        let at_home = resolve_project_path("scratch", Some("~"), home).unwrap();
        assert_eq!(at_home, PathBuf::from("/home/person/scratch"));
    }

    #[test]
    fn resolves_exact_path_with_tilde() {
        let home = Path::new("/home/person");
        let path = resolve_exact_path("~/Work/alpha", home).unwrap();
        assert_eq!(path, PathBuf::from("/home/person/Work/alpha"));
    }

    #[test]
    fn rejects_invalid_names_and_relative_places() {
        let home = Path::new("/home/person");
        assert!(resolve_project_path("", None, home).is_err());
        assert!(resolve_project_path("   ", None, home).is_err());
        assert!(resolve_project_path("a/b", None, home).is_err());
        assert!(resolve_project_path("..", None, home).is_err());
        assert!(resolve_project_path(".", None, home).is_err());
        assert!(resolve_project_path("bad\nname", None, home).is_err());
        assert!(resolve_project_path("bad\tname", None, home).is_err());
        assert!(resolve_project_path("demo", Some("relative/dir"), home).is_err());
        assert!(resolve_project_path("demo", Some("~other/Code"), home).is_err());
        assert!(resolve_project_path("demo", Some("~/Code\n/sub"), home).is_err());
    }

    #[test]
    fn handles_repeated_tilde_slashes_and_symlinks_with_parent_dir() {
        let home = Path::new("/home/person");
        assert_eq!(
            resolve_project_path("demo", Some("~//Code"), home).unwrap(),
            PathBuf::from("/home/person/Code/demo")
        );
        assert_eq!(
            resolve_exact_path("~///Work/alpha", home).unwrap(),
            PathBuf::from("/home/person/Work/alpha")
        );

        let base = std::env::temp_dir().join(format!("crowbar-proj-link-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let fake_home = base.join("home");
        let real_parent = base.join("external/nested");
        std::fs::create_dir_all(&fake_home).unwrap();
        std::fs::create_dir_all(&real_parent).unwrap();
        std::os::unix::fs::symlink(&real_parent, fake_home.join("link")).unwrap();

        // Without `..`, the symlink name is kept under home.
        assert_eq!(
            resolve_project_path("demo", Some("~/link"), &fake_home).unwrap(),
            fake_home.join("link/demo")
        );
        // With `..`, symlink target is resolved first so traversal matches the OS.
        let expected = std::fs::canonicalize(base.join("external")).unwrap().join("demo");
        assert_eq!(
            resolve_project_path("demo", Some("~/link/.."), &fake_home).unwrap(),
            expected
        );

        // Traversing `..` through a regular file fails cleanly.
        let blocker = fake_home.join("file.txt");
        std::fs::write(&blocker, "x").unwrap();
        assert!(resolve_project_path("demo", Some("~/file.txt/.."), &fake_home).is_err());

        // Directory listing walks up from non-existent leaf, skips hidden dirs, and sorts.
        std::fs::create_dir_all(fake_home.join("Projects")).unwrap();
        std::fs::create_dir_all(fake_home.join("Code")).unwrap();
        std::fs::create_dir_all(fake_home.join(".secret")).unwrap();
        let listing = list_subdirectories(Some("~/Projects/not-created-yet"), &fake_home).unwrap();
        assert_eq!(listing.path, fake_home.join("Projects"));
        assert_eq!(listing.parent.as_deref(), Some(fake_home.as_path()));
        let home_listing = list_subdirectories(None, &fake_home).unwrap();
        let names: Vec<&str> = home_listing.entries.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"Code"));
        assert!(names.contains(&"Projects"));
        assert!(names.contains(&"link"));
        assert!(!names.contains(&".secret"));
        assert!(!names.contains(&"file.txt"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ensure_project_dir_creates_new_and_preserves_existing() {
        let base = std::env::temp_dir().join(format!("crowbar-proj-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        let fresh = base.join("Projects/new-one");
        assert!(ensure_project_dir(&fresh).unwrap());
        assert!(fresh.is_dir());

        // Put a marker file inside and re-run ensure_project_dir: nothing changes.
        let marker = fresh.join("keep.txt");
        std::fs::write(&marker, "unchanged").unwrap();
        assert!(!ensure_project_dir(&fresh).unwrap());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "unchanged");

        // File blocking the target fails cleanly.
        let blocker = base.join("existing-file");
        std::fs::write(&blocker, "hello").unwrap();
        assert!(ensure_project_dir(&blocker).is_err());
        assert!(ensure_project_dir(&blocker.join("child")).is_err());

        // Read-only parent fails cleanly (skip when running as root).
        if unsafe { libc::geteuid() } != 0 {
            let ro = base.join("readonly");
            std::fs::create_dir_all(&ro).unwrap();
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
            assert!(ensure_project_dir(&ro.join("cant-make")).is_err());
            std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let _ = std::fs::remove_dir_all(&base);
    }
}
