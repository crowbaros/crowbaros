//! The init stage: the code that builds one sandbox and runs the program in
//! it.
//!
//! The Warden runs this as a process of its own, `crowbar-warden init`, so
//! that it has one thread and nothing of the Warden's but the order. It
//! starts as root, inside the compartment's cgroup, and reads its order
//! from its standard input, a socket. On that socket it reports the step
//! that failed. When the program starts, the socket closes with nothing
//! said, and that is how the Warden knows the start worked.

use std::convert::Infallible;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::path::Path;

use crate::order::Access;
use crate::sys::{self, checked, cstring};
use crate::wire::InitOrder;
use crate::{landlock, seccomp};

/// A step of the start, or the text that says which one failed.
type Step<T> = Result<T, String>;

/// Names the step an error came from.
fn at<T>(step: impl std::fmt::Display, result: io::Result<T>) -> Step<T> {
    result.map_err(|e| format!("{step}: {e}"))
}

/// The exit code of an init stage that could not start the program.
const NOT_STARTED: i32 = 125;

/// Runs the init stage. Never returns: the process becomes the program, or
/// exits.
#[doc(hidden)]
pub fn init_stage() -> ! {
    // SAFETY: standard input is open, and the call makes a new descriptor.
    let mut status = unsafe { libc::fcntl(0, libc::F_DUPFD_CLOEXEC, 64) };
    if status != -1 {
        let failure = match build(&mut status) {
            Ok(never) => match never {},
            Err(failure) => failure,
        };
        // SAFETY: `status` is the open status socket, and `failure` is
        // readable for its whole length.
        unsafe { libc::write(status, failure.as_ptr().cast(), failure.len()) };
    }
    std::process::exit(NOT_STARTED)
}

/// Builds the sandbox and becomes the program. `status` is the socket the
/// Warden listens on, wherever it has been moved to.
fn build(status: &mut RawFd) -> Step<Infallible> {
    // SAFETY: plain call. Files this stage makes get the mode it asks for,
    // less write access for others.
    unsafe { libc::umask(0o022) };
    // SAFETY: `status` is open for the whole of this function.
    let (order, connections) = receive(unsafe { BorrowedFd::borrow_raw(*status) })?;

    // The view is taken while the machine's files can still be seen.
    let mapping = id_mapping(&order)?;
    let view: Vec<OwnedFd> = order
        .view
        .iter()
        .map(|entry| {
            at(
                format_args!("the view entry {}", entry.source.display()),
                view_mount(&entry.source, entry.access, &mapping),
            )
        })
        .collect::<Step<_>>()?;
    drop(mapping);

    // Namespaces of its own for mounts, processes, System V and POSIX IPC,
    // the network, the host name, and cgroups. The new network namespace
    // has a loopback interface that is down, and no other.
    let namespaces = libc::CLONE_NEWNS
        | libc::CLONE_NEWPID
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWNET
        | libc::CLONE_NEWUTS
        | libc::CLONE_NEWCGROUP;
    // SAFETY: the call changes only which namespaces this process is in.
    at("new namespaces", checked(unsafe { libc::unshare(namespaces) }))?;
    // The process namespace begins with the next child, which is its first
    // process. This process stays outside and passes on how that one ends.
    // SAFETY: this process has one thread, so the child may run any code.
    let first = at("the sandbox's first process", checked(unsafe { libc::fork() }))?;
    if first != 0 {
        pass_on(first, 0);
    }
    // SAFETY: plain calls on this process alone. A session of its own has
    // no terminal, and the host name says nothing about the machine.
    at("a new session", checked(unsafe { libc::setsid() }))?;
    at(
        "the host name",
        checked(unsafe { libc::sethostname(HOST_NAME.as_ptr().cast(), HOST_NAME.len()) }),
    )?;
    at("the sandbox's root", root(&order, view))?;
    *status = at("the compartment's descriptors", descriptors(connections, *status))?;
    at("the compartment's user", become_user(&order))?;
    at("the second check on files", landlock::confine(&order))?;
    at("the filter on system calls", seccomp::confine(&order.needs))?;

    // The program is the second process. The first stays as its parent,
    // takes in whatever the program leaves behind, and ends when the
    // program does. Then the kernel ends every process still in here.
    // SAFETY: as above.
    let program = at("the program's process", checked(unsafe { libc::fork() }))?;
    if program != 0 {
        pass_on(program, 3);
    }
    Err(format!("{}: {}", order.program.display(), run(&order)))
}

