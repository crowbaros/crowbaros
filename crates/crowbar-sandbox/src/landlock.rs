//! Landlock: the second check, inside the namespaces.
//!
//! The mount namespace decides what a compartment can see. Landlock states
//! the same view again as rules the kernel checks on each use, so a mistake
//! in building the mounts does not open the machine's files. It also
//! refuses every change to the mounts, which matters for a compartment
//! that may make user namespaces, and, where the kernel can:
//!
//! - binding or connecting a TCP socket (Landlock ABI 4);
//! - connecting to an abstract Unix socket of a process outside the
//!   compartment, and signalling one (ABI 6).
//!
//! A kernel with no Landlock starts no compartment.

use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::order::Access;
use crate::sys::checked;
use crate::wire::InitOrder;

const EXECUTE: u64 = 1 << 0;
const WRITE_FILE: u64 = 1 << 1;
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
/// Bits 4 to 12: removing, and making each kind of file.
const CHANGE_DIR: u64 = 0x1ff << 4;
/// Moving or linking a file to another directory. ABI 2.
const REFER: u64 = 1 << 13;
/// ABI 3.
const TRUNCATE: u64 = 1 << 14;
/// ABI 5.
const IOCTL_DEV: u64 = 1 << 15;

/// The rights that apply to a file that is not a directory.
const ON_A_FILE: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE | IOCTL_DEV;

const READ: u64 = READ_FILE | READ_DIR;
const READ_AND_RUN: u64 = READ | EXECUTE;
const EVERYTHING: u64 = READ_AND_RUN | WRITE_FILE | CHANGE_DIR | REFER | TRUNCATE | IOCTL_DEV;

const NET_BIND_TCP: u64 = 1 << 0;
const NET_CONNECT_TCP: u64 = 1 << 1;
const SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
const SCOPE_SIGNAL: u64 = 1 << 1;

const CREATE_RULESET_VERSION: u32 = 1;
const RULE_PATH_BENEATH: u32 = 1;

/// `struct landlock_ruleset_attr`. A kernel that knows fewer fields accepts
/// the longer structure as long as the fields it does not know are zero.
#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

/// `struct landlock_path_beneath_attr`.
#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: RawFd,
}

/// Confines this process, and every process it starts, to the view of
/// `order`. It must already be unable to gain privileges.
pub(crate) fn confine(order: &InitOrder) -> io::Result<()> {
    // SAFETY: with no attribute, the call only reports the ABI version.
    let abi = checked(unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<RulesetAttr>(),
            0,
            CREATE_RULESET_VERSION,
        )
    })
    .map_err(|e| io::Error::new(e.kind(), format!("this kernel has no Landlock: {e}")))?;

    // The rights this kernel knows. A right it does not know cannot be
    // refused by it, and is not named.
    let mut known = READ_AND_RUN | WRITE_FILE | CHANGE_DIR;
    for (since, right) in [(2, REFER), (3, TRUNCATE), (5, IOCTL_DEV)] {
        if abi >= since {
            known |= right;
        }
    }
    let attr = RulesetAttr {
        handled_access_fs: known,
        handled_access_net: if abi >= 4 { NET_BIND_TCP | NET_CONNECT_TCP } else { 0 },
        scoped: if abi >= 6 {
            SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL
        } else {
            0
        },
    };
    // SAFETY: `attr` is a ruleset attribute of the size given.
    let ruleset = checked(unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            &attr as *const RulesetAttr,
            size_of::<RulesetAttr>(),
            0,
        )
    })?;
    // SAFETY: the call succeeded, so `ruleset` is a new descriptor.
    let ruleset = unsafe { OwnedFd::from_raw_fd(ruleset as RawFd) };

    let allow = |path: &Path, rights: u64| -> io::Result<()> {
        let place = File::options()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(path)?;
        let rights = if place.metadata()?.is_dir() {
            rights
        } else {
            rights & ON_A_FILE
        };
        let rule = PathBeneathAttr {
            allowed_access: rights & known,
            parent_fd: place.as_raw_fd(),
        };
        // SAFETY: `rule` is a path rule, and both descriptors are open.
        checked(unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset.as_raw_fd(),
                RULE_PATH_BENEATH,
                &rule as *const PathBeneathAttr,
                0,
            )
        })
        .map(drop)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
    };
    // Every directory may be listed: all of them are the sandbox's own or
    // in the view. Nothing else is allowed unless a rule below names it.
    allow(Path::new("/"), READ_DIR)?;
    allow(Path::new("/proc"), READ | WRITE_FILE)?;
    allow(Path::new("/dev"), READ | WRITE_FILE)?;
    allow(Path::new("/dev/shm"), EVERYTHING & !EXECUTE)?;
    allow(Path::new("/tmp"), EVERYTHING)?;
    for entry in &order.view {
        let rights = match entry.access {
            Access::ReadOnly => READ_AND_RUN,
            Access::ReadWrite => EVERYTHING,
        };
        allow(&entry.at, rights)?;
    }
    // SAFETY: `ruleset` is an open ruleset.
    checked(unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset.as_raw_fd(), 0) })?;
    Ok(())
}
