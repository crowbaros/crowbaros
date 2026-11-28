//! Generic, harness-agnostic Linux `inotify` watcher for harness session stores.
//!
//! Each harness's discovery adapter declares one or more [`WatchSpec`]s: a
//! directory on disk where its store lives and an optional list of filenames to
//! react to. Because a harness may never have run yet when Lever starts at boot,
//! [`StoreWatcher`] watches the deepest existing ancestor of any missing
//! directory and automatically promotes the watch as directories are created (or
//! re-arms on an ancestor if a watched directory is removed).

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// One directory a harness stores sessions in, and optionally which filenames
/// inside it signal a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchSpec {
    /// Target directory (may not exist yet when Lever starts).
    pub dir: PathBuf,
    /// If non-empty, only events for entries whose filename equals one of these
    /// strings trigger a change. If empty, any file event in `dir` triggers a
    /// change.
    pub filenames: Vec<String>,
}

impl WatchSpec {
    pub fn directory(dir: PathBuf) -> Self {
        Self {
            dir,
            filenames: Vec::new(),
        }
    }

    pub fn files(dir: PathBuf, filenames: Vec<String>) -> Self {
        Self { dir, filenames }
    }

    fn matches_name(&self, name: &str) -> bool {
        self.filenames.is_empty() || self.filenames.iter().any(|wanted| wanted == name)
    }
}

struct TargetState {
    spec: WatchSpec,
    /// Currently watched path (either `spec.dir` or its deepest existing ancestor).
    watched_path: Option<PathBuf>,
    /// True when `watched_path == Some(spec.dir)`.
    at_target: bool,
    wd: Option<libc::c_int>,
}

/// Non-blocking Linux `inotify` watcher usable directly in `poll(2)`.
pub struct StoreWatcher {
    fd: OwnedFd,
    targets: Vec<TargetState>,
    /// Maps an active `inotify` watch descriptor to the target indices using it.
    by_wd: HashMap<libc::c_int, Vec<usize>>,
}

/// What changes a store. Closing a file written to is not watched: every
/// write was told as it was made, and a process that only opened its store
/// to read it, as a one-shot listing does, closes it as it ends.
const TARGET_MASK: u32 =
    libc::IN_MODIFY | libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_DELETE | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF;

const ANCESTOR_MASK: u32 = libc::IN_CREATE | libc::IN_MOVED_TO | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF;

impl StoreWatcher {
    /// Creates an `inotify` watcher for `specs`. Returns `Ok(None)` when `specs`
    /// is empty so Lever allocates no descriptor when nothing is watched.
    pub fn new(specs: Vec<WatchSpec>) -> io::Result<Option<Self>> {
        if specs.is_empty() {
            return Ok(None);
        }
        // SAFETY: `inotify_init1` takes only flag bits and returns a new fd or -1.
        let raw = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a valid owned file descriptor from `inotify_init1`.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let targets = specs
            .into_iter()
            .map(|spec| TargetState {
                spec,
                watched_path: None,
                at_target: false,
                wd: None,
            })
            .collect();
        let mut watcher = Self {
            fd,
            targets,
            by_wd: HashMap::new(),
        };
        let _ = watcher.rearm_all();
        Ok(Some(watcher))
    }

    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Drains all pending `inotify` events and returns `true` if any watched
    /// store changed (or a watched directory was newly created).
    pub fn drain_changed(&mut self) -> bool {
        let mut buf = [0u8; 8192];
        let mut changed = false;
        let mut needs_rearm = false;

        loop {
            // SAFETY: reading into a properly aligned byte buffer from an owned non-blocking inotify fd.
            let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if n == 0 {
                break;
            }
            let n = n as usize;
            let mut offset = 0;
            while offset + size_of::<libc::inotify_event>() <= n {
                // SAFETY: `read_unaligned` safely reads the `inotify_event` header from `buf`.
                let event = unsafe { std::ptr::read_unaligned(buf.as_ptr().add(offset).cast::<libc::inotify_event>()) };
                let header_len = size_of::<libc::inotify_event>();
                let name_len = event.len as usize;
                let end = offset.saturating_add(header_len).saturating_add(name_len).min(n);
                let name = if name_len > 0 && offset + header_len <= end {
                    let raw_name = &buf[offset + header_len..end];
                    let nul = raw_name.iter().position(|&b| b == 0).unwrap_or(raw_name.len());
                    std::str::from_utf8(&raw_name[..nul]).ok()
                } else {
                    None
                };

                if event.mask & libc::IN_Q_OVERFLOW != 0 {
                    changed = true;
                    needs_rearm = true;
                } else if let Some(indices) = self.by_wd.get(&event.wd) {
                    for &idx in indices {
                        let target = &self.targets[idx];
                        if event.mask & (libc::IN_DELETE_SELF | libc::IN_MOVE_SELF | libc::IN_IGNORED) != 0 {
                            needs_rearm = true;
                            if target.at_target {
                                changed = true;
                            }
                            continue;
                        }
                        if target.at_target {
                            if let Some(name) = name {
                                if target.spec.matches_name(name) {
                                    changed = true;
                                }
                            } else {
                                changed = true;
                            }
                        } else {
                            // Ancestor watch: a directory may have appeared along the path.
                            needs_rearm = true;
                        }
                    }
                }

                offset = offset.saturating_add(header_len).saturating_add(name_len);
            }
        }

        if needs_rearm && self.rearm_all() {
            changed = true;
        }
        changed
    }