/// The host name every sandbox has.
const HOST_NAME: &str = "compartment";

/// Closes every descriptor from `keep` up, waits for `child` to end, and
/// ends the same way: with its exit code, or 128 plus the number of the
/// signal that ended it.
fn pass_on(child: libc::pid_t, keep: RawFd) -> ! {
    // SAFETY: plain calls. Nothing after this uses a descriptor from `keep`
    // up, and `state` is writable. Children that are not `child` are ones
    // the program left behind, and waiting takes them in too.
    unsafe {
        libc::syscall(libc::SYS_close_range, keep, u32::MAX, 0);
        loop {
            let mut state = 0;
            let ended = libc::waitpid(-1, &mut state, 0);
            if ended == child {
                libc::_exit(match libc::WIFEXITED(state) {
                    true => libc::WEXITSTATUS(state),
                    false => 128 + libc::WTERMSIG(state),
                });
            }
            if ended == -1 && *libc::__errno_location() != libc::EINTR {
                libc::_exit(NOT_STARTED);
            }
        }
    }
}

/// Reads the order and the broker connections from the Warden.
fn receive(socket: BorrowedFd<'_>) -> Step<(InitOrder, Vec<OwnedFd>)> {
    let mut length = [0u8; 8];
    let (read, connections) = at("the order", sys::receive_with_fds(socket, &mut length))?;
    if read != length.len() {
        return Err("the order: cut short".to_string());
    }
    let mut body = vec![0u8; u64::from_le_bytes(length) as usize];
    // SAFETY: the socket stays open, and `ManuallyDrop` keeps this second
    // owner from closing it.
    let mut stream = std::mem::ManuallyDrop::new(unsafe { File::from_raw_fd(socket.as_raw_fd()) });
    at("the order", stream.read_exact(&mut body))?;
    let order = InitOrder::decode(&body).ok_or("the order: not in the Warden's format")?;
    Ok((order, connections))
}

/// A user namespace that maps the storage user to the compartment's user,
/// and the storage group to its group. A mount that is given this mapping
/// shows the storage user's files as the compartment's own, and writes the
/// compartment's files to disk as the storage user's. Every other owner is
/// shown as nobody.
fn id_mapping(order: &InitOrder) -> Step<OwnedFd> {
    // SAFETY: this process has one thread, so the child may run any code.
    // It makes the namespace, stops, and is never continued.
    let child = at("a process for the ID mapping", checked(unsafe { libc::fork() }))?;
    if child == 0 {
        // SAFETY: plain calls on this process alone.
        unsafe {
            if libc::unshare(libc::CLONE_NEWUSER) == 0 {
                libc::raise(libc::SIGSTOP);
            }
            libc::_exit(1);
        }
    }
    let made = (|| {
        let mut state = 0;
        // SAFETY: `child` is this process's child, and `state` is writable.
        checked(unsafe { libc::waitpid(child, &mut state, libc::WUNTRACED) })?;
        if !libc::WIFSTOPPED(state) {
            return Err(io::Error::other("a user namespace was refused"));
        }
        let proc = Path::new("/proc").join(child.to_string());
        fs::write(proc.join("uid_map"), format!("{} {} 1", order.storage_uid, order.uid))?;
        fs::write(proc.join("gid_map"), format!("{} {} 1", order.storage_gid, order.gid))?;
        Ok(OwnedFd::from(File::open(proc.join("ns/user"))?))
    })();
    // SAFETY: `child` is still this process's child: it was not reaped.
    unsafe {
        libc::kill(child, libc::SIGKILL);
        libc::waitpid(child, std::ptr::null_mut(), 0);
    }
    at("the ID mapping", made)
}

