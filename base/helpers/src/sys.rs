//! The system calls std does not offer: a socket peer's user ID, accounts,
//! and dropping root.

use std::ffi::{CStr, CString, c_char};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

/// An account, as /etc/passwd lists it.
#[derive(Clone, Debug)]
pub struct Account {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub dir: PathBuf,
    pub shell: String,
}

/// This process's real user ID.
#[cfg(test)]
pub fn uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

/// The user ID of the process at the other end of a Unix socket, as the
/// kernel recorded it when the connection was made.
pub fn peer_uid(socket: &impl AsRawFd) -> io::Result<u32> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` and `len` describe a buffer of the size SO_PEERCRED fills.
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

/// Looks an account up with `getpwnam_r` or `getpwuid_r`, given as `call`.
fn lookup(
    call: impl Fn(*mut libc::passwd, *mut c_char, usize, *mut *mut libc::passwd) -> libc::c_int,
) -> io::Result<Option<Account>> {
    let mut size = 1024;
    loop {
        let mut buffer = vec![0 as c_char; size];
        // SAFETY: an all-zero passwd is valid; the call fills it in.
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found = std::ptr::null_mut();
        match call(&mut entry, buffer.as_mut_ptr(), size, &mut found) {
            libc::ERANGE if size < 1 << 20 => size *= 2,
            0 if found.is_null() => return Ok(None),
            0 => {
                // SAFETY: on success each field is a NUL-ended string in `buffer`.
                let text = |p: *const c_char| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
                return Ok(Some(Account {
                    name: text(entry.pw_name),
                    uid: entry.pw_uid,
                    gid: entry.pw_gid,
                    dir: PathBuf::from(text(entry.pw_dir)),
                    shell: text(entry.pw_shell),
                }));
            }
            // Not finding the name is not an error, whatever errno says.
            libc::ENOENT | libc::ESRCH | libc::EBADF | libc::EPERM => return Ok(None),
            errno => return Err(io::Error::from_raw_os_error(errno)),
        }
    }
}

pub fn account_by_name(name: &str) -> io::Result<Option<Account>> {
    let Ok(name) = CString::new(name) else { return Ok(None) };
    // SAFETY: the name ends in NUL, and the buffers are as large as we say.
    lookup(|entry, buffer, size, found| unsafe { libc::getpwnam_r(name.as_ptr(), entry, buffer, size, found) })
}

pub fn account_by_uid(uid: u32) -> io::Result<Option<Account>> {
    // SAFETY: the buffers are as large as we say.
    lookup(|entry, buffer, size, found| unsafe { libc::getpwuid_r(uid, entry, buffer, size, found) })
}

/// The account, or an error that names it.
pub fn require(account: io::Result<Option<Account>>, what: &str) -> io::Result<Account> {
    account?.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no account {what}")))
}

/// A listening socket at `path`, in place of anything there before, owned by
/// `owner` and `gid`, which only they may use.
pub fn listen_as(path: &Path, owner: u32, gid: u32) -> io::Result<UnixListener> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    let listener = UnixListener::bind(path)?;
    std::os::unix::fs::chown(path, Some(owner), Some(gid))?;
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o660))?;
    Ok(listener)
}

/// Drops root for this account and its groups, for good.
pub fn become_account(account: &Account) -> io::Result<()> {
    let name = CString::new(account.name.as_str())?;
    let mut groups = vec![0 as libc::gid_t; 64];
    loop {
        let mut count = groups.len() as libc::c_int;
        // SAFETY: `groups` holds `count` entries.
        if unsafe { libc::getgrouplist(name.as_ptr(), account.gid, groups.as_mut_ptr(), &mut count) } >= 0 {
            groups.truncate(count as usize);
            break;
        }
        groups.resize((count as usize).max(groups.len() * 2), 0);
    }
    let check = |rc: libc::c_int| {
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    };
    // SAFETY: plain system calls on values we own.
    unsafe {
        check(libc::setgroups(groups.len(), groups.as_ptr()))?;
        check(libc::setresgid(account.gid, account.gid, account.gid))?;
        check(libc::setresuid(account.uid, account.uid, account.uid))?;
    }
    let (mut r, mut e, mut s) = (0, 0, 0);
    // SAFETY: three out-parameters of ours.
    check(unsafe { libc::getresuid(&mut r, &mut e, &mut s) })?;
    // SAFETY: with a size of 0, getgroups only counts.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let mut now = vec![0 as libc::gid_t; count.max(0) as usize];
    // SAFETY: `now` holds `count` entries.
    let count = unsafe { libc::getgroups(count, now.as_mut_ptr()) };
    now.truncate(count.max(0) as usize);
    if [r, e, s] != [account.uid; 3] || now.contains(&0) || count < 0 {
        return Err(io::Error::other("could not become the person"));
    }
    Ok(())
}

/// Lets a descriptor pass to the program this process becomes.
pub fn keep_across_exec(fd: &impl AsRawFd) -> io::Result<()> {
    // SAFETY: F_SETFD on a descriptor we hold.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    /// A fresh, empty directory under the system's temporary one, removed
    /// when dropped.
    pub struct TempDir(pub std::path::PathBuf);

    impl TempDir {
        pub fn new() -> TempDir {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("crowbar-helpers-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_peer_is_known_by_its_user_id() {
        let (ours, _theirs) = UnixStream::pair().unwrap();
        assert_eq!(peer_uid(&ours).unwrap(), uid());
    }

    #[test]
    fn an_account_is_found_by_name_and_by_user_id() {
        let me = account_by_uid(uid()).unwrap().expect("this test's own account");
        assert_eq!(me.uid, uid());
        let by_name = account_by_name(&me.name).unwrap().unwrap();
        assert_eq!((by_name.uid, by_name.gid, by_name.dir), (me.uid, me.gid, me.dir));
        assert!(account_by_name("no-such-crowbar-account").unwrap().is_none());
    }

    #[test]
    fn a_socket_is_bound_for_its_group_alone() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = TempDir::new();
        let path = dir.0.join("socket");
        std::fs::write(&path, "stale").unwrap();
        let gid = account_by_uid(uid()).unwrap().unwrap().gid;
        // Only root may give the socket to root; a test that is not root
        // keeps it, as chown(2) allows an owner to keep its own.
        let owner = if uid() == 0 { 0 } else { uid() };
        let _listener = listen_as(&path, owner, gid).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(
            (meta.uid(), meta.gid(), meta.permissions().mode() & 0o7777),
            (owner, gid, 0o660)
        );
        UnixStream::connect(&path).unwrap();
    }
}
