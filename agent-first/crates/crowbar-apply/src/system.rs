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
    /// Flush everything written under `path` to disk. The engine calls this
    /// before it publishes a generation, so the boot menu never names a root
    /// whose files are still only in memory.
    fn sync(&self, path: &Path) -> Result<()>;
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

type Exec = fn(&mut Command) -> Result<String>;

struct ChrootMounts {
    mounted: Vec<std::path::PathBuf>,
    exec: Exec,
}

impl ChrootMounts {
    fn new(root: &Path) -> Result<Self> {
        Self::with(root, run)
    }

    fn with(root: &Path, exec: Exec) -> Result<Self> {
        let mut guard = ChrootMounts {
            mounted: Vec::new(),
            exec,
        };
        if root != Path::new("/") {
            for fs in ["dev", "proc", "sys"] {
                if let Err(e) = guard.bind(root, fs) {
                    return Err(and_teardown(e, guard.release()));
                }
            }
        }
        Ok(guard)
    }

    fn bind(&mut self, root: &Path, fs: &str) -> Result<()> {
        let target = chroot_mount_point(root, fs)?;
        (self.exec)(Command::new("mount").arg("--rbind").arg(format!("/{fs}")).arg(&target))?;
        if let Err(e) = (self.exec)(Command::new("mount").arg("--make-rslave").arg(&target)) {
            // Until it is a slave, the bind still shares mounts with the host:
            // a recursive unmount here would also unmount the host's own
            // submounts. Leave it mounted and say so.
            return err(format!(
                "{e}\n{} is left mounted: it still shares mounts with the host, so it was not unmounted",
                target.display()
            ));
        }
        // Recorded once it is safe to unmount.
        self.mounted.push(target);
        Ok(())
    }

    /// Unmount everything, newest first, and say what is still mounted.
    fn release(mut self) -> Result<()> {
        self.unmount_all()
    }

    fn unmount_all(&mut self) -> Result<()> {
        let mut problems = Vec::new();
        while let Some(target) = self.mounted.pop() {
            if let Err(e) = (self.exec)(Command::new("umount").arg("-R").arg(&target)) {
                problems.push(format!("{} is still mounted: {e}", target.display()));
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            err(problems.join("\n"))
        }
    }
}

/// A failure inside the mounts, with any unmount problem added to it.
fn and_teardown(cause: Error, unmounted: Result<()>) -> Error {
    match unmounted {
        Ok(()) => cause,
        Err(still) => Error(format!("{cause}\nalso: {still}")),
    }
}

/// Run `command` with the host's /dev, /proc and /sys bound into `root`.
fn in_chroot(root: &Path, command: &mut Command) -> Result<()> {
    let mounts = ChrootMounts::new(root)?;
    let ran = run(command);
    let unmounted = mounts.release();
    match ran {
        Ok(_) => unmounted,
        Err(e) => Err(and_teardown(e, unmounted)),
    }
}

/// Where the host's `/dev`, `/proc`, or `/sys` is bound inside `root`.
///
/// `mount` and `umount` resolve their target against the host root. If the
/// entry inside the generation were a symlink, the bind mount would land on
/// whatever it points at in the running system. So the entry must be a real
/// directory; it is created if it is missing.
fn chroot_mount_point(root: &Path, name: &str) -> Result<std::path::PathBuf> {
    let target = root.join(name);
    match std::fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_dir() => Ok(target),
        Ok(_) => err(format!(
            "{} is not a directory; refusing to bind the host's /{name} onto it",
            target.display()
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(&target)?;
            Ok(target)
        }
        Err(e) => Err(e.into()),
    }
}

impl Drop for ChrootMounts {
    fn drop(&mut self) {
        // Reached only on a panic; `release` reports through its result.
        if let Err(e) = self.unmount_all() {
            eprintln!("crowbar: {e}");
        }
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
        in_chroot(
            root,
            Command::new("xbps-install")
                .arg("-r")
                .arg(root)
                .args(["-S", "-y"])
                .args(packages),
        )
    }

