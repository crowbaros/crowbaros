//! Linux system calls std does not expose: peer credentials, signal pipe,
//! polling, file mode helpers, non-blocking pipes, random bytes, and child
//! process death-with-parent.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static SIGNAL_PIPE_WRITE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn handle_signal(sig: libc::c_int) {
    let fd = SIGNAL_PIPE_WRITE.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = sig as u8;
        // SAFETY: `write` to a non-blocking pipe is async-signal-safe, and
        // `errno` is put back so the call this handler interrupted still
        // reports its own error.
        unsafe {
            let errno = libc::__errno_location();
            let saved = *errno;
            let _ = libc::write(fd, (&raw const byte).cast(), 1);
            *errno = saved;
        }
    }
}

/// Receives `SIGTERM` and `SIGINT` over a non-blocking pipe readable by `poll`.
pub struct SignalPipe {
    read: OwnedFd,
    _write: OwnedFd,
}

impl SignalPipe {
    /// Installs signal handlers for `SIGTERM` and `SIGINT` and returns the
    /// readable pipe end.
    pub fn install() -> io::Result<Self> {
        let mut fds = [0 as RawFd; 2];
        // SAFETY: `fds` is a valid two-element array for `pipe2`.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `pipe2` succeeded and gave us two owned descriptors.
        let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        SIGNAL_PIPE_WRITE.store(write.as_raw_fd(), Ordering::Release);

        // SAFETY: `handle_signal` only performs an atomic load and `write(2)`.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handle_signal as *const () as libc::sighandler_t;
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_flags = 0;
            for sig in [libc::SIGTERM, libc::SIGINT] {
                if libc::sigaction(sig, &action, std::ptr::null_mut()) != 0 {
                    SIGNAL_PIPE_WRITE.store(-1, Ordering::Release);
                    return Err(io::Error::last_os_error());
                }
            }
        }

        Ok(Self { read, _write: write })
    }

    pub fn fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }

    /// Reads a pending signal number, if any arrived.
    pub fn take_signal(&self) -> Option<libc::c_int> {
        let mut buf = [0u8; 16];
        // SAFETY: reading from an owned non-blocking pipe descriptor.
        let n = unsafe { libc::read(self.read.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 { Some(buf[0] as libc::c_int) } else { None }
    }
}

impl Drop for SignalPipe {
    fn drop(&mut self) {
        SIGNAL_PIPE_WRITE.store(-1, Ordering::Release);
    }
}

/// Human-readable name for a stop signal.
pub fn signal_name(sig: libc::c_int) -> &'static str {
    match sig {
        libc::SIGTERM => "SIGTERM",
        libc::SIGINT => "SIGINT",
        _ => "SIGNAL",
    }
}

