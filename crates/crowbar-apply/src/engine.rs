//! Generations: snapshot, converge, record, and make bootable.
//!
//! An apply has two phases, with one commit point between them.
//!
//! 1. **Build.** Everything happens inside the new subvolume, which is then
//!    flushed to disk. A failure here deletes the subvolume. The default, the
//!    boot menu, and the generation records were never touched.
//! 2. **Publish.** Write the record, switch the default, render the boot menu.
//!    The atomic rename of `grub.cfg` is the commit point, because that file is
//!    the only one GRUB reads. A failure before it is undone: the default goes
//!    back, the record is removed, and the subvolume is deleted, but only once
//!    the menu on disk is known not to name it.
//!
//! After the commit point nothing removes the generation. Whatever fails then
//! (flushing the menu's directory, writing the ledger entry) is reported next
//! to an apply that stands.

use crate::bootmenu::{self, BootEntry, MenuOptions, boot_path_ok, render_grub_cfg};
use crate::confine::entry_in_root;
use crate::generations::{Generation, Layout, find_boot_files};
use crate::ledger::{self, Entry};
use crate::plan::{self, Change};
use crate::system::{PackageManager, Snapshotter};
use crate::{Error, Result, WriteFailed, err, write_atomic, write_file};
use crowbar_state::State;
use std::fmt;
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
    /// The generation is committed, but these later steps failed.
    pub after_commit: Vec<String>,
}

/// An `init`, `apply`, or `rollback` that did not take effect.
#[derive(Debug)]
pub struct Failure {
    pub message: String,
    /// False: the boot menu, the boot default, the generation records, and the
    /// subvolumes are as they were before the command. True: something could
    /// not be put back or cleaned up, and the message says what is left.
    pub left_behind: bool,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

/// An error raised before anything was changed.
impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Failure {
            message: e.0,
            left_behind: false,
        }
    }
}

/// How a failed `publish` was resolved.
#[derive(Debug, PartialEq, Eq)]
enum Withdrawal {
    /// The default and the records are as they were, and the boot menu does
    /// not name the generation. Its subvolume can be deleted.
    Withdrawn,
    /// The boot menu on disk names the generation, so it is committed after
    /// all. Its record and the default were made to agree with the menu.
    Committed,
}