/// The argument of `mount_setattr`.
#[repr(C)]
struct MountAttr {
    attr_set: u64,
    attr_clr: u64,
    propagation: u64,
    userns_fd: u64,
}

const OPEN_TREE_CLONE: libc::c_uint = 1;
const MOUNT_ATTR_RDONLY: u64 = 0x1;
const MOUNT_ATTR_NOSUID: u64 = 0x2;
const MOUNT_ATTR_NODEV: u64 = 0x4;
const MOUNT_ATTR_IDMAP: u64 = 0x0010_0000;
const MOVE_MOUNT_F_EMPTY_PATH: libc::c_uint = 0x4;

/// A mount of `source` alone, attached nowhere yet: the file or directory,
/// without whatever else is mounted inside it. It never honours a setuid
/// bit or a device file, and it is ID-mapped.
fn view_mount(source: &Path, access: Access, mapping: &OwnedFd) -> io::Result<OwnedFd> {
    let path = cstring(source)?;
    // SAFETY: `path` is a C string, and the call returns a new descriptor.
    let tree = checked(unsafe {
        libc::syscall(
            libc::SYS_open_tree,
            libc::AT_FDCWD,
            path.as_ptr(),
            OPEN_TREE_CLONE | libc::O_CLOEXEC as libc::c_uint,
        )
    })?;
    // SAFETY: the call succeeded, so `tree` is a new descriptor.
    let tree = unsafe { OwnedFd::from_raw_fd(tree as RawFd) };
    let read_only = match access {
        Access::ReadOnly => MOUNT_ATTR_RDONLY,
        Access::ReadWrite => 0,
    };
    let attr = MountAttr {
        attr_set: MOUNT_ATTR_NOSUID | MOUNT_ATTR_NODEV | MOUNT_ATTR_IDMAP | read_only,
        attr_clr: 0,
        propagation: libc::MS_PRIVATE,
        userns_fd: mapping.as_raw_fd() as u64,
    };
    // SAFETY: `attr` is a `mount_attr` of the size given, alive for the call.
    checked(unsafe {
        libc::syscall(
            libc::SYS_mount_setattr,
            tree.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            &attr as *const MountAttr,
            size_of::<MountAttr>(),
        )
    })?;
    Ok(tree)
}

fn mount(source: &str, target: &Path, kind: Option<&str>, flags: libc::c_ulong, options: &str) -> io::Result<()> {
    let source = CString::new(source)?;
    let target_path = cstring(target)?;
    let kind = kind.map(CString::new).transpose()?;
    let options = CString::new(options)?;
    // SAFETY: every pointer is a C string that lives until the call returns,
    // or null where the call allows it.
    checked(unsafe {
        libc::mount(
            source.as_ptr(),
            target_path.as_ptr(),
            kind.as_ref().map_or(std::ptr::null(), |kind| kind.as_ptr()),
            flags,
            options.as_ptr().cast(),
        )
    })
    .map(drop)
    .map_err(|e| io::Error::new(e.kind(), format!("mount at {}: {e}", target.display())))
}

const CLOSED: libc::c_ulong = libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC;

/// The device files every sandbox has, with their device numbers. None of
/// them reaches hardware.
const DEVICES: [(&str, u32, u32); 5] = [
    ("null", 1, 3),
    ("zero", 1, 5),
    ("full", 1, 7),
    ("random", 1, 8),
    ("urandom", 1, 9),
];