/// The user ID of the peer connected to a Unix stream socket, as recorded by
/// the kernel at `connect(2)`.
pub fn peer_uid(socket: &impl AsRawFd) -> io::Result<u32> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` match the kernel's `SO_PEERCRED` struct layout.
    let rc = unsafe {
        libc::getsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

/// Waits on `fds` until at least one descriptor is ready, `timeout_ms` elapses,
/// or a signal interrupts.
pub fn poll(fds: &mut [libc::pollfd], timeout_ms: libc::c_int) -> io::Result<usize> {
    // SAFETY: `fds` is a valid mutable slice of `pollfd`.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
    if rc < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            return Ok(0);
        }
        return Err(err);
    }
    Ok(rc as usize)
}

/// Marks a file descriptor non-blocking.
pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fcntl` on an open file descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fcntl` with `O_NONBLOCK` added to the existing flags.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sets the file mode bits on an open descriptor.
pub fn fchmod(fd: &impl AsRawFd, mode: u32) -> io::Result<()> {
    // SAFETY: `fchmod` on a valid open file descriptor.
    if unsafe { libc::fchmod(fd.as_raw_fd(), mode as libc::mode_t) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Sets the file mode bits on a path.
pub fn chmod(path: &Path, mode: u32) -> io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

/// Removes `path` only if it is an existing Unix socket left behind by an
/// earlier process that did not stop cleanly.
pub fn remove_stale_socket(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => std::fs::remove_file(path),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Generates `bytes` of random data formatted as lowercase hex, for Lever's
/// own thread IDs.
pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    let mut filled = 0;
    while filled < buf.len() {
        // SAFETY: `buf[filled..]` is valid writable memory of the passed length.
        let rc = unsafe { libc::getrandom(buf[filled..].as_mut_ptr().cast(), buf.len() - filled, 0) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // The kernel gave no random bytes. An ID must still differ from
            // the one before it, so the rest comes from the process ID, the
            // time, and a count of the IDs made this way.
            static MADE: AtomicU64 = AtomicU64::new(0);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_nanos() as u64);
            let mut seed = u64::from(std::process::id()) ^ nanos.rotate_left(17) ^ MADE.fetch_add(1, Ordering::Relaxed);
            for slot in &mut buf[filled..] {
                // SplitMix64: each step gives eight well-mixed bits.
                seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut mixed = seed;
                mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                *slot = (mixed ^ (mixed >> 31)) as u8;
            }
            break;
        }
        filled += rc as usize;
    }
    let mut hex = String::with_capacity(bytes * 2);
    for byte in buf {
        use std::fmt::Write as _;
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

/// Spawns a harness child with piped stdin/stdout and null stderr, arranged
/// so the kernel sends `SIGTERM` to the child if Lever dies unexpectedly.
pub fn spawn_harness(
    program: &str,
    args: &[String],
    env_set: &[(String, String)],
    env_remove: &[&str],
) -> io::Result<Child> {
    // SAFETY: `getpid` cannot fail.
    let parent_pid = unsafe { libc::getpid() };
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for key in env_remove {
        cmd.env_remove(key);
    }
    for (key, value) in env_set {
        cmd.env(key, value);
    }
    // SAFETY: `prctl(PR_SET_PDEATHSIG)`, `getppid`, and `_exit` are
    // async-signal-safe.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(io::Error::last_os_error());
            }
            // Lever died before the kernel was asked to tell this child, so
            // nothing will. The child still has Lever's own handler for
            // `SIGTERM`, which only writes to a pipe, so raising the signal
            // would end nothing: the child ends itself.
            if libc::getppid() != parent_pid {
                libc::_exit(128 + libc::SIGTERM);
            }
            Ok(())
        });
    }
    cmd.spawn()
}

/// Every process under `pid`, read from `/proc`: its children, theirs, and so
/// on. A process that left the tree (one whose parent ended before it) is not
/// found.
pub fn descendants(pid: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut parents: Vec<(u32, u32)> = Vec::new();
    for entry in entries.flatten() {
        let Some(child) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // The command name is in parentheses and may hold anything, so the
        // fields are read from after its last closing parenthesis.
        let Some(rest) = stat.rfind(')').map(|at| &stat[at + 1..]) else {
            continue;
        };
        if let Some(parent) = rest.split_whitespace().nth(1).and_then(|p| p.parse::<u32>().ok()) {
            parents.push((child, parent));
        }
    }
    let mut found = Vec::new();
    let mut next = vec![pid];
    while let Some(parent) = next.pop() {
        for &(child, _) in parents.iter().filter(|(_, p)| *p == parent) {
            if !found.contains(&child) {
                found.push(child);
                next.push(child);
            }
        }
    }
    found
}

/// Makes Lever the parent of every process its harness leaves behind: one
/// whose parent ends before it comes to Lever, and not to the system, so a
/// Stop the harness ignores still finds it.
pub fn become_subreaper() -> io::Result<()> {
    // SAFETY: `prctl(PR_SET_CHILD_SUBREAPER)` only sets a flag on Lever.
    if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Lever's own children, read from `/proc`.
fn own_children() -> Vec<u32> {
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for task in tasks.flatten() {
        if let Ok(children) = std::fs::read_to_string(task.path().join("children")) {
            found.extend(children.split_whitespace().filter_map(|p| p.parse::<u32>().ok()));
        }
    }
    found
}

/// Waits for each child of Lever's that has ended, other than `keep`, whose
/// end is its own to wait for. What a harness left behind comes to Lever,
/// and is taken when it ends.
pub fn reap_orphans(keep: Option<u32>) {
    for pid in own_children() {
        if Some(pid) == keep {
            continue;
        }
        let mut status = 0;
        // SAFETY: waiting, without blocking, for a child of Lever's.
        unsafe {
            libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG);
        }
    }
}

/// A process's state letter from `/proc`, or `None` when it is gone.
fn process_state(pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rfind(')')
        .and_then(|at| stat[at + 1..].trim_start().chars().next())
}

/// Every process under Lever that has not ended.
pub fn live_under_lever() -> Vec<u32> {
    // SAFETY: `getpid` cannot fail.
    let me = unsafe { libc::getpid() } as u32;
    descendants(me)
        .into_iter()
        .filter(|&pid| !matches!(process_state(pid), None | Some('Z' | 'X')))
        .collect()
}

/// Ends every process under Lever at once: each is held still first, and the
/// tree read again once each held process has stopped, until no process is
/// new, so none starts another while the others end. Lever is their
/// subreaper, so one whose parent ended is under Lever still.
pub fn kill_all_under_lever() {
    let mut held: Vec<u32> = Vec::new();
    for _ in 0..100 {
        let new: Vec<u32> = live_under_lever()
            .into_iter()
            .filter(|pid| !held.contains(pid))
            .collect();
        if new.is_empty() {
            break;
        }
        for &pid in &new {
            signal_pid(pid, libc::SIGSTOP);
        }
        // A process stops when the signal reaches it, not when it is sent:
        // one in the middle of a fork finishes it first.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        while new
            .iter()
            .any(|&pid| matches!(process_state(pid), Some(state) if !matches!(state, 'T' | 't' | 'Z' | 'X')))
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        held.extend(new);
    }
    for &pid in &held {
        signal_pid(pid, libc::SIGKILL);
    }
}

/// Sends a signal to a child process.
pub fn signal_pid(pid: u32, sig: libc::c_int) {
    // SAFETY: sending a signal to Lever's own child process.
    unsafe {
        let _ = libc::kill(pid as libc::pid_t, sig);
    }
}