/// The state a generation was built from, if it records one.
pub fn load_state(root: &Path) -> Result<Option<State>> {
    match plan::read_own(root, STATE_PATH)? {
        Some(text) => Ok(Some(State::from_toml_str(&text)?)),
        None => Ok(None),
    }
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
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

    /// Take the lock that serializes everything that allocates a generation id
    /// or writes the default and the boot menu. It is released when the
    /// returned file is dropped. A second caller fails at once; it does not wait.
    pub fn lock(&self) -> Result<std::fs::File> {
        let path = self.layout.lock_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => err(format!(
                "another crowbar apply or rollback is running (lock held on {}); try again when it finishes",
                path.display()
            )),
            Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
        }
    }

    /// What `apply` would change, read from any root (the booted system is `/`).
    pub fn plan(&self, desired: &State, root: &Path) -> Result<Vec<Change>> {
        let previous = load_state(root)?;
        let actual = plan::read_actual(root, desired, previous.as_ref(), self.pkgs)?;
        plan::compute(desired, previous.as_ref(), &actual)
    }

    /// Register an existing `gens/1` subvolume as generation 1 (image build time).
    /// The list holds what failed after generation 1 was committed.
    pub fn init(&self, state_text: &str) -> std::result::Result<(Generation, Vec<String>), Failure> {
        let _lock = self.lock()?;
        let state = State::from_toml_str(state_text).map_err(Error::from)?;
        let root = self.layout.gen_root(1);
        if !root.is_dir() {
            return Err(Error(format!(
                "{} does not exist; build the base root there first",
                root.display()
            ))
            .into());
        }
        if self.layout.meta_path(1).exists() {
            return Err(Error("generation 1 is already registered".into()).into());
        }
        write_atomic(&entry_in_root(&root, STATE_PATH)?.host, state_text.as_bytes())?;
        write_atomic(&entry_in_root(&root, GENERATION_PATH)?.host, b"1\n")?;
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
        let mut after_commit = self.commit(&generation, None)?;
        let logged = self.log(
            "init",
            Some(1),
            None,
            "ok",
            "registered base image as generation 1".into(),
        );
        after_commit.extend(
            logged
                .err()
                .map(|e| format!("the ledger entry could not be written: {e}")),
        );
        Ok((generation, after_commit))
    }

    /// Build generation N+1 from `from`, converge it to `desired`, and make it the
    /// boot default. The running system is not modified.
    pub fn apply(&self, desired: &State, desired_text: &str, from: u32) -> std::result::Result<ApplyReport, Failure> {
        let _lock = self.lock()?;
        let source = self.layout.gen_root(from);
        if !source.is_dir() {
            return Err(Error(format!(
                "source generation {from} does not exist at {}",
                source.display()
            ))
            .into());
        }
        let previous_default = self.layout.read_default()?;
        let to = self.layout.next_id()?;
        let target = self.layout.gen_root(to);
        if let Err(e) = self.snap.snapshot(&source, &target) {
            return Err(self.abandon(to, from, &target, e));
        }

        let built = self.build(desired, desired_text, from, to, &target).and_then(|built| {
            if built.is_some() {
                // Before the boot menu can name this generation, its files
                // must be on disk, not only in memory.
                self.snap.sync(&target)?;
            }
            Ok(built)
        });
        let (changes, generation) = match built {
            Ok(Some(built)) => built,
            Ok(None) => {
                return match self.snap.delete(&target) {
                    Ok(()) => Ok(ApplyReport {
                        from,
                        to: None,
                        changes: Vec::new(),
                        after_commit: Vec::new(),
                    }),
                    Err(e) => Err(Failure {
                        message: format!(
                            "nothing needed to change, but the scratch copy at {} could not be deleted: {e}",
                            target.display()
                        ),
                        left_behind: true,
                    }),
                };
            }
            Err(e) => return Err(self.abandon(to, from, &target, e)),
        };

        let mut after_commit = match self.commit(&generation, previous_default) {
            Ok(problems) => problems,
            Err(failure) if failure.left_behind => {
                let failure = Failure {
                    message: format!(
                        "{failure}\nthe subvolume of generation {to} is kept at {} so that the boot menu cannot \
                         name a root that is gone",
                        target.display()
                    ),
                    left_behind: true,
                };
                let _ = self.log("apply", Some(to), Some(from), "failed", failure.message.clone());
                return Err(failure);
            }
            Err(failure) => return Err(self.abandon(to, from, &target, Error(failure.message))),
        };

        // Committed. Nothing below may remove the generation.
        let detail = changes.iter().map(|c| c.to_string()).collect::<Vec<_>>().join("; ");
        let logged = self.log("apply", Some(to), Some(from), "ok", detail);
        after_commit.extend(
            logged
                .err()
                .map(|e| format!("the ledger entry could not be written: {e}")),
        );
        Ok(ApplyReport {
            from,
            to: Some(to),
            changes,
            after_commit,
        })
    }

    /// Converge the new subvolume. Touches nothing outside `root`.
    fn build(
        &self,
        desired: &State,
        desired_text: &str,
        from: u32,
        to: u32,
        root: &Path,
    ) -> Result<Option<(Vec<Change>, Generation)>> {
        let changes = self.plan(desired, root)?;
        if changes.is_empty() {
            return Ok(None);
        }
        plan::apply_changes(root, desired, &changes, self.pkgs)?;
        write_atomic(&entry_in_root(root, STATE_PATH)?.host, desired_text.as_bytes())?;
        write_atomic(
            &entry_in_root(root, GENERATION_PATH)?.host,
            format!("{to}\n").as_bytes(),
        )?;
        let (kernel, initrd) = find_boot_files(root, to)?;
        let generation = Generation {
            id: to,
            parent: Some(from),
            created: (self.now)(),
            summary: plan::summarize(&changes),
            state_fingerprint: desired.fingerprint(),
            kernel,
            initrd,
        };
        Ok(Some((changes, generation)))
    }

    /// Delete a generation that was never published and record the failure.
    /// Returns the failure to hand back, with any cleanup problem added to it.
    fn abandon(&self, to: u32, from: u32, target: &Path, cause: Error) -> Failure {
        let mut message = cause.to_string();
        let mut left_behind = false;
        if std::fs::symlink_metadata(target).is_ok()
            && let Err(e) = self.snap.delete(target)
        {
            message.push_str(&format!(
                "\nalso: could not delete the half-built generation {to} at {}: {e}",
                target.display()
            ));
            left_behind = true;
        }
        if let Err(e) = self.log("apply", Some(to), Some(from), "failed", cause.to_string()) {
            message.push_str(&format!("\nalso: could not record the failure in the ledger: {e}"));
        }
        Failure { message, left_behind }
    }

    /// Publish a generation, and undo the publish if it fails before the
    /// commit point.
    ///
    /// `Ok(problems)`: the generation is committed, and `problems` lists what
    /// failed afterwards. `Err` with `left_behind` false: nothing was changed.
    /// `Err` with `left_behind` true: the publish failed and could not be fully
    /// undone, so the caller must keep the subvolume.
    fn commit(
        &self,
        generation: &Generation,
        previous_default: Option<u32>,
    ) -> std::result::Result<Vec<String>, Failure> {
        let id = generation.id;
        let cause = match self.publish(generation) {
            Ok(problem) => return Ok(problem.into_iter().collect()),
            Err(e) => e,
        };
        match self.unpublish(generation, previous_default) {
            Ok(Withdrawal::Withdrawn) => Err(cause.into()),
            Ok(Withdrawal::Committed) => Ok(vec![format!(
                "publishing reported a failure, but the boot menu on disk names generation {id}, so it stands: {cause}"
            )]),
            Err(why) => Err(Failure {
                message: format!("{cause}\ngeneration {id} could not be fully withdrawn: {why}"),
                left_behind: true,
            }),
        }
    }

    /// Record, default, boot menu, in that order. The rename of `grub.cfg` in
    /// the last step is the commit point.
    ///
    /// `Ok(None)`: committed. `Ok(Some(problem))`: committed, because the new
    /// menu is in place, but flushing its directory failed. `Err`: the menu was
    /// not replaced.
    fn publish(&self, generation: &Generation) -> Result<Option<String>> {
        self.layout.write_meta(generation)?;
        self.layout.write_default(generation.id)?;
        match self.write_menu() {
            Ok(()) => Ok(None),
            Err(failed) if failed.renamed => Ok(Some(format!(
                "the new boot menu is in place but could not be flushed to disk: {}",
                failed.error
            ))),
            Err(failed) => Err(failed.error),
        }
    }

    /// Resolve a `publish` that reported a failure before the commit point.
    ///
    /// It looks at what GRUB will read before it touches anything. If the menu
    /// on disk names the generation, the generation is committed, and its
    /// record and the default are made to agree. Otherwise the default and the
    /// record go back. `Err` says what could not be done.
    fn unpublish(
        &self,
        generation: &Generation,
        previous_default: Option<u32>,
    ) -> std::result::Result<Withdrawal, String> {
        let id = generation.id;
        let named = match std::fs::read_to_string(self.layout.grub_cfg()) {
            Ok(cfg) => Ok(bootmenu::references(&cfg, id)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("the boot menu could not be read back: {e}")),
        };
        if named == Ok(true) {
            if self.layout.get(id).ok().as_ref() != Some(generation) {
                self.layout.write_meta(generation).map_err(|e| {
                    format!("the boot menu names generation {id}, but its record could not be written: {e}")
                })?;
            }
            if self.layout.read_default().ok().flatten() != Some(id) {
                self.layout.write_default(id).map_err(|e| {
                    format!("the boot menu names generation {id}, but the boot default could not be set to it: {e}")
                })?;
            }
            return Ok(Withdrawal::Committed);
        }
        self.restore_default(previous_default)?;
        remove_if_present(&self.layout.meta_path(id))
            .map_err(|e| format!("the record of generation {id} could not be removed: {e}"))?;
        named.map(|_| Withdrawal::Withdrawn)
    }

    fn restore_default(&self, previous: Option<u32>) -> std::result::Result<(), String> {
        match previous {
            Some(previous) => self.layout.write_default(previous).map_err(|e| e.to_string()),
            None => remove_if_present(&self.layout.default_path()).map_err(|e| e.to_string()),
        }
        .map_err(|e| format!("the boot default could not be restored: {e}"))
    }

    /// Make `target` the boot default. The list holds what failed after the
    /// rollback took effect.
    pub fn rollback(&self, target: u32, booted: Option<u32>) -> std::result::Result<Vec<String>, Failure> {
        let _lock = self.lock()?;
        self.layout.get(target)?;
        if !self.layout.gen_root(target).is_dir() {
            return Err(Error(format!("generation {target} has a record but no subvolume")).into());
        }
        let previous = self.layout.read_default()?;
        // Every generation in the menu exists, so either default boots. If the
        // menu is not replaced, the old default goes back so the files agree.
        let undo = |cause: Error| match self.restore_default(previous) {
            Ok(()) => Failure::from(cause),
            Err(why) => Failure {
                message: format!("{cause}\nalso: {why}"),
                left_behind: true,
            },
        };
        if let Err(e) = self.layout.write_default(target) {
            return Err(undo(e));
        }
        let mut after_commit = Vec::new();
        match self.write_menu() {
            Ok(()) => {}
            Err(failed) if failed.renamed => after_commit.push(format!(
                "the new boot menu is in place but could not be flushed to disk: {}",
                failed.error
            )),
            Err(failed) => return Err(undo(failed.error)),
        }
        let logged = self.log(
            "rollback",
            Some(target),
            previous,
            "ok",
            format!(
                "boot default {} -> {target} (booted: {})",
                fmt_opt(previous),
                fmt_opt(booted)
            ),
        );
        after_commit.extend(
            logged
                .err()
                .map(|e| format!("the ledger entry could not be written: {e}")),
        );
        Ok(after_commit)
    }

    /// Regenerate the boot menu from the generation records.
    pub fn render_menu(&self) -> Result<()> {
        let _lock = self.lock()?;
        self.write_menu().map_err(|failed| failed.error)
    }

    fn write_menu(&self) -> std::result::Result<(), WriteFailed> {
        let rendered = (|| -> Result<String> {
            let gens = self.layout.list()?;
            let Some(newest) = gens.last() else {
                return err("no generations are registered");
            };
            let default = self.layout.read_default()?.unwrap_or(newest.id);
            let mut entries = Vec::new();
            for g in &gens {
                // The records are the engine's own, but these two strings go
                // into the one file that boots every generation.
                for path in [&g.kernel, &g.initrd] {
                    if !boot_path_ok(path, g.id) {
                        return err(format!(
                            "the record of generation {} names a boot file that cannot go into the boot menu: {path:?}",
                            g.id
                        ));
                    }
                }
                entries.push(BootEntry {
                    id: g.id,
                    title: format!("CrowbarOS generation {} - {} - {}", g.id, g.created, g.summary),
                    kernel: g.kernel.clone(),
                    initrd: g.initrd.clone(),
                });
            }
            Ok(render_grub_cfg(&entries, default, &self.menu))
        })()
        .map_err(|error| WriteFailed { renamed: false, error })?;
        write_file(&self.layout.grub_cfg(), rendered.as_bytes(), None)
    }
}

