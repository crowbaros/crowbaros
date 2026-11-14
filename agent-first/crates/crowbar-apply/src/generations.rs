//! On-disk layout of generations on the Btrfs top level (subvolid 5).
//!
//! ```text
//! /gens/<id>/                  one writable subvolume per generation (a full root)
//! /crowbar/generations/<id>.json   record: parent, time, summary, boot files
//! /crowbar/default             id GRUB boots by default
//! /crowbar/grub.cfg            generated boot menu
//! /crowbar/ledger.jsonl        append-only history of applies and rollbacks
//! ```

use crate::{Result, err, write_atomic};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Generation {
    pub id: u32,
    pub parent: Option<u32>,
    pub created: String,
    pub summary: String,
    pub state_fingerprint: String,
    pub kernel: String,
    pub initrd: String,
}

#[derive(Debug, Clone)]
pub struct Layout {
    pub top: PathBuf,
}

impl Layout {
    pub fn new(top: impl Into<PathBuf>) -> Self {
        Layout { top: top.into() }
    }
    pub fn gens_dir(&self) -> PathBuf {
        self.top.join("gens")
    }
    pub fn gen_root(&self, id: u32) -> PathBuf {
        self.gens_dir().join(id.to_string())
    }
    pub fn meta_dir(&self) -> PathBuf {
        self.top.join("crowbar/generations")
    }
    pub fn meta_path(&self, id: u32) -> PathBuf {
        self.meta_dir().join(format!("{id}.json"))
    }
    pub fn grub_cfg(&self) -> PathBuf {
        self.top.join("crowbar/grub.cfg")
    }
    pub fn default_path(&self) -> PathBuf {
        self.top.join("crowbar/default")
    }
    pub fn ledger_path(&self) -> PathBuf {
        self.top.join("crowbar/ledger.jsonl")
    }

    pub fn list(&self) -> Result<Vec<Generation>> {
        let mut gens = Vec::new();
        let dir = self.meta_dir();
        if !dir.exists() {
            return Ok(gens);
        }
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                gens.push(serde_json::from_str(&std::fs::read_to_string(&path)?)?);
            }
        }
        gens.sort_by_key(|g: &Generation| g.id);
        Ok(gens)
    }

    pub fn get(&self, id: u32) -> Result<Generation> {
        let path = self.meta_path(id);
        if !path.exists() {
            return err(format!("generation {id} has no record at {}", path.display()));
        }
        Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
    }

    /// One past the highest id seen in either records or subvolumes, so an
    /// abandoned subvolume is never reused.
    pub fn next_id(&self) -> Result<u32> {
        let mut max = self.list()?.iter().map(|g| g.id).max().unwrap_or(0);
        if self.gens_dir().exists() {
            for entry in std::fs::read_dir(self.gens_dir())? {
                if let Some(id) = entry?.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) {
                    max = max.max(id);
                }
            }
        }
        Ok(max + 1)
    }

    pub fn write_meta(&self, generation: &Generation) -> Result<()> {
        let json = serde_json::to_string_pretty(generation)? + "\n";
        write_atomic(&self.meta_path(generation.id), json.as_bytes())
    }

    pub fn read_default(&self) -> Result<Option<u32>> {
        let path = self.default_path();
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(path)?;
        match text.trim().parse() {
            Ok(id) => Ok(Some(id)),
            Err(_) => err(format!(
                "{} does not hold a generation id",
                self.default_path().display()
            )),
        }
    }

    pub fn write_default(&self, id: u32) -> Result<()> {
        write_atomic(&self.default_path(), format!("{id}\n").as_bytes())
    }
}

/// Natural-ish version ordering: compare numeric runs as numbers.
fn version_key(version: &str) -> Vec<(u64, String)> {
    let mut key = Vec::new();
    let mut digits = String::new();
    let mut other = String::new();
    for c in version.chars() {
        if c.is_ascii_digit() {
            if !other.is_empty() {
                key.push((0, std::mem::take(&mut other)));
            }
            digits.push(c);
        } else {
            if !digits.is_empty() {
                key.push((digits.parse().unwrap_or(u64::MAX), String::new()));
                digits.clear();
            }
            other.push(c);
        }
    }
    if !digits.is_empty() {
        key.push((digits.parse().unwrap_or(u64::MAX), String::new()));
    }
    if !other.is_empty() {
        key.push((0, other));
    }
    key
}

/// Find the newest kernel with a matching initramfs in a generation root and
/// return both as paths relative to the Btrfs top level.
pub fn find_boot_files(gen_root: &Path, id: u32) -> Result<(String, String)> {
    let boot = gen_root.join("boot");
    let mut versions = Vec::new();
    if boot.exists() {
        for entry in std::fs::read_dir(&boot)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if let Some(version) = name.strip_prefix("vmlinuz-")
                && boot.join(format!("initramfs-{version}.img")).exists()
            {
                versions.push(version.to_string());
            }
        }
    }
    versions.sort_by_key(|v| version_key(v));
    match versions.last() {
        Some(v) => Ok((
            format!("/gens/{id}/boot/vmlinuz-{v}"),
            format!("/gens/{id}/boot/initramfs-{v}.img"),
        )),
        None => err(format!(
            "generation {id} has no kernel with a matching initramfs in {}",
            boot.display()
        )),
    }
}

/// `crowbar.generation=N` on the kernel command line names the booted generation.
pub fn parse_cmdline_generation(cmdline: &str) -> Option<u32> {
    cmdline
        .split_whitespace()
        .find_map(|arg| arg.strip_prefix("crowbar.generation=")?.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_generation_from_cmdline() {
        assert_eq!(
            parse_cmdline_generation("root=LABEL=x rw crowbar.generation=12 quiet"),
            Some(12)
        );
        assert_eq!(parse_cmdline_generation("root=LABEL=x crowbar.generation=abc"), None);
        assert_eq!(parse_cmdline_generation("root=LABEL=x"), None);
    }

    #[test]
    fn versions_sort_numerically() {
        let mut v = vec!["6.9.1_1", "6.18.63_1", "6.18.7_2"];
        v.sort_by_key(|s| version_key(s));
        assert_eq!(v, vec!["6.9.1_1", "6.18.7_2", "6.18.63_1"]);
    }
}
