//! Resolve in-system paths inside a root, the way `chroot` would.
//!
//! A generation is a full root, so it holds symlinks that are absolute in
//! *its* namespace (`/etc/sv/sshd/supervise -> /run/runit/supervise.sshd`).
//! Joining such a path onto the generation directory and handing it to the
//! kernel resolves the link against the host root, which is the running
//! system. Every engine read and write goes through this module instead.
//!
//! The walk is done in userspace, one component at a time:
//!
//! - an absolute symlink target restarts at the root being resolved in;
//! - `..` stops at that root;
//! - more than [`MAX_SYMLINKS`] links is an error (a loop).
//!
//! What this does not defend against: something else changing the tree
//! between the walk and the use of the result (swapping a directory for a
//! symlink). The engine builds a generation that nothing else runs in or
//! writes to, and holds the apply lock while it does. A root with a
//! concurrent, hostile writer needs `openat2(RESOLVE_IN_ROOT)` instead.

use crate::{Result, err};
use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// The same limit the kernel uses for one path lookup.
pub const MAX_SYMLINKS: u32 = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InRoot {
    /// Where the path is on the host: the root plus the resolved in-system path.
    pub host: PathBuf,
    /// The resolved in-system path, for example `/run/runit/supervise.sshd/x`.
    pub system: String,
}

/// Resolve `absolute` inside `root`, following every symlink. Use this to read
/// a file or to name a directory.
pub fn in_root(root: &Path, absolute: &str) -> Result<InRoot> {
    resolve(root, absolute, true)
}

/// Resolve the directory part of `absolute` inside `root` and leave the last
/// component alone. Use this to replace, create, or remove the entry itself:
/// a symlink at that name is replaced or removed, never written through.
pub fn entry_in_root(root: &Path, absolute: &str) -> Result<InRoot> {
    resolve(root, absolute, false)
}

fn parts(path: &Path) -> impl DoubleEndedIterator<Item = OsString> + '_ {
    path.components().filter_map(|c| match c {
        Component::Normal(name) => Some(name.to_os_string()),
        Component::ParentDir => Some(OsString::from("..")),
        Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
    })
}

fn resolve(root: &Path, absolute: &str, follow_last: bool) -> Result<InRoot> {
    let mut pending: VecDeque<OsString> = parts(Path::new(absolute)).collect();
    let mut resolved: Vec<OsString> = Vec::new();
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        if part == ".." {
            resolved.pop();
            continue;
        }
        let mut host = root.to_path_buf();
        host.extend(&resolved);
        host.push(&part);
        let is_link = std::fs::symlink_metadata(&host).is_ok_and(|m| m.file_type().is_symlink());
        if is_link && (follow_last || !pending.is_empty()) {
            links += 1;
            if links > MAX_SYMLINKS {
                return err(format!(
                    "{absolute}: too many levels of symbolic links under {}",
                    root.display()
                ));
            }
            let target = std::fs::read_link(&host)?;
            if target.is_absolute() {
                resolved.clear();
            }
            for part in parts(&target).rev() {
                pending.push_front(part);
            }
        } else {
            // Not a link, or missing. A missing component has nothing under it,
            // so the rest of the path is taken as written.
            resolved.push(part);
        }
    }
    let mut host = root.to_path_buf();
    host.extend(&resolved);
    let system = resolved
        .iter()
        .map(|part| format!("/{}", part.to_string_lossy()))
        .collect::<String>();
    Ok(InRoot {
        host,
        system: if system.is_empty() { "/".into() } else { system },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;
    use std::os::unix::fs::symlink;

    #[test]
    fn absolute_symlinks_are_rerooted() {
        let dir = scratch();
        let root = dir.0.join("root");
        let outside = dir.0.join("outside");
        std::fs::create_dir_all(root.join("etc")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("etc/alias")).unwrap();

        let found = in_root(&root, "/etc/alias/value").unwrap();
        assert!(found.host.starts_with(&root), "{found:?}");
        assert_eq!(found.system, format!("{}/value", outside.display()));
    }

    #[test]
    fn dotdot_cannot_climb_above_the_root() {
        let dir = scratch();
        let root = dir.0.join("root");
        std::fs::create_dir_all(root.join("etc")).unwrap();
        symlink("../../../../outside", root.join("etc/up")).unwrap();

        let found = in_root(&root, "/etc/up/value").unwrap();
        assert_eq!(found.host, root.join("outside/value"));
        assert_eq!(found.system, "/outside/value");
    }

    #[test]
    fn relative_links_and_the_last_component() {
        let dir = scratch();
        let root = dir.0.join("root");
        std::fs::create_dir_all(root.join("usr/bin")).unwrap();
        symlink("usr/bin", root.join("bin")).unwrap();
        symlink("/usr/bin/real", root.join("usr/bin/link")).unwrap();

        assert_eq!(in_root(&root, "/bin/tool").unwrap().system, "/usr/bin/tool");
        // Reading follows the last link; naming the entry does not.
        assert_eq!(in_root(&root, "/bin/link").unwrap().system, "/usr/bin/real");
        assert_eq!(entry_in_root(&root, "/bin/link").unwrap().system, "/usr/bin/link");
        assert_eq!(in_root(&root, "/").unwrap().host, root);
    }

    #[test]
    fn a_symlink_loop_is_an_error() {
        let dir = scratch();
        let root = dir.0.join("root");
        std::fs::create_dir_all(&root).unwrap();
        symlink("/b", root.join("a")).unwrap();
        symlink("/a", root.join("b")).unwrap();

        let e = in_root(&root, "/a/value").unwrap_err();
        assert!(e.0.contains("too many levels"), "{e}");
    }
}
