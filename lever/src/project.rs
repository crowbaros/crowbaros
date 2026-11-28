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

fn expand_tilde_candidate(candidate: &str, home: &Path, label: &str) -> Result<PathBuf, ProjectError> {
    if candidate.is_empty() {
        return Err(ProjectError::Invalid(format!("{label} must not be empty.")));
    }
    if candidate.chars().any(char::is_control) {
        return Err(ProjectError::Invalid(format!("{label} contains an invalid character.")));
    }
    let expanded = if candidate == "~" {
        home.to_path_buf()
    } else if let Some(rest) = candidate.strip_prefix("~/") {
        // Strip repeated slashes after `~/` so `~//Code` stays under `home`
        // instead of `PathBuf::join` replacing `home` with `/Code`.
        let relative = rest.trim_start_matches('/');
        if relative.is_empty() {
            home.to_path_buf()
        } else {
            home.join(relative)
        }
    } else if candidate.starts_with('~') {
        return Err(ProjectError::Invalid(format!(
            "{label} must be an absolute path or start with ~/."
        )));
    } else {
        PathBuf::from(candidate)
    };
    if !expanded.is_absolute() {
        return Err(ProjectError::Invalid(format!(
            "{label} must be an absolute path or start with ~/."
        )));
    }
    Ok(expanded)
}

