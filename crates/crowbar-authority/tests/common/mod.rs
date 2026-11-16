//! Helpers the test files share.

// Each test file is its own crate and uses only some of these.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crowbar_authority::{Grant, Holder, Rule};
use crowbar_capability::{EffectLimits, Rate, Resource, Right, Rights, Time};
use crowbar_labels::ObjectId;

/// The time every test starts at.
pub const NOW: Time = Time::from_secs(1_000);

/// A minute after [`NOW`].
pub const LATER: Time = Time::from_secs(1_060);

/// A directory of its own for one test, removed when the test ends.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "crowbar-authority-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        Scratch(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn morning_pages() -> Holder {
    Holder::Tool("morning-pages".to_string())
}

/// "Morning Pages may read and write your writing."
pub fn writing_grant() -> Grant {
    Grant {
        holder: morning_pages(),
        resource: Resource::Data([ObjectId::new("writing/journal.md")].into()),
        rights: Rights::from([Right::Read, Right::Write]),
        limits: EffectLimits::new(),
        reason: "Keeps your morning pages.".to_string(),
    }
}

/// "Morning Pages may sync to your Nextcloud, at most once every ten
/// minutes."
pub fn sync_rule() -> Rule {
    Rule {
        holder: morning_pages(),
        operation: "nextcloud.sync".to_string(),
        limits: EffectLimits::new().rate(Rate::per(1, 600)),
        sources: [].into(),
    }
}