/// Builds the sandbox's root and moves into it: an empty filesystem with
/// the sandbox's own `/proc`, `/dev`, and `/tmp`, and the view.
fn root(order: &InitOrder, view: Vec<OwnedFd>) -> io::Result<()> {
    let root = order.root.as_path();
    // Nothing mounted here reaches the machine's own mounts.
    mount("none", Path::new("/"), None, libc::MS_REC | libc::MS_PRIVATE, "")?;
    mount("crowbar", root, Some("tmpfs"), CLOSED, "mode=0755,size=1m")?;
    let directory = |path: &Path| fs::DirBuilder::new().recursive(true).mode(0o755).create(path);

    // Only the processes of this sandbox, and nothing else of the kernel.
    directory(&root.join("proc"))?;
    mount("proc", &root.join("proc"), Some("proc"), CLOSED, "subset=pid")?;

    let dev = root.join("dev");
    directory(&dev)?;
    mount(
        "crowbar",
        &dev,
        Some("tmpfs"),
        libc::MS_NOSUID | libc::MS_NOEXEC,
        "mode=0755,size=64k",
    )?;
    for (name, major, minor) in DEVICES {
        let path = dev.join(name);
        let c_path = cstring(&path)?;
        // SAFETY: `c_path` is a C string.
        checked(unsafe { libc::mknod(c_path.as_ptr(), libc::S_IFCHR | 0o666, libc::makedev(major, minor)) })?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666))?;
    }
    for (name, target) in [
        ("fd", "/proc/self/fd"),
        ("stdin", "/proc/self/fd/0"),
        ("stdout", "/proc/self/fd/1"),
        ("stderr", "/proc/self/fd/2"),
    ] {
        std::os::unix::fs::symlink(target, dev.join(name))?;
    }
    directory(&dev.join("shm"))?;
    mount("crowbar", &dev.join("shm"), Some("tmpfs"), CLOSED, "mode=1777")?;
    let fixed = libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY;
    mount("none", &dev, None, fixed | libc::MS_NOSUID | libc::MS_NOEXEC, "")?;

    directory(&root.join("tmp"))?;
    mount(
        "crowbar",
        &root.join("tmp"),
        Some("tmpfs"),
        libc::MS_NOSUID | libc::MS_NODEV,
        "mode=1777",
    )?;

    for (entry, tree) in order.view.iter().zip(view) {
        let place = root.join(entry.at.strip_prefix("/").map_err(io::Error::other)?);
        if File::from(tree.try_clone()?).metadata()?.is_dir() {
            directory(&place)?;
        } else {
            directory(place.parent().unwrap_or(root))?;
            File::create(&place)?;
        }
        let c_place = cstring(&place)?;
        // SAFETY: `tree` is an open mount, and `c_place` is a C string.
        checked(unsafe {
            libc::syscall(
                libc::SYS_move_mount,
                tree.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_FDCWD,
                c_place.as_ptr(),
                MOVE_MOUNT_F_EMPTY_PATH,
            )
        })
        .map_err(|e| io::Error::new(e.kind(), format!("the view entry at {}: {e}", entry.at.display())))?;
    }
    mount("none", root, None, fixed | CLOSED, "")?;

    // The new root goes over the old one, and the old one is let go.
    std::env::set_current_dir(root)?;
    // SAFETY: both arguments are C strings.
    checked(unsafe { libc::syscall(libc::SYS_pivot_root, c".".as_ptr(), c".".as_ptr()) })?;
    // SAFETY: the argument is a C string.
    checked(unsafe { libc::umount2(c".".as_ptr(), libc::MNT_DETACH) })?;
    std::env::set_current_dir("/")
}