    /// Ensures every target watches either its exact directory or the deepest
    /// existing ancestor. Returns `true` if any target promoted to its exact
    /// directory (which may already contain newly-created store files).
    ///
    /// Loops until the deepest existing directory after `inotify_add_watch`
    /// matches what was watched, closing the TOCTOU window where a nested
    /// child directory is created between `deepest_existing_dir` and
    /// `inotify_add_watch` on its parent.
    fn rearm_all(&mut self) -> bool {
        let mut promoted = false;
        for _ in 0..16 {
            let mut desired: Vec<(usize, PathBuf, bool)> = Vec::with_capacity(self.targets.len());
            for (idx, target) in self.targets.iter().enumerate() {
                if let Some((path, at_target)) = deepest_existing_dir(&target.spec.dir) {
                    desired.push((idx, path, at_target));
                }
            }

            // Collect watch masks per path; a single directory may back multiple targets.
            let mut new_wds: HashMap<PathBuf, (libc::c_int, u32)> = HashMap::new();
            for &(idx, ref path, at_target) in &desired {
                let mask = if at_target { TARGET_MASK } else { ANCESTOR_MASK };
                let entry = new_wds.entry(path.clone()).or_insert((-1, 0));
                entry.1 |= mask;
                if at_target && !self.targets[idx].at_target {
                    promoted = true;
                }
            }

            // Collect currently active wds before adding/updating.
            let old_wds: Vec<libc::c_int> = self.by_wd.keys().copied().collect();
            let mut active_wds: HashMap<libc::c_int, Vec<usize>> = HashMap::new();

            for (path, (wd_slot, mask)) in &mut new_wds {
                if let Some(wd) = add_watch(self.fd.as_raw_fd(), path, *mask) {
                    *wd_slot = wd;
                }
            }

            for target in &mut self.targets {
                target.watched_path = None;
                target.at_target = false;
                target.wd = None;
            }

            for (idx, path, at_target) in desired {
                if let Some(&(wd, _)) = new_wds.get(&path)
                    && wd >= 0
                {
                    let target = &mut self.targets[idx];
                    target.watched_path = Some(path);
                    target.at_target = at_target;
                    target.wd = Some(wd);
                    active_wds.entry(wd).or_default().push(idx);
                }
            }

            for old_wd in old_wds {
                if !active_wds.contains_key(&old_wd) {
                    // SAFETY: removing an existing watch descriptor from our inotify fd.
                    unsafe {
                        let _ = libc::inotify_rm_watch(self.fd.as_raw_fd(), old_wd);
                    }
                }
            }

            self.by_wd = active_wds;

            // Re-check after installing watches: if a deeper directory appeared
            // while we were attaching a parent watch, promote immediately.
            let settled = self.targets.iter().all(|t| {
                let current = deepest_existing_dir(&t.spec.dir).map(|(p, _)| p);
                current == t.watched_path
            });
            if settled {
                break;
            }
        }
        promoted
    }
}