fn fmt_opt(id: Option<u32>) -> String {
    id.map_or_else(|| "unknown".into(), |i| i.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system::CopySnapshot;
    use crate::testutil::scratch;
    use std::cell::RefCell;
    use std::collections::BTreeSet;

    struct FakePkgs {
        installed: RefCell<BTreeSet<String>>,
        available: BTreeSet<&'static str>,
    }

    impl PackageManager for FakePkgs {
        fn manual_packages(&self, root: &Path) -> Result<BTreeSet<String>> {
            Ok(std::fs::read_to_string(root.join("var/db/fake-pkgs"))
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
                &root.join("var/db/fake-pkgs"),
                set.into_iter().collect::<Vec<_>>().join("\n").as_bytes(),
            )
        }
        fn remove(&self, root: &Path, packages: &[String]) -> Result<()> {
            let mut set = self.manual_packages(root)?;
            for p in packages {
                set.remove(p);
            }
            write_atomic(
                &root.join("var/db/fake-pkgs"),
                set.into_iter().collect::<Vec<_>>().join("\n").as_bytes(),
            )
        }
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
        engine_with(top, pkgs, snap)
    }

    fn engine_with<'a>(top: &Path, pkgs: &'a FakePkgs, snap: &'a dyn Snapshotter) -> Engine<'a> {
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
            .entries
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
        assert!(err.message.contains("does-not-exist"), "{err}");
        assert!(!err.left_behind);
        assert!(!top.join("gens/2").exists());
        assert_eq!(e.layout.read_default().unwrap(), Some(1));
        let last = ledger::read(&e.layout.ledger_path()).unwrap().entries.pop().unwrap();
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

    fn file_state(path: &str) -> String {
        format!("{BASE}[[file]]\npath = \"{path}\"\ncontent = \"CHANGED\"\n")
    }

    #[test]
    fn a_managed_file_cannot_leave_the_generation_through_a_symlink() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let outside = top.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("value"), "ORIGINAL").unwrap();
        std::os::unix::fs::symlink(&outside, top.join("gens/1/etc/alias")).unwrap();
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();

        let text = file_state("/etc/alias/value");
        let report = e.apply(&State::from_toml_str(&text).unwrap(), &text, 1).unwrap();
        assert_eq!(report.to, Some(2));
        assert_eq!(std::fs::read_to_string(outside.join("value")).unwrap(), "ORIGINAL");
        // The link's target was taken as a path inside generation 2.
        let inside = top.join("gens/2").join(outside.strip_prefix("/").unwrap());
        assert_eq!(std::fs::read_to_string(inside.join("value")).unwrap(), "CHANGED");
    }

    #[test]
    fn a_symlink_cannot_alias_a_protected_location() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        std::os::unix::fs::symlink("/etc/crowbar", top.join("gens/1/etc/alias")).unwrap();
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();

        let text = file_state("/etc/alias/state.toml");
        let state = State::from_toml_str(&text).unwrap();
        let planned = e.plan(&state, &top.join("gens/1")).unwrap_err();
        assert!(planned.0.contains("protected location"), "{planned}");
        let applied = e.apply(&state, &text, 1).unwrap_err();
        assert!(applied.message.contains("/etc/crowbar/state.toml"), "{applied}");
        assert!(!top.join("gens/2").exists());
        assert_eq!(e.layout.read_default().unwrap(), Some(1));
    }

    #[test]
    fn a_ledger_failure_after_the_commit_point_does_not_undo_the_apply() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();
        // The ledger can no longer be appended to.
        std::fs::remove_file(e.layout.ledger_path()).unwrap();
        std::fs::create_dir(e.layout.ledger_path()).unwrap();

        let report = e.apply(&State::from_toml_str(GOOD).unwrap(), GOOD, 1).unwrap();
        assert_eq!(report.to, Some(2));
        assert_eq!(report.after_commit.len(), 1, "{:?}", report.after_commit);
        assert!(top.join("gens/2/etc/motd").exists());
        assert!(e.layout.meta_path(2).exists());
        assert_eq!(e.layout.read_default().unwrap(), Some(2));
        let cfg = std::fs::read_to_string(e.layout.grub_cfg()).unwrap();
        assert!(cfg.contains("set default=\"gen-2\""));

        assert_eq!(e.rollback(1, Some(2)).unwrap().len(), 1);
        assert_eq!(e.layout.read_default().unwrap(), Some(1));
    }

    #[test]
    fn a_failed_menu_render_is_undone_before_the_generation_is_deleted() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();
        let menu_before = std::fs::read_to_string(e.layout.grub_cfg()).unwrap();
        // The menu's temp file cannot be created, so the render fails after the
        // record and the default were already written.
        std::fs::create_dir_all(top.join("crowbar/.grub.cfg.crowbar-tmp/blocker")).unwrap();

        let failed = e.apply(&State::from_toml_str(GOOD).unwrap(), GOOD, 1).unwrap_err();
        assert!(!failed.left_behind, "{failed}");
        assert_eq!(e.layout.read_default().unwrap(), Some(1));
        assert!(!e.layout.meta_path(2).exists());
        assert!(!top.join("gens/2").exists());
        assert_eq!(std::fs::read_to_string(e.layout.grub_cfg()).unwrap(), menu_before);
        let last = ledger::read(&e.layout.ledger_path()).unwrap().entries.pop().unwrap();
        assert_eq!((last.action.as_str(), last.outcome.as_str()), ("apply", "failed"));

        assert!(!e.rollback(1, Some(1)).unwrap_err().left_behind);
        assert_eq!(e.layout.read_default().unwrap(), Some(1));
    }

    #[test]
    fn a_generation_the_menu_might_name_is_kept_and_its_id_is_not_reused() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();
        // The menu can be neither rewritten nor read back.
        std::fs::remove_file(e.layout.grub_cfg()).unwrap();
        std::fs::create_dir_all(e.layout.grub_cfg().join("blocker")).unwrap();

        let failed = e.apply(&State::from_toml_str(GOOD).unwrap(), GOOD, 1).unwrap_err();
        assert!(failed.message.contains("is kept"), "{failed}");
        assert!(failed.left_behind);
        assert_eq!(e.layout.read_default().unwrap(), Some(1));
        assert!(!e.layout.meta_path(2).exists());
        assert!(top.join("gens/2").is_dir());
        assert_eq!(e.layout.next_id().unwrap(), 3);
    }

    #[test]
    fn a_second_apply_or_rollback_fails_while_the_lock_is_held() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();

        let held = e.lock().unwrap();
        let blocked = e.apply(&State::from_toml_str(GOOD).unwrap(), GOOD, 1).unwrap_err();
        assert!(
            blocked.message.contains("another crowbar apply or rollback"),
            "{blocked}"
        );
        assert!(!top.join("gens/2").exists());
        assert!(e.rollback(1, None).is_err());
        drop(held);

        assert_eq!(
            e.apply(&State::from_toml_str(GOOD).unwrap(), GOOD, 1).unwrap().to,
            Some(2)
        );
    }

    #[test]
    fn a_failed_flush_after_the_menu_rename_does_not_undo_the_apply() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();
        // The rename of grub.cfg lands, then flushing its directory fails.
        let _fail = crate::testutil::fail_dir_sync_after(&e.layout.grub_cfg());

        let report = e.apply(&State::from_toml_str(GOOD).unwrap(), GOOD, 1).unwrap();
        assert_eq!(report.to, Some(2));
        assert_eq!(report.after_commit.len(), 1, "{:?}", report.after_commit);
        assert!(report.after_commit[0].contains("could not be flushed"));
        assert!(top.join("gens/2/etc/motd").exists());
        assert!(e.layout.meta_path(2).exists());
        assert_eq!(e.layout.read_default().unwrap(), Some(2));
        let cfg = std::fs::read_to_string(e.layout.grub_cfg()).unwrap();
        assert!(cfg.contains("set default=\"gen-2\""));
        let last = ledger::read(&e.layout.ledger_path()).unwrap().entries.pop().unwrap();
        assert_eq!((last.action.as_str(), last.outcome.as_str()), ("apply", "ok"));

        // The same for a rollback: it is in effect, with the problem reported.
        let problems = e.rollback(1, Some(2)).unwrap();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(e.layout.read_default().unwrap(), Some(1));
        let cfg = std::fs::read_to_string(e.layout.grub_cfg()).unwrap();
        assert!(cfg.contains("set default=\"gen-1\""));
    }

    #[test]
    fn a_withdrawal_rolls_forward_when_the_menu_on_disk_names_the_generation() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();
        e.apply(&State::from_toml_str(GOOD).unwrap(), GOOD, 1).unwrap();
        let generation = e.layout.get(2).unwrap();
        // As if the publish had reported a failure although the menu landed,
        // and the record was lost on the way.
        std::fs::remove_file(e.layout.meta_path(2)).unwrap();

        assert_eq!(e.unpublish(&generation, Some(1)), Ok(Withdrawal::Committed));
        assert_eq!(e.layout.get(2).unwrap(), generation);
        assert_eq!(e.layout.read_default().unwrap(), Some(2));
        assert!(top.join("gens/2").is_dir());

        // When the menu does not name it, the default and the record go back.
        let mut unpublished = generation.clone();
        unpublished.id = 3;
        e.layout.write_meta(&unpublished).unwrap();
        e.layout.write_default(3).unwrap();
        assert_eq!(e.unpublish(&unpublished, Some(2)), Ok(Withdrawal::Withdrawn));
        assert!(!e.layout.meta_path(3).exists());
        assert_eq!(e.layout.read_default().unwrap(), Some(2));
    }

    #[test]
    fn init_reports_a_ledger_failure_as_after_the_commit_and_withdraws_a_failed_publish() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);

        // The menu cannot be written: nothing of generation 1 is registered.
        std::fs::create_dir_all(top.join("crowbar/.grub.cfg.crowbar-tmp/blocker")).unwrap();
        let failed = e.init(BASE).unwrap_err();
        assert!(!failed.left_behind, "{failed}");
        assert!(!e.layout.meta_path(1).exists());
        assert_eq!(e.layout.read_default().unwrap(), None);
        std::fs::remove_dir_all(top.join("crowbar/.grub.cfg.crowbar-tmp")).unwrap();

        // The ledger cannot be written: generation 1 is registered all the same.
        std::fs::create_dir_all(e.layout.ledger_path()).unwrap();
        let (generation, problems) = e.init(BASE).unwrap();
        assert_eq!(generation.id, 1);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(e.layout.read_default().unwrap(), Some(1));
        assert!(e.layout.grub_cfg().exists());
    }

    /// A snapshotter that checks, when asked to flush a generation, that
    /// nothing yet names that generation.
    struct SyncRecorder<'a> {
        layout: Layout,
        synced: &'a RefCell<Vec<(std::path::PathBuf, bool)>>,
    }

    impl Snapshotter for SyncRecorder<'_> {
        fn snapshot(&self, source: &Path, dest: &Path) -> Result<()> {
            CopySnapshot.snapshot(source, dest)
        }
        fn delete(&self, path: &Path) -> Result<()> {
            CopySnapshot.delete(path)
        }
        fn sync(&self, path: &Path) -> Result<()> {
            let id: u32 = path.file_name().unwrap().to_str().unwrap().parse().unwrap();
            let menu = std::fs::read_to_string(self.layout.grub_cfg()).unwrap();
            let unpublished = !bootmenu::references(&menu, id)
                && !self.layout.meta_path(id).exists()
                && self.layout.read_default().unwrap() != Some(id);
            self.synced.borrow_mut().push((path.to_path_buf(), unpublished));
            Ok(())
        }
    }

    #[test]
    fn a_generation_is_flushed_to_disk_before_it_is_published() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let synced = RefCell::new(Vec::new());
        let pkgs = pkgs();
        let snap = SyncRecorder {
            layout: Layout::new(top),
            synced: &synced,
        };
        let e = engine_with(top, &pkgs, &snap);
        e.init(BASE).unwrap();

        assert_eq!(
            e.apply(&State::from_toml_str(GOOD).unwrap(), GOOD, 1).unwrap().to,
            Some(2)
        );
        assert_eq!(*synced.borrow(), vec![(top.join("gens/2"), true)]);
        // A no-op apply publishes nothing, so it flushes nothing.
        e.apply(&State::from_toml_str(GOOD).unwrap(), GOOD, 2).unwrap();
        assert_eq!(synced.borrow().len(), 1);
    }

    #[test]
    fn a_failed_flush_abandons_the_generation() {
        struct NoSync;
        impl Snapshotter for NoSync {
            fn snapshot(&self, source: &Path, dest: &Path) -> Result<()> {
                CopySnapshot.snapshot(source, dest)
            }
            fn delete(&self, path: &Path) -> Result<()> {
                CopySnapshot.delete(path)
            }
            fn sync(&self, _: &Path) -> Result<()> {
                err("disk is gone")
            }
        }
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let pkgs = pkgs();
        let e = engine_with(top, &pkgs, &NoSync);
        e.init(BASE).unwrap();

        let failed = e.apply(&State::from_toml_str(GOOD).unwrap(), GOOD, 1).unwrap_err();
        assert!(
            failed.message.contains("disk is gone") && !failed.left_behind,
            "{failed}"
        );
        assert!(!top.join("gens/2").exists());
        assert!(!e.layout.meta_path(2).exists());
        assert_eq!(e.layout.read_default().unwrap(), Some(1));
    }

    #[test]
    fn a_managed_path_is_never_read_through_a_final_symlink() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let root = top.join("gens/1");
        // The link's target already holds exactly the declared content.
        std::fs::write(root.join("etc/real"), "CHANGED").unwrap();
        std::os::unix::fs::symlink("/etc/real", root.join("etc/motd")).unwrap();
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();

        let text = file_state("/etc/motd");
        let state = State::from_toml_str(&text).unwrap();
        // The entry is a link, not the declared file, so it differs.
        let planned = e.plan(&state, &root).unwrap();
        assert!(
            planned
                .iter()
                .any(|c| matches!(c, Change::WriteFile { path, .. } if path == "/etc/motd")),
            "{planned:?}"
        );
        e.apply(&state, &text, 1).unwrap();
        let motd = top.join("gens/2/etc/motd");
        assert!(std::fs::symlink_metadata(&motd).unwrap().file_type().is_file());
        assert_eq!(std::fs::read_to_string(&motd).unwrap(), "CHANGED");
        assert!(e.plan(&state, &top.join("gens/2")).unwrap().is_empty());
    }

    #[test]
    fn a_managed_path_that_is_not_a_regular_file_is_not_opened() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let root = top.join("gens/1");
        // A socket and a directory. (A FIFO is the case that matters most,
        // because opening one blocks, but std cannot create one and a test that
        // spawns `mkfifo` would hand its lock file to the child for a moment.)
        let _socket = std::os::unix::net::UnixListener::bind(root.join("etc/motd")).unwrap();
        std::fs::create_dir(root.join("etc/issue")).unwrap();
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();

        let text = format!(
            "{}[[file]]\npath = \"/etc/issue\"\ncontent = \"x\"\n",
            file_state("/etc/motd")
        );
        let planned = e.plan(&State::from_toml_str(&text).unwrap(), &root).unwrap();
        let writes = planned.iter().filter(|c| matches!(c, Change::WriteFile { .. })).count();
        assert_eq!(writes, 2, "{planned:?}");
    }

    #[test]
    fn a_menu_title_that_quotes_an_entry_id_does_not_commit_a_failed_publish() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();
        // Generation 2's menu title holds the words a generation 3 entry has.
        let text = file_state("/etc/--id gen-3 x");
        e.apply(&State::from_toml_str(&text).unwrap(), &text, 1).unwrap();
        let menu_before = std::fs::read_to_string(e.layout.grub_cfg()).unwrap();
        assert!(menu_before.contains("--id gen-3 x"), "{menu_before}");
        // Generation 3's menu cannot be written.
        std::fs::create_dir_all(top.join("crowbar/.grub.cfg.crowbar-tmp/blocker")).unwrap();

        let next = format!("{text}[[file]]\npath = \"/etc/issue\"\ncontent = \"x\"\n");
        let failed = e.apply(&State::from_toml_str(&next).unwrap(), &next, 2).unwrap_err();
        assert!(!failed.left_behind, "{failed}");
        assert_eq!(e.layout.read_default().unwrap(), Some(2));
        assert!(!e.layout.meta_path(3).exists());
        assert!(!top.join("gens/3").exists());
        assert_eq!(std::fs::read_to_string(e.layout.grub_cfg()).unwrap(), menu_before);
    }

    #[test]
    fn the_hostname_fstab_and_recorded_state_are_never_read_through_a_symlink() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let root = top.join("gens/1");
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();
        let state = State::from_toml_str(BASE).unwrap();
        assert!(e.plan(&state, &root).unwrap().is_empty());

        // A link to a file with the right hostname is still not the hostname file.
        std::fs::rename(root.join("etc/hostname"), root.join("etc/hostname.real")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname.real", root.join("etc/hostname")).unwrap();
        let planned = e.plan(&state, &root).unwrap();
        assert!(
            matches!(planned.as_slice(), [Change::SetHostname { from: None, .. }]),
            "{planned:?}"
        );
        std::fs::remove_file(root.join("etc/hostname")).unwrap();
        std::fs::rename(root.join("etc/hostname.real"), root.join("etc/hostname")).unwrap();

        // fstab and the recorded State are refused, not read, when they are not
        // regular files. (A FIFO is the case that hangs; see the note above.)
        for path in ["etc/fstab", "etc/crowbar/state.toml"] {
            let real = root.join(format!("{path}.real"));
            std::fs::rename(root.join(path), &real).unwrap();
            std::os::unix::fs::symlink(&real, root.join(path)).unwrap();
            let refused = e.plan(&state, &root).unwrap_err();
            assert!(refused.0.contains("not a regular text file"), "{path}: {refused}");
            std::fs::remove_file(root.join(path)).unwrap();
            let _socket = std::os::unix::net::UnixListener::bind(root.join(path)).unwrap();
            let refused = e.plan(&state, &root).unwrap_err();
            assert!(refused.0.contains("not a regular text file"), "{path}: {refused}");
            std::fs::remove_file(root.join(path)).unwrap();
            std::fs::rename(&real, root.join(path)).unwrap();
        }
        assert!(e.plan(&state, &root).unwrap().is_empty());
    }

    #[test]
    fn the_menu_is_not_rendered_from_a_record_with_a_bad_boot_path() {
        let dir = scratch();
        let top = &dir.0;
        base_root(top);
        let (pkgs, snap) = (pkgs(), CopySnapshot);
        let e = engine(top, &pkgs, &snap);
        e.init(BASE).unwrap();
        let menu_before = std::fs::read_to_string(e.layout.grub_cfg()).unwrap();
        let mut record = e.layout.get(1).unwrap();
        record.kernel = "/gens/1/boot/vmlinuz-6\n}\nmenuentry \"pwn\" {".into();
        e.layout.write_meta(&record).unwrap();

        let refused = e.render_menu().unwrap_err();
        assert!(refused.0.contains("cannot go into the boot menu"), "{refused}");
        assert_eq!(std::fs::read_to_string(e.layout.grub_cfg()).unwrap(), menu_before);
    }
}