/// Leaves the process with the sandbox's `/dev/null` as its standard input
/// and outputs, the broker connections after them in order, and the status
/// socket. Returns where the status socket is now.
fn descriptors(connections: Vec<OwnedFd>, status: RawFd) -> io::Result<RawFd> {
    let count = connections.len() as RawFd;
    let above = 3 + count + 1;
    // Everything moves out of the way first, so that no descriptor is
    // overwritten before it has been placed.
    let lifted = |fd: RawFd| checked(unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, above) });
    let mut held = Vec::new();
    for connection in &connections {
        held.push(lifted(connection.as_raw_fd())?);
    }
    let lifted_status = lifted(status)?;
    drop(connections);
    let null = File::options().read(true).write(true).open("/dev/null")?;
    if !null.metadata()?.file_type().is_char_device() {
        return Err(io::Error::other("/dev/null is not the null device"));
    }
    for fd in 0..3 {
        // SAFETY: `null` is open, and the target is replaced whole.
        checked(unsafe { libc::dup2(null.as_raw_fd(), fd) })?;
    }
    drop(null);
    for (index, fd) in held.into_iter().enumerate() {
        // SAFETY: `fd` is open, and the target is replaced whole.
        checked(unsafe { libc::dup2(fd, 3 + index as RawFd) })?;
    }
    let placed = 3 + count;
    // SAFETY: `lifted_status` is open, and the target is replaced whole.
    checked(unsafe { libc::dup3(lifted_status, placed, libc::O_CLOEXEC) })?;
    // Whatever else is open is closed: a descriptor the Warden did not mean
    // to pass on never reaches the program.
    // SAFETY: the call closes descriptors this function no longer uses.
    checked(unsafe { libc::syscall(libc::SYS_close_range, placed + 1, u32::MAX, 0) })?;
    Ok(placed)
}

/// Gives up root for the compartment's own user, for good: no group but
/// its own, no capability, none to be had from any program it runs, and
/// none of the Warden's keys.
fn become_user(order: &InitOrder) -> io::Result<()> {
    const JOIN_SESSION_KEYRING: libc::c_int = 1;
    // SAFETY: plain calls on this process's own credentials and limits.
    unsafe {
        // The bounding set is what a program could ever be given. The
        // kernel names its capabilities from 0 up and refuses the first
        // number it does not have.
        let mut capability = 0;
        while libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) == 0 {
            capability += 1;
        }
        if capability == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::EINVAL) {
            return Err(io::Error::last_os_error());
        }
        checked(libc::setgroups(0, std::ptr::null()))?;
        checked(libc::setresgid(order.gid, order.gid, order.gid))?;
        // Leaving user 0 clears the capabilities this process holds.
        checked(libc::setresuid(order.uid, order.uid, order.uid))?;
        checked(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0))?;
        // A session keyring of its own, in place of the one it was born
        // with. A kernel with no keyrings has nothing to replace.
        let joined = libc::syscall(libc::SYS_keyctl, JOIN_SESSION_KEYRING, std::ptr::null::<libc::c_char>());
        if joined == -1 && io::Error::last_os_error().raw_os_error() != Some(libc::ENOSYS) {
            return Err(io::Error::last_os_error());
        }
        // No core file: the machine's handler for one runs as root, outside
        // every sandbox, on memory the compartment chose.
        let none = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        checked(libc::setrlimit(libc::RLIMIT_CORE, &none))?;
    }
    Ok(())
}

/// Becomes the program. Returns only when it could not, with the reason.
fn run(order: &InitOrder) -> io::Error {
    // The order was checked, so none of its text holds a NUL.
    let c_string = |text: &str| CString::new(text).expect("a checked order holds no NUL");
    let program = c_string(&order.program.to_string_lossy());
    let mut arguments = vec![program.clone()];
    arguments.extend(order.args.iter().map(|argument| c_string(argument)));
    let environment: Vec<CString> = order
        .env
        .iter()
        .map(|(name, value)| c_string(&format!("{name}={value}")))
        .collect();
    let pointers = |strings: &[CString]| {
        strings
            .iter()
            .map(|s| s.as_ptr())
            .chain([std::ptr::null()])
            .collect::<Vec<_>>()
    };
    // SAFETY: both arrays hold C strings that outlive the call and end in a
    // null pointer.
    unsafe {
        libc::execve(
            program.as_ptr(),
            pointers(&arguments).as_ptr(),
            pointers(&environment).as_ptr(),
        )
    };
    io::Error::last_os_error()
}
