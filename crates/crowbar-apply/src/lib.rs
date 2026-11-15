//! First-cut Crowbar apply engine for Milestone 1.
//!
//! The engine never edits the running system. `apply` snapshots the booted
//! generation into a new Btrfs subvolume, converges that copy toward the
//! declared state, records it, and makes it the boot default. Rolling back is
//! choosing an older generation in the boot menu (or `crowbar rollback`).

pub mod bootmenu;
pub mod confine;
pub mod engine;
pub mod fstab;
pub mod generations;
pub mod ledger;
pub mod plan;
pub mod system;
pub mod timefmt;

use std::fmt;
use std::io::Write;
use std::path::Path;

#[derive(Debug)]
pub struct Error(pub String);

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error(e.to_string())
    }
}

impl From<crowbar_state::StateError> for Error {
    fn from(e: crowbar_state::StateError) -> Self {
        Error(e.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error(e.to_string())
    }
}

pub fn err<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error(msg.into()))
}

/// Why [`write_file`] failed, and on which side of the rename.
#[derive(Debug)]
pub struct WriteFailed {
    /// True when the new file is already in place under its final name and
    /// only the flush of its directory failed. False when the path still holds
    /// whatever it held before.
    pub renamed: bool,
    pub error: Error,
}

/// Write a file by writing a sibling temp file and renaming it into place.
///
/// The caller passes a path that is already resolved (see [`confine`]). The
/// temp file is created with `create_new`, which never follows a symlink, so
/// a link left under the temp name cannot redirect the write.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    write_file(path, contents, None).map_err(|failed| failed.error)
}

/// [`write_atomic`], with the mode set before the file gets its final name and
/// with an error that says whether the rename happened.
pub fn write_file(path: &Path, contents: &[u8], mode: Option<u32>) -> std::result::Result<(), WriteFailed> {
    let before_rename = |error: std::io::Error| WriteFailed {
        renamed: false,
        error: error.into(),
    };
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(parent).map_err(before_rename)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!(".{name}{}", crowbar_state::TEMP_SUFFIX));
    let _ = std::fs::remove_file(&tmp);
    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        if let Some(mode) = mode {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        }
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(before_rename(error));
    }
    // The rename is only durable once the directory entry is on disk.
    sync_dir(parent, path).map_err(|error| WriteFailed {
        renamed: true,
        error: error.into(),
    })
}

fn sync_dir(dir: &Path, written: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if testutil::dir_sync_should_fail(written) {
        return Err(std::io::Error::other("injected: directory sync failed"));
    }
    let _ = written;
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;

    pub struct Scratch(pub PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    thread_local! {
        static FAIL_DIR_SYNC: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    }

    /// Make the directory sync that follows the rename of `path` fail, on this
    /// thread, until the returned guard is dropped.
    pub fn fail_dir_sync_after(path: &std::path::Path) -> FailDirSync {
        FAIL_DIR_SYNC.with(|f| *f.borrow_mut() = Some(path.to_path_buf()));
        FailDirSync
    }

    pub struct FailDirSync;

    impl Drop for FailDirSync {
        fn drop(&mut self) {
            FAIL_DIR_SYNC.with(|f| *f.borrow_mut() = None);
        }
    }

    pub fn dir_sync_should_fail(path: &std::path::Path) -> bool {
        FAIL_DIR_SYNC.with(|f| f.borrow().as_deref() == Some(path))
    }

    pub fn scratch() -> Scratch {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "crowbar-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    #[test]
    fn write_atomic_does_not_write_through_a_planted_temp_link() {
        let dir = scratch();
        let victim = dir.0.join("victim");
        std::fs::write(&victim, "ORIGINAL").unwrap();
        std::os::unix::fs::symlink(&victim, dir.0.join(".motd.crowbar-tmp")).unwrap();

        write_atomic(&dir.0.join("motd"), b"CHANGED").unwrap();
        assert_eq!(std::fs::read_to_string(dir.0.join("motd")).unwrap(), "CHANGED");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "ORIGINAL");
        assert!(!dir.0.join(".motd.crowbar-tmp").exists());
    }

    #[test]
    fn a_failed_directory_sync_is_reported_as_after_the_rename() {
        let dir = scratch();
        let path = dir.0.join("motd");
        let _fail = crate::testutil::fail_dir_sync_after(&path);

        let failed = write_file(&path, b"NEW", None).unwrap_err();
        assert!(failed.renamed, "{failed:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "NEW");

        // A failure before the rename leaves the old content and says so.
        std::fs::create_dir_all(dir.0.join(".other.crowbar-tmp/blocker")).unwrap();
        std::fs::write(dir.0.join("other"), "OLD").unwrap();
        let failed = write_file(&dir.0.join("other"), b"NEW", None).unwrap_err();
        assert!(!failed.renamed, "{failed:?}");
        assert_eq!(std::fs::read_to_string(dir.0.join("other")).unwrap(), "OLD");
    }

    #[test]
    fn the_mode_is_set_before_the_file_gets_its_name() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch();
        let path = dir.0.join("secret");
        // Stop right after the rename: whatever mode the file has now is the
        // mode it had the moment it became visible under its name.
        let _fail = crate::testutil::fail_dir_sync_after(&path);
        assert!(write_file(&path, b"s3cret", Some(0o600)).unwrap_err().renamed);
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777, 0o600);
    }
}
