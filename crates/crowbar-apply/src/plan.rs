//! Diff a declared state against a generation root and converge it.

use crate::confine::{entry_in_root, in_root};
use crate::system::PackageManager;
use crate::{Error, Result, err, fstab, write_atomic, write_file};
use crowbar_state::State;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const RUNSVDIR: &str = "/etc/runit/runsvdir/default";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    SetHostname { from: Option<String>, to: String },
    RemovePackages(Vec<String>),
    InstallPackages(Vec<String>),
    WriteFile { path: String, mode: u32, content: String },
    RemoveFile { path: String },
    EnableService(String),
    DisableService(String),
    SetMounts { from: Vec<String>, to: Vec<String> },
}

impl Change {
    /// Changes that can stop the next boot from reaching a usable system.
    /// The generation model makes them recoverable, but the plan calls them out.
    pub fn affects_boot(&self) -> bool {
        matches!(
            self,
            Change::SetMounts { .. } | Change::DisableService(_) | Change::RemovePackages(_)
        )
    }
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Change::SetHostname { from, to } => {
                write!(f, "hostname: {} -> {to}", from.as_deref().unwrap_or("(unset)"))
            }
            Change::RemovePackages(p) => write!(f, "remove packages: {}", p.join(", ")),
            Change::InstallPackages(p) => write!(f, "install packages: {}", p.join(", ")),
            Change::WriteFile { path, mode, .. } => write!(f, "write file {path} (mode {mode:04o})"),
            Change::RemoveFile { path } => write!(f, "remove file {path}"),
            Change::EnableService(s) => write!(f, "enable service {s}"),
            Change::DisableService(s) => write!(f, "disable service {s}"),
            Change::SetMounts { from, to } => {
                write!(f, "managed mounts: {} -> {}", from.len(), to.len())?;
                for line in to.iter().filter(|l| !from.contains(l)) {
                    write!(f, "\n      + {line}")?;
                }
                for line in from.iter().filter(|l| !to.contains(l)) {
                    write!(f, "\n      - {line}")?;
                }
                Ok(())
            }
        }
    }
}

/// What a generation root looks like, limited to what the state can manage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Actual {
    pub hostname: Option<String>,
    pub manual_packages: BTreeSet<String>,
    pub enabled_services: BTreeSet<String>,
    /// For every path the desired or previous state mentions: what is there.
    pub files: BTreeMap<String, Found>,
    pub fstab: String,
}

/// What is at a managed path. The entry itself is examined: a symlink there is
/// never followed, and nothing but a regular file is ever opened, so a link to
/// a device or a FIFO cannot stall or exhaust a plan or a drift check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    Absent,
    File {
        content: String,
        mode: u32,
    },
    /// A symlink, a directory, a device, a FIFO, or a regular file that is not
    /// UTF-8 text or is larger than any managed file. It always differs from a
    /// declared file, and applying replaces it.
    Other,
}

/// Larger than this, a regular file is reported as [`Found::Other`] unread.
const MAX_MANAGED_FILE: u64 = 16 * 1024 * 1024;

fn find(entry: &Path) -> Found {
    let Ok(meta) = std::fs::symlink_metadata(entry) else {
        return Found::Absent;
    };
    if !meta.file_type().is_file() || meta.len() > MAX_MANAGED_FILE {
        return Found::Other;
    }
    match std::fs::read(entry).map(String::from_utf8) {
        Ok(Ok(content)) => Found::File {
            content,
            mode: meta.permissions().mode() & 0o7777,
        },
        _ => Found::Other,
    }
}

/// The text of a file the engine keeps itself: `/etc/fstab` and the recorded
/// State. The rule is the one for `[[file]]` paths: the entry is examined, a
/// final symlink is never followed, and nothing but a regular file is opened.
/// `None` means there is no such file.
pub(crate) fn read_own(root: &Path, path: &str) -> Result<Option<String>> {
    match find(&entry_in_root(root, path)?.host) {
        Found::Absent => Ok(None),
        Found::File { content, .. } => Ok(Some(content)),
        Found::Other => err(format!(
            "{path} in {} is not a regular text file; refusing to read it",
            root.display()
        )),
    }
}