fn deepest_existing_dir(target: &Path) -> Option<(PathBuf, bool)> {
    let mut cur = target;
    let mut exact = true;
    loop {
        if cur.is_dir() {
            return Some((cur.to_path_buf(), exact));
        }
        exact = false;
        cur = cur.parent()?;
    }
}

fn add_watch(inotify_fd: RawFd, path: &Path, mask: u32) -> Option<libc::c_int> {
    let bytes = path.as_os_str().as_bytes();
    let mut nul_terminated = Vec::with_capacity(bytes.len() + 1);
    nul_terminated.extend_from_slice(bytes);
    nul_terminated.push(0);
    // SAFETY: `nul_terminated` is a valid NUL-terminated C path string.
    let wd = unsafe { libc::inotify_add_watch(inotify_fd, nul_terminated.as_ptr().cast(), mask) };
    (wd >= 0).then_some(wd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "crowbar-lever-watcher-{tag}-{}-{}",
            std::process::id(),
            sys::random_hex(4)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn watches_existing_directory_and_filters_by_filename() {
        let dir = temp_dir("filter");
        let spec = WatchSpec::files(dir.clone(), vec!["store.db".to_string(), "store.db-wal".to_string()]);
        let mut watcher = StoreWatcher::new(vec![spec]).unwrap().unwrap();
        assert!(!watcher.drain_changed());

        std::fs::write(dir.join("unrelated.log"), "noise").unwrap();
        assert!(!watcher.drain_changed(), "unrelated files must not trigger a change");

        std::fs::write(dir.join("store.db-wal"), "frame").unwrap();
        assert!(watcher.drain_changed(), "matching filename must trigger a change");
        assert!(!watcher.drain_changed(), "drain clears pending events");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn promotes_watch_when_missing_nested_directory_appears_and_rearms_after_removal() {
        let root = temp_dir("promote");
        let nested = root.join("share/harness");
        let spec = WatchSpec::files(nested.clone(), vec!["sessions.db".to_string()]);
        let mut watcher = StoreWatcher::new(vec![spec]).unwrap().unwrap();
        assert!(!watcher.drain_changed());

        // Create intermediate directory, then target directory with file.
        std::fs::create_dir_all(&nested).unwrap();
        assert!(
            watcher.drain_changed(),
            "creating the watched target directory must promote the watch and signal change"
        );

        std::fs::write(nested.join("sessions.db"), "v1").unwrap();
        assert!(watcher.drain_changed());

        // Delete the directory and recreate it later (as tests or resets do).
        // The kernel tells of a directory's own removal only once nothing
        // holds it open, and a process another test forks meanwhile holds a
        // copy of every descriptor until it runs its program: wait for it.
        let held = std::fs::File::open(&nested).unwrap();
        std::fs::remove_dir_all(&nested).unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(held);
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while watcher.targets[0].at_target && std::time::Instant::now() < deadline {
            let _ = watcher.drain_changed();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(
            !watcher.targets[0].at_target,
            "the removed directory's watch must move to its parent"
        );

        std::fs::create_dir_all(&nested).unwrap();
        assert!(watcher.drain_changed());
        std::fs::write(nested.join("sessions.db"), "v2").unwrap();
        assert!(watcher.drain_changed());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn concurrent_nested_directory_creation_never_misses_promotion() {
        for round in 0..25 {
            let root = temp_dir(&format!("race-{round}"));
            let nested = root.join("a/b/c/store");
            let spec = WatchSpec::directory(nested.clone());
            let mut watcher = StoreWatcher::new(vec![spec]).unwrap().unwrap();
            let target = nested.clone();
            let creator = std::thread::spawn(move || {
                std::fs::create_dir_all(&target).unwrap();
                std::fs::write(target.join("sessions.json"), "[]").unwrap();
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            let mut saw_change = false;
            while std::time::Instant::now() < deadline {
                if watcher.drain_changed() {
                    saw_change = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            creator.join().unwrap();
            assert!(saw_change, "round {round} missed promotion during concurrent creation");
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}