    fn remove(&self, root: &Path, packages: &[String]) -> Result<()> {
        in_chroot(
            root,
            Command::new("xbps-remove")
                .arg("-r")
                .arg(root)
                .args(["-R", "-y"])
                .args(packages),
        )
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

    fn sync(&self, path: &Path) -> Result<()> {
        // Commits the filesystem's current transaction, which covers every
        // file in the subvolume, not only the ones the engine wrote itself.
        run(Command::new("btrfs").args(["filesystem", "sync"]).arg(path)).map(|_| ())
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

/// Flush every regular file and directory under `path`, one by one.
pub fn sync_tree(path: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            sync_tree(&entry?.path())?;
        }
    }
    if meta.is_dir() || meta.file_type().is_file() {
        std::fs::File::open(path)?.sync_all()?;
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

    fn sync(&self, path: &Path) -> Result<()> {
        sync_tree(path)
    }
}

#[cfg(test)]
mod tests {
    use super::{ChrootMounts, chroot_mount_point, pkgname_from_pkgver};
    use crate::testutil::scratch;
    use crate::{Result, err};
    use std::cell::RefCell;
    use std::process::Command;

    thread_local! {
        static CALLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    fn record(command: &Command) -> String {
        let mut line = command.get_program().to_string_lossy().into_owned();
        for arg in command.get_args() {
            line.push(' ');
            line.push_str(&arg.to_string_lossy());
        }
        CALLS.with(|calls| calls.borrow_mut().push(line.clone()));
        line
    }

    fn rslave_fails(command: &mut Command) -> Result<String> {
        if record(command).contains("--make-rslave") {
            return err("make-rslave refused");
        }
        Ok(String::new())
    }

    fn proc_bind_and_umount_fail(command: &mut Command) -> Result<String> {
        let line = record(command);
        if line.starts_with("mount --rbind /proc") || line.starts_with("umount") {
            return err(format!("{line} refused"));
        }
        Ok(String::new())
    }

    #[test]
    fn a_bind_that_is_still_shared_with_the_host_is_never_unmounted() {
        let dir = scratch();
        let root = dir.0.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let dev = root.join("dev").display().to_string();

        // A recursive unmount of a bind that is not yet a slave would also
        // unmount the host's own submounts.
        let failed = ChrootMounts::with(&root, rslave_fails).err().unwrap();
        assert!(failed.0.contains("make-rslave refused"), "{failed}");
        assert!(failed.0.contains(&format!("{dev} is left mounted")), "{failed}");
        assert_eq!(
            CALLS.with(|calls| calls.take()),
            vec![
                format!("mount --rbind /dev {dev}"),
                format!("mount --make-rslave {dev}"),
            ]
        );
    }

    #[test]
    fn an_earlier_bind_is_unmounted_when_a_later_one_fails() {
        let dir = scratch();
        let root = dir.0.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let dev = root.join("dev").display().to_string();
        let proc = root.join("proc").display().to_string();

        // /dev is bound and made a slave, then binding /proc fails.
        let failed = ChrootMounts::with(&root, proc_bind_and_umount_fail).err().unwrap();
        assert_eq!(
            CALLS.with(|calls| calls.take()),
            vec![
                format!("mount --rbind /dev {dev}"),
                format!("mount --make-rslave {dev}"),
                format!("mount --rbind /proc {proc}"),
                format!("umount -R {dev}"),
            ]
        );
        // A mount that cannot be undone is reported, not dropped silently.
        assert!(failed.0.contains(&format!("{dev} is still mounted")), "{failed}");
    }

    #[test]
    fn a_chroot_mount_point_must_be_a_real_directory() {
        let dir = scratch();
        let root = dir.0.join("root");
        let outside = dir.0.join("outside");
        std::fs::create_dir_all(root.join("proc")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("dev")).unwrap();
        std::fs::write(root.join("sys"), "not a directory").unwrap();

        let refused = chroot_mount_point(&root, "dev").unwrap_err();
        assert!(refused.0.contains("refusing to bind"), "{refused}");
        assert!(chroot_mount_point(&root, "sys").is_err());
        assert_eq!(chroot_mount_point(&root, "proc").unwrap(), root.join("proc"));
        // A missing one is created inside the generation.
        std::fs::remove_file(root.join("sys")).unwrap();
        assert_eq!(chroot_mount_point(&root, "sys").unwrap(), root.join("sys"));
        assert!(root.join("sys").is_dir());
    }

    #[test]
    fn strips_xbps_versions() {
        assert_eq!(pkgname_from_pkgver("htop-3.3.0_1"), "htop");
        assert_eq!(pkgname_from_pkgver("grub-x86_64-efi-2.12_2"), "grub-x86_64-efi");
        assert_eq!(pkgname_from_pkgver("noversion"), "noversion");
    }
}
