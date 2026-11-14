//! Diff a declared state against a generation root and converge it.

use crate::system::PackageManager;
use crate::{Result, err, fstab, under, write_atomic};
use crowbar_state::State;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

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
    /// For every path the desired or previous state mentions: content and mode, if present.
    pub files: BTreeMap<String, Option<(String, u32)>>,
    pub fstab: String,
}

pub fn read_actual(
    root: &Path,
    desired: &State,
    previous: Option<&State>,
    pkgs: &dyn PackageManager,
) -> Result<Actual> {
    let hostname = std::fs::read_to_string(under(root, "/etc/hostname"))
        .ok()
        .map(|s| s.trim().to_string());
    let mut enabled_services = BTreeSet::new();
    let runsvdir = under(root, RUNSVDIR);
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
        let full = under(root, path);
        let found = match std::fs::read_to_string(&full) {
            Ok(content) => Some((content, std::fs::metadata(&full)?.permissions().mode() & 0o7777)),
            Err(_) => None,
        };
        files.insert(path.clone(), found);
    }
    Ok(Actual {
        hostname,
        manual_packages: pkgs.manual_packages(root)?,
        enabled_services,
        files,
        fstab: std::fs::read_to_string(under(root, "/etc/fstab")).unwrap_or_default(),
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
        let current = actual.files.get(&file.path).cloned().flatten();
        if current != Some((file.content.clone(), mode)) {
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
            if !still_declared && matches!(actual.files.get(&old.path), Some(Some(_))) {
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
            Change::SetHostname { to, .. } => {
                write_atomic(&under(root, "/etc/hostname"), format!("{to}\n").as_bytes())?
            }
            Change::RemovePackages(p) => pkgs.remove(root, p)?,
            Change::InstallPackages(p) => pkgs.install(root, p)?,
            Change::WriteFile { path, mode, content } => {
                let full = under(root, path);
                write_atomic(&full, content.as_bytes())?;
                std::fs::set_permissions(&full, std::fs::Permissions::from_mode(*mode))?;
            }
            Change::RemoveFile { path } => std::fs::remove_file(under(root, path))?,
            Change::EnableService(name) => {
                let definition = format!("/etc/sv/{name}");
                if !under(root, &definition).is_dir() {
                    return err(format!(
                        "cannot enable service {name}: {definition} does not exist in the new generation"
                    ));
                }
                let link = under(root, &format!("{RUNSVDIR}/{name}"));
                std::fs::create_dir_all(link.parent().expect("runsvdir has a parent"))?;
                std::os::unix::fs::symlink(&definition, link)?;
            }
            Change::DisableService(name) => std::fs::remove_file(under(root, &format!("{RUNSVDIR}/{name}")))?,
            Change::SetMounts { .. } => {
                for mount in &desired.mounts {
                    std::fs::create_dir_all(under(root, &mount.target))?;
                }
                let current = std::fs::read_to_string(under(root, "/etc/fstab")).unwrap_or_default();
                write_atomic(
                    &under(root, "/etc/fstab"),
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
        summary.truncate(57);
        summary.push_str("...");
    }
    summary
}
