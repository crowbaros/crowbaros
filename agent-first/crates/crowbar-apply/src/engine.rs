//! Generations: snapshot, converge, record, and make bootable.

use crate::bootmenu::{BootEntry, MenuOptions, render_grub_cfg};
use crate::generations::{Generation, Layout, find_boot_files};
use crate::ledger::{self, Entry};
use crate::plan::{self, Change};
use crate::system::{PackageManager, Snapshotter};
use crate::{Result, err, under, write_atomic};
use crowbar_state::State;
use std::path::Path;

pub const STATE_PATH: &str = "/etc/crowbar/state.toml";
pub const GENERATION_PATH: &str = "/etc/crowbar/generation";

pub struct Engine<'a> {
    pub layout: Layout,
    pub pkgs: &'a dyn PackageManager,
    pub snap: &'a dyn Snapshotter,
    pub menu: MenuOptions,
    pub actor: String,
    pub now: fn() -> String,
}

#[derive(Debug)]
pub struct ApplyReport {
    pub from: u32,
    /// `None` when the state already matched and no generation was created.
    pub to: Option<u32>,
    pub changes: Vec<Change>,
}

/// The state a generation was built from, if it records one.
pub fn load_state(root: &Path) -> Result<Option<State>> {
    let path = under(root, STATE_PATH);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(State::from_toml_str(&std::fs::read_to_string(path)?)?))
}

impl Engine<'_> {
    fn log(
        &self,
        action: &str,
        generation: Option<u32>,
        from: Option<u32>,
        outcome: &str,
        detail: String,
    ) -> Result<()> {
        ledger::append(
            &self.layout.ledger_path(),
            &Entry {
                time: (self.now)(),
                actor: self.actor.clone(),
                action: action.into(),
                generation,
                from,
                outcome: outcome.into(),
                detail,
            },
        )
    }

    /// What `apply` would change, read from any root (the booted system is `/`).
    pub fn plan(&self, desired: &State, root: &Path) -> Result<Vec<Change>> {
        let previous = load_state(root)?;
        let actual = plan::read_actual(root, desired, previous.as_ref(), self.pkgs)?;
        plan::compute(desired, previous.as_ref(), &actual)
    }

    /// Register an existing `gens/1` subvolume as generation 1 (image build time).
    pub fn init(&self, state_text: &str) -> Result<Generation> {
        let state = State::from_toml_str(state_text)?;
        let root = self.layout.gen_root(1);
        if !root.is_dir() {
            return err(format!(
                "{} does not exist; build the base root there first",
                root.display()
            ));
        }
        if self.layout.meta_path(1).exists() {
            return err("generation 1 is already registered");
        }
        write_atomic(&under(&root, STATE_PATH), state_text.as_bytes())?;
        write_atomic(&under(&root, GENERATION_PATH), b"1\n")?;
        let (kernel, initrd) = find_boot_files(&root, 1)?;
        let generation = Generation {
            id: 1,
            parent: None,
            created: (self.now)(),
            summary: "base image".into(),
            state_fingerprint: state.fingerprint(),
            kernel,
            initrd,
        };
        self.layout.write_meta(&generation)?;
        self.layout.write_default(1)?;
        self.render_menu()?;
        self.log(
            "init",
            Some(1),
            None,
            "ok",
            "registered base image as generation 1".into(),
        )?;
        Ok(generation)
    }

    /// Build generation N+1 from `from`, converge it to `desired`, and make it the
    /// boot default. The running system is not modified.
    pub fn apply(&self, desired: &State, desired_text: &str, from: u32) -> Result<ApplyReport> {
        let source = self.layout.gen_root(from);
        if !source.is_dir() {
            return err(format!(
                "source generation {from} does not exist at {}",
                source.display()
            ));
        }
        let to = self.layout.next_id()?;
        let target = self.layout.gen_root(to);
        self.snap.snapshot(&source, &target)?;
        match self.build(desired, desired_text, from, to, &target) {
            Ok(changes) if changes.is_empty() => {
                self.snap.delete(&target)?;
                Ok(ApplyReport {
                    from,
                    to: None,
                    changes,
                })
            }
            Ok(changes) => Ok(ApplyReport {
                from,
                to: Some(to),
                changes,
            }),
            Err(e) => {
                let cleanup = self.snap.delete(&target);
                self.log("apply", Some(to), Some(from), "failed", e.to_string())?;
                cleanup?;
                Err(e)
            }
        }
    }

    fn build(&self, desired: &State, desired_text: &str, from: u32, to: u32, root: &Path) -> Result<Vec<Change>> {
        let changes = self.plan(desired, root)?;
        if changes.is_empty() {
            return Ok(changes);
        }
        plan::apply_changes(root, desired, &changes, self.pkgs)?;
        write_atomic(&under(root, STATE_PATH), desired_text.as_bytes())?;
        write_atomic(&under(root, GENERATION_PATH), format!("{to}\n").as_bytes())?;
        let (kernel, initrd) = find_boot_files(root, to)?;
        let summary = plan::summarize(&changes);
        self.layout.write_meta(&Generation {
            id: to,
            parent: Some(from),
            created: (self.now)(),
            summary: summary.clone(),
            state_fingerprint: desired.fingerprint(),
            kernel,
            initrd,
        })?;
        self.layout.write_default(to)?;
        self.render_menu()?;
        let detail = changes.iter().map(|c| c.to_string()).collect::<Vec<_>>().join("; ");
        self.log("apply", Some(to), Some(from), "ok", detail)?;
        Ok(changes)
    }

    /// Make `target` the boot default.
    pub fn rollback(&self, target: u32, booted: Option<u32>) -> Result<()> {
        self.layout.get(target)?;
        if !self.layout.gen_root(target).is_dir() {
            return err(format!("generation {target} has a record but no subvolume"));
        }
        let previous = self.layout.read_default()?;
        self.layout.write_default(target)?;
        self.render_menu()?;
        self.log(
            "rollback",
            Some(target),
            previous,
            "ok",
            format!(
                "boot default {} -> {target} (booted: {})",
                fmt_opt(previous),
                fmt_opt(booted)
            ),
        )
    }

    pub fn render_menu(&self) -> Result<()> {
        let gens = self.layout.list()?;
        let Some(newest) = gens.last() else {
            return err("no generations are registered");
        };
        let default = self.layout.read_default()?.unwrap_or(newest.id);
        let entries: Vec<BootEntry> = gens
            .iter()
            .map(|g| BootEntry {
                id: g.id,
                title: format!("CrowbarOS generation {} - {} - {}", g.id, g.created, g.summary),
                kernel: g.kernel.clone(),
                initrd: g.initrd.clone(),
            })
            .collect();
        write_atomic(
            &self.layout.grub_cfg(),
            render_grub_cfg(&entries, default, &self.menu).as_bytes(),
        )
    }
}

