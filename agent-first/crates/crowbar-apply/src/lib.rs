//! First-cut Crowbar apply engine for Milestone 1.
//!
//! The engine never edits the running system. `apply` snapshots the booted
//! generation into a new Btrfs subvolume, converges that copy toward the
//! declared state, records it, and makes it the boot default. Rolling back is
//! choosing an older generation in the boot menu (or `crowbar rollback`).

pub mod bootmenu;
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

/// Write a file by writing a sibling temp file and renaming it into place.
pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = path.with_file_name(format!(".{name}.crowbar-tmp"));
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Resolve an absolute in-system path under a generation root.
pub fn under(root: &Path, absolute: &str) -> std::path::PathBuf {
    root.join(absolute.trim_start_matches('/'))
}