/// Expands a leading `~` or `~/` against `home`, then normalizes `.` and `..`
/// (resolving existing symlinks before traversing `..` so the result matches
/// real filesystem traversal). Requires an absolute path after expansion and
/// rejects control characters. Always preserves trailing spaces in the path
/// itself so a directory like `notes ` never silently redirects to `notes`.
pub fn expand_absolute_path(raw: &str, home: &Path, label: &str) -> Result<PathBuf, ProjectError> {
    if raw.trim().is_empty() {
        return Err(ProjectError::Invalid(format!("{label} must not be empty.")));
    }
    let candidate = if raw.trim() == "~" { "~" } else { raw.trim_start() };
    let expanded = expand_tilde_candidate(candidate, home, label)?;
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
    let parent = match place.filter(|s| !s.trim().is_empty()) {
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
    let start = match raw.filter(|s| !s.trim().is_empty()) {
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
            // Skip names or paths that cannot round-trip cleanly through the
            // UTF-8 JSON protocol (non-UTF-8 bytes or control characters).
            let raw_name = entry.file_name();
            let name = raw_name.to_str()?;
            if name.starts_with('.') || name.chars().any(char::is_control) {
                return None;
            }
            let path = entry.path();
            path.to_str()?;
            path.is_dir().then_some((name.to_string(), path))
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
                        out = std::fs::canonicalize(&out).map_err(|e| {
                            ProjectError::Filesystem(format!(
                                "Cannot resolve {} while traversing {label}: {e}",
                                out.display()
                            ))
                        })?;
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        if std::fs::symlink_metadata(&out).is_ok() {
                            return Err(ProjectError::Filesystem(format!(
                                "{label} traverses {}, which is a broken link.",
                                out.display()
                            )));
                        }
                    }
                    Err(err) => {
                        return Err(ProjectError::Filesystem(format!(
                            "Cannot inspect {} while resolving {label}: {err}",
                            out.display()
                        )));
                    }
                }
                // Never pop past root.
                if out.components().count() > 1 {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    let Some(s) = out.to_str() else {
        return Err(ProjectError::Invalid(format!(
            "{label} resolves to a path that is not valid UTF-8."
        )));
    };
    if s.chars().any(char::is_control) {
        return Err(ProjectError::Invalid(format!(
            "{label} resolves to a path containing a control character."
        )));
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

        // Traversing `..` through a regular file, a non-directory ancestor, or
        // a symlink loop fails cleanly rather than popping past the error.
        let blocker = fake_home.join("file.txt");
        std::fs::write(&blocker, "x").unwrap();
        assert!(resolve_project_path("demo", Some("~/file.txt/.."), &fake_home).is_err());
        assert!(resolve_project_path("demo", Some("~/file.txt/sub/.."), &fake_home).is_err());

        std::os::unix::fs::symlink(fake_home.join("loop_b"), fake_home.join("loop_a")).unwrap();
        std::os::unix::fs::symlink(fake_home.join("loop_a"), fake_home.join("loop_b")).unwrap();
        assert!(resolve_project_path("demo", Some("~/loop_a/.."), &fake_home).is_err());

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

        // Sibling directories differing only by a trailing space remain distinct,
        // even if the trailing-space directory is missing or renamed before confirm.
        let plain = base.join("notes");
        let trailing = base.join("notes ");
        std::fs::create_dir_all(plain.join("plain-child")).unwrap();
        std::fs::create_dir_all(trailing.join("spaced-child")).unwrap();
        let listed = list_subdirectories(Some(&trailing.to_string_lossy()), &base).unwrap();
        assert_eq!(listed.path, trailing);
        let child_names: Vec<&str> = listed.entries.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(child_names, vec!["spaced-child"]);
        assert_eq!(
            resolve_exact_path(&trailing.to_string_lossy(), &base).unwrap(),
            trailing
        );
        assert_eq!(
            resolve_project_path("my-app", Some(&trailing.to_string_lossy()), &base).unwrap(),
            trailing.join("my-app")
        );
        let missing_trailing = base.join("removed-notes ");
        assert_eq!(
            resolve_exact_path(&missing_trailing.to_string_lossy(), &base).unwrap(),
            missing_trailing
        );
        assert_eq!(
            resolve_project_path("my-app", Some(&missing_trailing.to_string_lossy()), &base).unwrap(),
            missing_trailing.join("my-app")
        );

        // Control-character directory names are omitted from listings, and
        // symlink traversal into a control-character path via `..` is rejected
        // by `normalize_path`.
        let ctrl = base.join("bad\tdir");
        let ctrl_sub = ctrl.join("sub");
        std::fs::create_dir_all(&ctrl_sub).unwrap();
        let listed_ctrl = list_subdirectories(Some(&base.to_string_lossy()), &base).unwrap();
        assert!(
            !listed_ctrl.entries.iter().any(|(n, _)| n.contains('\t')),
            "control-char dir should be excluded: {:?}",
            listed_ctrl.entries
        );
        #[cfg(unix)]
        {
            let ctrl_sym = base.join("ctrl-link");
            std::os::unix::fs::symlink(&ctrl_sub, &ctrl_sym).unwrap();
            let via_ctrl = format!("{}/..", ctrl_sym.to_string_lossy());
            assert!(matches!(
                resolve_exact_path(&via_ctrl, &base),
                Err(ProjectError::Invalid(_))
            ));
            assert!(matches!(
                resolve_project_path("app", Some(&via_ctrl), &base),
                Err(ProjectError::Invalid(_))
            ));
            assert!(matches!(
                list_subdirectories(Some(&via_ctrl), &base),
                Err(ProjectError::Invalid(_))
            ));
        }

        // Non-UTF-8 directory names are omitted from listings, and symlinks
        // that traverse into a non-UTF-8 path via `..` are rejected before any
        // folder is created or returned to the picker.
        #[cfg(unix)]
        {
            use std::ffi::OsStr;
            use std::os::unix::ffi::OsStrExt;
            let bad = base.join(OsStr::from_bytes(b"bad-\xff-dir"));
            let bad_sub = bad.join("sub");
            if std::fs::create_dir_all(&bad_sub).is_ok() {
                let listed = list_subdirectories(Some(&base.to_string_lossy()), &base).unwrap();
                assert!(
                    !listed.entries.iter().any(|(n, _)| n.starts_with("bad-")),
                    "non-UTF-8 dir should be excluded: {:?}",
                    listed.entries
                );
                let sym = base.join("utf8-link");
                std::os::unix::fs::symlink(&bad_sub, &sym).unwrap();
                let via_link = format!("{}/..", sym.to_string_lossy());
                assert!(matches!(
                    resolve_exact_path(&via_link, &base),
                    Err(ProjectError::Invalid(_))
                ));
                assert!(matches!(
                    resolve_project_path("app", Some(&via_link), &base),
                    Err(ProjectError::Invalid(_))
                ));
                assert!(matches!(
                    list_subdirectories(Some(&via_link), &base),
                    Err(ProjectError::Invalid(_))
                ));
            }
        }

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
