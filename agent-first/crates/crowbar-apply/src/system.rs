//! The two system interfaces the engine needs: packages and snapshots.
//! Real implementations shell out to xbps and btrfs; tests use fakes.

use crate::{Error, Result, err};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

pub trait PackageManager {
    /// Names of manually installed packages in the root.
    fn manual_packages(&self, root: &Path) -> Result<BTreeSet<String>>;
    fn install(&self, root: &Path, packages: &[String]) -> Result<()>;
    fn remove(&self, root: &Path, packages: &[String]) -> Result<()>;
}

pub trait Snapshotter {
    fn snapshot(&self, source: &Path, dest: &Path) -> Result<()>;
    fn delete(&self, path: &Path) -> Result<()>;
}

pub fn run(command: &mut Command) -> Result<String> {
    let output = command
        .output()
        .map_err(|e| Error(format!("cannot run {command:?}: {e}")))?;
    if !output.status.success() {
        return err(format!(
            "{command:?} failed ({}):\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `htop-3.3.0_1` -> `htop`. xbps package versions never contain '-'.
pub fn pkgname_from_pkgver(pkgver: &str) -> &str {
    pkgver.rsplit_once('-').map_or(pkgver, |(name, _)| name)
}

/// xbps, run against an alternate root with /dev, /proc and /sys bound in so
/// package scripts behave as they would on the booted system.
pub struct Xbps;

struct ChrootMounts<'a> {
    root: &'a Path,
    mounted: Vec<std::path::PathBuf>,
}

impl<'a> ChrootMounts<'a> {
    fn new(root: &'a Path) -> Result<Self> {
        let mut guard = ChrootMounts {
            root,
            mounted: Vec::new(),
        };
        if root != Path::new("/") {
            for fs in ["dev", "proc", "sys"] {
                let target = root.join(fs);
                std::fs::create_dir_all(&target)?;
                run(Command::new("mount").arg("--rbind").arg(format!("/{fs}")).arg(&target))?;
                run(Command::new("mount").arg("--make-rslave").arg(&target))?;
                guard.mounted.push(target);
            }
        }
        Ok(guard)
    }
}

impl Drop for ChrootMounts<'_> {
    fn drop(&mut self) {
        for target in self.mounted.iter().rev() {
            let _ = run(Command::new("umount").arg("-R").arg(target));
        }
        let _ = self.root;
    }
}

impl PackageManager for Xbps {
    fn manual_packages(&self, root: &Path) -> Result<BTreeSet<String>> {
        let out = run(Command::new("xbps-query").arg("-r").arg(root).arg("-m"))?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| pkgname_from_pkgver(l).to_string())
            .collect())
    }

    fn install(&self, root: &Path, packages: &[String]) -> Result<()> {
        let _mounts = ChrootMounts::new(root)?;
        run(Command::new("xbps-install")
            .arg("-r")
            .arg(root)
            .args(["-S", "-y"])
            .args(packages))
        .map(|_| ())
    }

    fn remove(&self, root: &Path, packages: &[String]) -> Result<()> {
        let _mounts = ChrootMounts::new(root)?;
        run(Command::new("xbps-remove")
            .arg("-r")
            .arg(root)
            .args(["-R", "-y"])
            .args(packages))
        .map(|_| ())
    }
}

/// Writable Btrfs snapshots: each generation is a full, bootable root.
pub struct Btrfs;

impl Snapshotter for Btrfs {
    fn snapshot(&self, source: &Path, dest: &Path) -> Result<()> {
        run(Command::new("btrfs")
            .args(["subvolume", "snapshot"])
            .arg(source)
            .arg(dest))
        .map(|_| ())
    }

    fn delete(&self, path: &Path) -> Result<()> {
        run(Command::new("btrfs").args(["subvolume", "delete"]).arg(path)).map(|_| ())
    }
}

/// Plain recursive copy. Used by tests and when building on non-Btrfs scratch space.
pub struct CopySnapshot;

pub fn copy_tree(source: &Path, dest: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(source)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(source)?, dest)?;
    } else if meta.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &dest.join(entry.file_name()))?;
        }
        std::fs::set_permissions(dest, meta.permissions())?;
    } else {
        std::fs::copy(source, dest)?;
    }
    Ok(())
}

impl Snapshotter for CopySnapshot {
    fn snapshot(&self, source: &Path, dest: &Path) -> Result<()> {
        if dest.exists() {
            return err(format!("{} already exists", dest.display()));
        }
        copy_tree(source, dest)
    }

    fn delete(&self, path: &Path) -> Result<()> {
        Ok(std::fs::remove_dir_all(path)?)
    }
}

#[cfg(test)]
mod tests {
    use super::pkgname_from_pkgver;

    #[test]
    fn strips_xbps_versions() {
        assert_eq!(pkgname_from_pkgver("htop-3.3.0_1"), "htop");
        assert_eq!(pkgname_from_pkgver("grub-x86_64-efi-2.12_2"), "grub-x86_64-efi");
        assert_eq!(pkgname_from_pkgver("noversion"), "noversion");
    }
}