fn fmt_opt(id: Option<u32>) -> String {
    id.map_or_else(|| "unknown".into(), |i| i.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::CopySnapshot;
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    struct FakePkgs {
        installed: RefCell<BTreeSet<String>>,
        available: BTreeSet<&'static str>,
    }

    impl PackageManager for FakePkgs {
        fn manual_packages(&self, root: &Path) -> Result<BTreeSet<String>> {
            let db = under(root, "/var/db/fake-pkgs");
            Ok(std::fs::read_to_string(db)
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect())
        }
        fn install(&self, root: &Path, packages: &[String]) -> Result<()> {
            let mut set = self.manual_packages(root)?;
            for p in packages {
                if !self.available.contains(p.as_str()) {
                    return err(format!("package {p} not found in repository"));
                }
                set.insert(p.clone());
                self.installed.borrow_mut().insert(p.clone());
            }
            write_atomic(
                &under(root, "/var/db/fake-pkgs"),
                set.into_iter().collect::<Vec<_>>().join("\n").as_bytes(),
            )
        }
        fn remove(&self, root: &Path, packages: &[String]) -> Result<()> {
            let mut set = self.manual_packages(root)?;
            for p in packages {
                set.remove(p);
            }
            write_atomic(
                &under(root, "/var/db/fake-pkgs"),
                set.into_iter().collect::<Vec<_>>().join("\n").as_bytes(),
            )
        }
    }

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch() -> Scratch {
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

    fn base_root(top: &Path) {
        let root = top.join("gens/1");
        for dir in ["boot", "etc/sv/sshd", "etc/sv/dhcpcd", "etc/runit/runsvdir/default"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("boot/vmlinuz-6.18.63_1"), "kernel").unwrap();
        std::fs::write(root.join("boot/initramfs-6.18.63_1.img"), "initrd").unwrap();
        std::fs::write(root.join("etc/hostname"), "crowbar-lab\n").unwrap();
        std::fs::write(root.join("etc/fstab"), "LABEL=crowbar-root / btrfs rw 0 0\n").unwrap();
        std::os::unix::fs::symlink("/etc/sv/sshd", root.join("etc/runit/runsvdir/default/sshd")).unwrap();
    }

    const BASE: &str = "version = 1\n[system]\nhostname = \"crowbar-lab\"\n[services]\nenabled = [\"sshd\"]\n";
    const GOOD: &str = r#"version = 1
[system]
hostname = "crowbar-lab"
[packages]
install = ["jq"]
[services]
enabled = ["sshd", "dhcpcd"]
[[file]]
path = "/etc/motd"
content = "hello from generation 2\n"
"#;
    const BROKEN: &str = r#"version = 1
[system]
hostname = "crowbar-lab"
[packages]
install = ["jq"]
[services]
enabled = ["sshd", "dhcpcd"]
[[file]]
path = "/etc/motd"
content = "hello from generation 2\n"
[[mount]]
device = "LABEL=crowbar-data"
target = "/srv/data"
fstype = "ext4"
"#;

    fn engine<'a>(top: &Path, pkgs: &'a FakePkgs, snap: &'a CopySnapshot) -> Engine<'a> {
        Engine {
            layout: Layout::new(top),
            pkgs,
            snap,
            menu: MenuOptions::default(),
            actor: "test".into(),
            now: || "2026-11-14T20:00:00Z".into(),
        }
    }

    fn pkgs() -> FakePkgs {
        FakePkgs {
            installed: RefCell::new(BTreeSet::new()),
            available: ["jq", "htop"].into_iter().collect(),
        }
    }

    #[test]
    fn full_milestone_loop_on_a_scratch_tree() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);

        e.init(BASE).unwrap();
        assert_eq!(e.layout.read_default().unwrap(), Some(1));
        assert!(
            std::fs::read_to_string(e.layout.grub_cfg())
                .unwrap()
                .contains("set default=\"gen-1\"")
        );

        let good = State::from_toml_str(GOOD).unwrap();
        let report = e.apply(&good, GOOD, 1).unwrap();
        assert_eq!(report.to, Some(2));
        let g2 = top.join("gens/2");
        assert_eq!(
            std::fs::read_to_string(g2.join("etc/motd")).unwrap(),
            "hello from generation 2\n"
        );
        assert!(
            std::fs::symlink_metadata(g2.join("etc/runit/runsvdir/default/dhcpcd"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(g2.join("etc/crowbar/generation")).unwrap(),
            "2\n"
        );
        // Generation 1 is untouched.
        assert!(!top.join("gens/1/etc/motd").exists());
        assert!(!top.join("gens/1/etc/runit/runsvdir/default/dhcpcd").exists());

        // Re-applying the same state is a no-op and creates nothing.
        let again = e.apply(&good, GOOD, 2).unwrap();
        assert_eq!(again.to, None);
        assert!(!top.join("gens/3").exists());

        let broken = State::from_toml_str(BROKEN).unwrap();
        let report = e.apply(&broken, BROKEN, 2).unwrap();
        assert_eq!(report.to, Some(3));
        assert!(report.changes.iter().any(Change::affects_boot));
        let fstab = std::fs::read_to_string(top.join("gens/3/etc/fstab")).unwrap();
        assert!(fstab.contains("LABEL=crowbar-data\t/srv/data"));
        assert!(
            !std::fs::read_to_string(top.join("gens/2/etc/fstab"))
                .unwrap()
                .contains("crowbar-data")
        );
        let cfg = std::fs::read_to_string(e.layout.grub_cfg()).unwrap();
        assert!(cfg.contains("set default=\"gen-3\""));
        assert!(cfg.contains("--id gen-2") && cfg.contains("--id gen-1"));

        e.rollback(2, Some(2)).unwrap();
        assert_eq!(e.layout.read_default().unwrap(), Some(2));
        assert!(
            std::fs::read_to_string(e.layout.grub_cfg())
                .unwrap()
                .contains("set default=\"gen-2\"")
        );

        let actions: Vec<String> = ledger::read(&e.layout.ledger_path())
            .unwrap()
            .into_iter()
            .map(|l| l.action)
            .collect();
        assert_eq!(actions, vec!["init", "apply", "apply", "rollback"]);
    }

    #[test]
    fn failed_apply_leaves_no_generation_and_keeps_default() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();

        let text = BASE.replace("[\"sshd\"]", "[\"sshd\", \"does-not-exist\"]");
        let err = e.apply(&State::from_toml_str(&text).unwrap(), &text, 1).unwrap_err();
        assert!(err.0.contains("does-not-exist"), "{err}");
        assert!(!top.join("gens/2").exists());
        assert_eq!(e.layout.read_default().unwrap(), Some(1));
        let last = ledger::read(&e.layout.ledger_path()).unwrap().pop().unwrap();
        assert_eq!((last.action.as_str(), last.outcome.as_str()), ("apply", "failed"));

        let text = BASE.replace("[services]", "[packages]\ninstall = [\"nope\"]\n[services]");
        assert!(e.apply(&State::from_toml_str(&text).unwrap(), &text, 1).is_err());
        assert!(!top.join("gens/3").exists());
    }

    #[test]
    fn removing_a_declared_package_and_file() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();
        e.apply(&State::from_toml_str(GOOD).unwrap(), GOOD, 1).unwrap();

        let report = e.apply(&State::from_toml_str(BASE).unwrap(), BASE, 2).unwrap();
        assert_eq!(report.to, Some(3));
        assert!(report.changes.contains(&Change::RemovePackages(vec!["jq".into()])));
        assert!(report.changes.contains(&Change::RemoveFile {
            path: "/etc/motd".into()
        }));
        assert!(report.changes.contains(&Change::DisableService("dhcpcd".into())));
        assert!(!top.join("gens/3/etc/motd").exists());
    }

    #[test]
    fn rollback_rejects_unknown_generation() {
        let dir = scratch();
        base_root(&dir.0);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(&dir.0, &pkgs, &snap);
        e.init(BASE).unwrap();
        assert!(e.rollback(7, None).is_err());
        assert_eq!(e.layout.read_default().unwrap(), Some(1));
    }
}