/// The host path of a `[[file]]` entry inside a generation root.
///
/// The State was validated on the path as written. A symlink in the generation
/// can make that path mean somewhere else, so the protected-location check runs
/// again on where it really lands.
fn managed_file(root: &Path, path: &str) -> Result<PathBuf> {
    let entry = entry_in_root(root, path)?;
    if entry.system != path {
        crowbar_state::check_managed_path(&entry.system).map_err(|problem| {
            Error(format!(
                "file {path} is {} through a symlink in the generation: {problem}",
                entry.system
            ))
        })?;
    }
    Ok(entry.host)
}

pub fn read_actual(
    root: &Path,
    desired: &State,
    previous: Option<&State>,
    pkgs: &dyn PackageManager,
) -> Result<Actual> {
    // Anything but a regular file there counts as no hostname, and applying
    // replaces it.
    let hostname = match find(&entry_in_root(root, "/etc/hostname")?.host) {
        Found::File { content, .. } => Some(content.trim().to_string()),
        Found::Absent | Found::Other => None,
    };
    let mut enabled_services = BTreeSet::new();
    let runsvdir = in_root(root, RUNSVDIR)?.host;
    if runsvdir.exists() {
        for entry in std::fs::read_dir(runsvdir)? {
            enabled_services.insert(entry?.file_name().to_string_lossy().into_owned());
        }
    }
    let mut files = BTreeMap::new();
    let paths = desired
        .files
        .iter()
        .map(|f| &f.path)
        .chain(previous.into_iter().flat_map(|p| p.files.iter().map(|f| &f.path)));
    for path in paths {
        // Fail the plan, not the apply, if the path aliases a protected location.
        files.insert(path.clone(), find(&managed_file(root, path)?));
    }
    Ok(Actual {
        hostname,
        manual_packages: pkgs.manual_packages(root)?,
        enabled_services,
        files,
        fstab: read_own(root, "/etc/fstab")?.unwrap_or_default(),
    })
}

/// Compute the ordered list of changes that takes `actual` to `desired`.
///
/// Ownership rule: the state owns the hostname, the exact set of enabled
/// services, its declared files and mounts, and the packages it lists.
/// A package is only removed if a previous state listed it, so the base
/// image's packages are never swept away by accident.
pub fn compute(desired: &State, previous: Option<&State>, actual: &Actual) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    if actual.hostname.as_deref() != Some(desired.system.hostname.as_str()) {
        changes.push(Change::SetHostname {
            from: actual.hostname.clone(),
            to: desired.system.hostname.clone(),
        });
    }

    let wanted = &desired.packages.install;
    if let Some(prev) = previous {
        let remove: Vec<String> = prev
            .packages
            .install
            .difference(wanted)
            .filter(|p| actual.manual_packages.contains(*p))
            .cloned()
            .collect();
        if !remove.is_empty() {
            changes.push(Change::RemovePackages(remove));
        }
    }
    let install: Vec<String> = wanted.difference(&actual.manual_packages).cloned().collect();
    if !install.is_empty() {
        changes.push(Change::InstallPackages(install));
    }

    for file in &desired.files {
        let mode = file.mode_bits().map_err(crate::Error)?;
        let wanted = Found::File {
            content: file.content.clone(),
            mode,
        };
        if actual.files.get(&file.path) != Some(&wanted) {
            changes.push(Change::WriteFile {
                path: file.path.clone(),
                mode,
                content: file.content.clone(),
            });
        }
    }
    if let Some(prev) = previous {
        for old in &prev.files {
            let still_declared = desired.files.iter().any(|f| f.path == old.path);
            if !still_declared && matches!(actual.files.get(&old.path), Some(Found::File { .. } | Found::Other)) {
                changes.push(Change::RemoveFile { path: old.path.clone() });
            }
        }
    }

    for service in desired.services.enabled.difference(&actual.enabled_services) {
        changes.push(Change::EnableService(service.clone()));
    }
    for service in actual.enabled_services.difference(&desired.services.enabled) {
        changes.push(Change::DisableService(service.clone()));
    }

    let from = fstab::managed_lines(&actual.fstab);
    let to = fstab::render_lines(&desired.mounts);
    if from != to {
        changes.push(Change::SetMounts { from, to });
    }
    Ok(changes)
}

