//! Linux system calls std does not expose: peer credentials, signal pipe,
//! polling, file mode helpers, non-blocking pipes, random bytes, and child
//! process death-with-parent.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    let mut cmd = harness_command(program, args, env_set, env_remove);
    die_with_lever(&mut cmd);
    cmd.spawn()
}

/// A harness command with piped stdin and stdout and the given environment.
pub fn harness_command(program: &str, args: &[String], env_set: &[(String, String)], env_remove: &[&str]) -> Command {
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
    cmd
}

/// Has the command's child end when Lever does.
fn die_with_lever(cmd: &mut Command) {
    // SAFETY: `getpid` cannot fail.
    let parent_pid = unsafe { libc::getpid() };
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
}

/// Children a worker thread started and waits for itself. Lever's main loop
/// neither reaps them nor counts them as its harness's: they are not its to
/// wait for or to end.
static WORKER_CHILDREN: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn worker_children() -> std::sync::MutexGuard<'static, Vec<u32>> {
    WORKER_CHILDREN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A child a worker thread owns: started so Lever's reaper and harness stop
/// pass it by, and ended and waited for when dropped.
pub struct WorkerChild {
    child: Child,
    done: bool,
}

impl WorkerChild {
    /// Starts `cmd` as a worker's child that ends when Lever does. The
    /// registry is held while it starts, so the reaper never sees the child
    /// before it is known.
    pub fn spawn(mut cmd: Command) -> io::Result<Self> {
        die_with_lever(&mut cmd);
        // In a group of its own, so what it starts is ended with it.
        cmd.process_group(0);
        let mut owned = worker_children();
        let child = cmd.spawn()?;
        owned.push(child.id());
        Ok(Self { child, done: false })
    }

    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Waits up to `grace` for the child to end, then ends it: `SIGTERM`,
    /// another `grace`, then `SIGKILL`. Returns how it ended.
    pub fn finish(&mut self, grace: Duration) -> Option<std::process::ExitStatus> {
        if self.done {
            return None;
        }
        self.done = true;
        let group = -(self.child.id() as libc::pid_t);
        if let Some(status) = wait_until(&mut self.child, Instant::now() + grace) {
            // What it started and left behind has the same grace to end on
            // its own, and is then ended.
            let deadline = Instant::now() + grace;
            // SAFETY: probing the child's own process group with no signal.
            let left = || unsafe { libc::kill(group, 0) } == 0;
            while left() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if left() {
                // SAFETY: signalling the child's own process group.
                unsafe { libc::kill(group, libc::SIGKILL) };
            }
            worker_children().retain(|&pid| pid != self.child.id());
            return Some(status);
        }
        // SAFETY: signalling the child's own process group.
        unsafe { libc::kill(group, libc::SIGTERM) };
        let status = wait_until(&mut self.child, Instant::now() + grace);
        // Whatever of its group is left, the child gone or not, is ended
        // with it: one that ignores `SIGTERM` does not outlive the timeout.
        // SAFETY: signalling the child's own process group.
        unsafe { libc::kill(group, libc::SIGKILL) };
        let status = status.or_else(|| self.child.wait().ok());
        worker_children().retain(|&pid| pid != self.child.id());
        status
    }
}

impl Drop for WorkerChild {
    fn drop(&mut self) {
        self.finish(Duration::from_millis(300));
    }
}

fn wait_until(child: &mut Child, deadline: Instant) -> Option<std::process::ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => return None,
        }
    }
}

/// The error a bounded run gives when it runs out of time.
pub const TIMED_OUT: &str = "timed out";

/// What a bounded one-shot command printed.
#[derive(Debug)]
pub struct BoundedOutput {
    pub success: bool,
    pub stdout: Vec<u8>,
}

/// Runs `cmd` as a worker's child with no stdin, reading its stdout until it
/// closes. The child is ended when `timeout` passes or it prints more than
/// `max_stdout` bytes, and the run is then an error.
pub fn run_bounded(mut cmd: Command, timeout: Duration, max_stdout: usize) -> Result<BoundedOutput, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = WorkerChild::spawn(cmd).map_err(|e| format!("could not start: {e}"))?;
    let mut stdout = child.child().stdout.take().ok_or("no stdout pipe")?;
    set_nonblocking(stdout.as_raw_fd()).map_err(|e| format!("could not set stdout nonblocking: {e}"))?;
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return Err(TIMED_OUT.to_string());
        };
        let mut pfd = [libc::pollfd {
            fd: stdout.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
        poll(&mut pfd, timeout_ms).map_err(|e| format!("poll failed: {e}"))?;
        match std::io::Read::read(&mut stdout, &mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                out.extend_from_slice(&chunk[..n]);
                if out.len() > max_stdout {
                    return Err("printed too much".to_string());
                }
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
            Err(e) => return Err(format!("could not read: {e}")),
        }
    }
    drop(stdout);
    let grace = deadline
        .checked_duration_since(Instant::now())
        .unwrap_or_default()
        .min(Duration::from_millis(300));
    let status = child.finish(grace);
    Ok(BoundedOutput {
        success: status.is_some_and(|s| s.success()),
        stdout: out,
    })
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
    // Held while reaping, so a worker's child is known before it can be seen.
    let workers = worker_children();
    for pid in own_children() {
        if Some(pid) == keep || workers.contains(&pid) {
            continue;
        }
        let mut status = 0;
        // SAFETY: waiting, without blocking, for a child of Lever's.
        unsafe {
            libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG);
        }
    }
}

