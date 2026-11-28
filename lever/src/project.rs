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
/// separators or NUL bytes, and not be `.` or `..`.
pub fn validate_name(raw: &str) -> Result<&str, ProjectError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ProjectError::Invalid("Project name must not be empty.".to_string()));
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains('\0') {
        return Err(ProjectError::Invalid(
            "Project name must be a single folder name with no path separators.".to_string(),
        ));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(ProjectError::Invalid("Project name cannot be '.' or '..'.".to_string()));
    }
    Ok(trimmed)
}

/// Expands a leading `~` or `~/` against `home`, then normalizes `.` and `..`
/// lexically. Requires an absolute path after expansion and rejects NUL bytes.
pub fn expand_absolute_path(raw: &str, home: &Path, label: &str) -> Result<PathBuf, ProjectError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ProjectError::Invalid(format!("{label} must not be empty.")));
    }
    if trimmed.contains('\0') {
        return Err(ProjectError::Invalid(format!("{label} contains an invalid character.")));
    }
    let expanded = if trimmed == "~" {
        home.to_path_buf()
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        home.join(rest)
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
    let normalized = lexical_normalize(&expanded);
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
        None => lexical_normalize(&home.join(DEFAULT_PROJECTS_DIR)),
    };
    Ok(lexical_normalize(&parent.join(valid_name)))
}

/// Resolves an explicit target project folder path (used when the person picks
/// an exact folder in the UI or CLI).
pub fn resolve_exact_path(path: &str, home: &Path) -> Result<PathBuf, ProjectError> {
    expand_absolute_path(path, home, "Project folder")
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

/// Normalizes `.` and `..` lexically without touching the filesystem.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Never pop past root.
                if out.components().count() > 1 {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    out
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
        assert!(resolve_project_path("demo", Some("relative/dir"), home).is_err());
        assert!(resolve_project_path("demo", Some("~other/Code"), home).is_err());
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