/// Apply changes to a generation root that is not the running system.
pub fn apply_changes(root: &Path, desired: &State, changes: &[Change], pkgs: &dyn PackageManager) -> Result<()> {
    for change in changes {
        match change {
            Change::SetHostname { to, .. } => write_atomic(
                &entry_in_root(root, "/etc/hostname")?.host,
                format!("{to}\n").as_bytes(),
            )?,
            Change::RemovePackages(p) => pkgs.remove(root, p)?,
            Change::InstallPackages(p) => pkgs.install(root, p)?,
            Change::WriteFile { path, mode, content } => {
                // The mode is set on the temp file, so the file never exists
                // under its name with a wider mode than declared.
                write_file(&managed_file(root, path)?, content.as_bytes(), Some(*mode))
                    .map_err(|failed| failed.error)?;
            }
            Change::RemoveFile { path } => std::fs::remove_file(managed_file(root, path)?)?,
            Change::EnableService(name) => {
                let definition = format!("/etc/sv/{name}");
                if !in_root(root, &definition)?.host.is_dir() {
                    return err(format!(
                        "cannot enable service {name}: {definition} does not exist in the new generation"
                    ));
                }
                let link = entry_in_root(root, &format!("{RUNSVDIR}/{name}"))?.host;
                std::fs::create_dir_all(link.parent().expect("runsvdir has a parent"))?;
                std::os::unix::fs::symlink(&definition, link)?;
            }
            Change::DisableService(name) => {
                std::fs::remove_file(entry_in_root(root, &format!("{RUNSVDIR}/{name}"))?.host)?
            }
            Change::SetMounts { .. } => {
                for mount in &desired.mounts {
                    std::fs::create_dir_all(in_root(root, &mount.target)?.host)?;
                }
                let current = read_own(root, "/etc/fstab")?.unwrap_or_default();
                write_atomic(
                    &entry_in_root(root, "/etc/fstab")?.host,
                    fstab::splice(&current, &desired.mounts).as_bytes(),
                )?;
            }
        }
    }
    Ok(())
}

/// A short label for the boot menu, e.g. "+2 pkgs, 1 file, mounts".
pub fn summarize(changes: &[Change]) -> String {
    let mut parts = Vec::new();
    for change in changes {
        parts.push(match change {
            Change::SetHostname { to, .. } => format!("hostname {to}"),
            Change::RemovePackages(p) => format!("-{} pkg", p.len()),
            Change::InstallPackages(p) => format!("+{} pkg", p.len()),
            Change::WriteFile { path, .. } => format!("file {path}"),
            Change::RemoveFile { path } => format!("rm {path}"),
            Change::EnableService(s) => format!("+svc {s}"),
            Change::DisableService(s) => format!("-svc {s}"),
            Change::SetMounts { to, .. } => format!("mounts={}", to.len()),
        });
    }
    let mut summary = parts.join(", ");
    if summary.len() > 60 {
        // `truncate` panics inside a character, and a path may hold any.
        let mut end = 57;
        while !summary.is_char_boundary(end) {
            end -= 1;
        }
        summary.truncate(end);
        summary.push_str("...");
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_summary_is_cut_on_a_character_boundary() {
        // Two-byte characters placed so that byte 57 falls inside one.
        let path = format!("/etc/{}", "é".repeat(30));
        let summary = summarize(&[Change::WriteFile {
            path,
            mode: 0o644,
            content: String::new(),
        }]);
        assert!(summary.ends_with("..."), "{summary}");
        assert!(summary.len() <= 60, "{summary}");
        assert!(summary.starts_with("file /etc/éé"), "{summary}");
    }
}