/// The fields of a process's `/proc` stat after its command name, or `None`
/// when it is gone.
fn stat_fields(pid: u32) -> Option<Vec<String>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(')')? + 1..];
    Some(rest.split_whitespace().map(str::to_string).collect())
}

/// A process's state letter, or `None` when it is gone.
fn process_state(pid: u32) -> Option<char> {
    stat_fields(pid)?.first()?.chars().next()
}

/// When a process started, in clock ticks since boot.
pub fn start_time(pid: u32) -> Option<u64> {
    // The 22nd field of the stat; the 3rd after the command name is the 1st.
    stat_fields(pid)?.get(19)?.parse().ok()
}

/// Every process of a harness's that has not ended: the harness's own tree,
/// and those that left it, which came to Lever as their subreaper. One of
/// Lever's children that started before the harness is not its: an earlier
/// harness left it, and it is not this one's to end.
pub fn live_of_harness(harness: u32, started: u64) -> Vec<u32> {
    let mut found = Vec::new();
    // Held while Lever's children are read, so a worker's child started
    // meanwhile is known before it can be seen.
    let workers = worker_children();
    for root in own_children() {
        if workers.contains(&root) {
            continue;
        }
        if root != harness && start_time(root).is_none_or(|t| t < started) {
            continue;
        }
        found.push(root);
        found.extend(descendants(root));
    }
    drop(workers);
    found.retain(|&pid| !matches!(process_state(pid), None | Some('Z' | 'X')));
    found.sort_unstable();
    found.dedup();
    found
}

/// Ends every process of a harness's at once: each is held still first,
/// and they are found again once each held one has stopped, until no
/// process is new, so none starts another while the others end.
pub fn kill_all_of_harness(harness: u32, started: u64) {
    let mut held: Vec<u32> = Vec::new();
    for _ in 0..100 {
        let new: Vec<u32> = live_of_harness(harness, started)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reaper_and_the_harness_stop_leave_a_workers_child_alone() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exit 7"]);
        let mut child = WorkerChild::spawn(cmd).unwrap();
        let pid = child.child().id();
        // Wait for it to end without reaping it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_state(pid) != Some('Z') && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(process_state(pid), Some('Z'));
        reap_orphans(None);
        assert!(!live_of_harness(u32::MAX, 0).contains(&pid));
        // Still its own to wait for, with its own exit status.
        let status = child.finish(Duration::from_secs(1)).unwrap();
        assert_eq!(status.code(), Some(7));
    }

    #[test]
    fn a_running_workers_child_is_not_the_harnesss() {
        let mut cmd = Command::new("sleep");
        cmd.arg("5");
        let mut child = WorkerChild::spawn(cmd).unwrap();
        let pid = child.child().id();
        assert!(!live_of_harness(u32::MAX, 0).contains(&pid));
        child.finish(Duration::from_millis(10));
    }

    #[test]
    fn a_command_that_runs_too_long_is_ended_with_what_it_started() {
        let marker = std::env::temp_dir().join(format!("lever-group-{}", random_hex(4)));
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &format!("sleep 30 & echo $! > {}; wait", marker.display())]);
        let started = Instant::now();
        assert!(run_bounded(cmd, Duration::from_millis(300), 1024).is_err());
        let grandchild: u32 = std::fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
        let _ = std::fs::remove_file(&marker);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(process_state(grandchild), None | Some('Z' | 'X')) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(process_state(grandchild), None | Some('Z' | 'X')),
            "what it started still runs"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn what_a_timed_out_command_started_is_ended_though_it_ignores_sigterm() {
        let marker = std::env::temp_dir().join(format!("lever-group-term-{}", random_hex(4)));
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            &format!("(trap '' TERM; exec sleep 30) & echo $! > {}; wait", marker.display()),
        ]);
        assert!(run_bounded(cmd, Duration::from_millis(300), 1024).is_err());
        let grandchild: u32 = std::fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
        let _ = std::fs::remove_file(&marker);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(process_state(grandchild), None | Some('Z' | 'X')) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(process_state(grandchild), None | Some('Z' | 'X')),
            "it outlived the timeout"
        );
    }

    #[test]
    fn what_a_command_leaves_behind_when_it_ends_is_ended_too() {
        let marker = std::env::temp_dir().join(format!("lever-group-left-{}", random_hex(4)));
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            &format!("sleep 30 >/dev/null & echo $! > {}; echo done", marker.display()),
        ]);
        let out = run_bounded(cmd, Duration::from_secs(5), 1024).unwrap();
        assert!(out.success);
        let left: u32 = std::fs::read_to_string(&marker).unwrap().trim().parse().unwrap();
        let _ = std::fs::remove_file(&marker);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !matches!(process_state(left), None | Some('Z' | 'X')) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(process_state(left), None | Some('Z' | 'X')),
            "what it left still runs"
        );
    }

    #[test]
    fn a_bounded_run_ends_a_command_that_runs_too_long() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo started; exec sleep 30"]);
        let started = Instant::now();
        let err = run_bounded(cmd, Duration::from_millis(200), 1024).unwrap_err();
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));

        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo done"]);
        let out = run_bounded(cmd, Duration::from_secs(5), 1024).unwrap();
        assert!(out.success);
        assert_eq!(out.stdout, b"done\n");
    }
}
