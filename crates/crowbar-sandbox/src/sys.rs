//! The few kernel calls the Warden makes that the standard library has no
//! name for. Each wrapper turns a failure into an `io::Error`.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub(crate) fn cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

/// A result that is -1 when the call failed and set `errno`.
pub(crate) trait Returned: Copy {
    fn failed(self) -> bool;
}

impl Returned for i32 {
    fn failed(self) -> bool {
        self == -1
    }
}

impl Returned for i64 {
    fn failed(self) -> bool {
        self == -1
    }
}

impl Returned for isize {
    fn failed(self) -> bool {
        self == -1
    }
}

/// The result of a call that returns -1 and sets `errno` when it fails.
pub(crate) fn checked<T: Returned>(result: T) -> io::Result<T> {
    if result.failed() {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

/// A connected pair of stream sockets, both closed on exec.
pub(crate) fn socket_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors the call writes.
    checked(unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    })?;
    // SAFETY: the call succeeded, so both are new descriptors nothing else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// The most descriptors one message carries.
pub(crate) const MAX_PASSED: usize = 32;

fn control_space() -> usize {
    // SAFETY: plain arithmetic on a length.
    unsafe { libc::CMSG_SPACE((MAX_PASSED * size_of::<RawFd>()) as u32) as usize }
}

/// Sends `bytes` in one message, with `fds` attached to it.
pub(crate) fn send_with_fds(socket: BorrowedFd<'_>, bytes: &[u8], fds: &[BorrowedFd<'_>]) -> io::Result<()> {
    assert!(fds.len() <= MAX_PASSED && !bytes.is_empty());
    let mut control = vec![0u8; control_space()];
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr() as *mut libc::c_void,
        iov_len: bytes.len(),
    };
    // SAFETY: an all-zero `msghdr` is a valid empty one.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    if !fds.is_empty() {
        let payload = size_of_val(fds);
        message.msg_control = control.as_mut_ptr().cast();
        // SAFETY: plain arithmetic on a length.
        message.msg_controllen = unsafe { libc::CMSG_SPACE(payload as u32) } as _;
        // SAFETY: `msg_control` points at `control`, which is large enough
        // for one header and `fds.len()` descriptors, so the first header
        // and its data are inside it. `BorrowedFd` has the layout of `RawFd`.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(payload as u32) as _;
            std::ptr::copy_nonoverlapping(fds.as_ptr().cast::<u8>(), libc::CMSG_DATA(header), payload);
        }
    }
    // SAFETY: `message` points at `iov` and `control`, both alive here.
    let sent = checked(unsafe { libc::sendmsg(socket.as_raw_fd(), &message, libc::MSG_NOSIGNAL) })?;
    if sent as usize != bytes.len() {
        return Err(io::Error::new(io::ErrorKind::WriteZero, "the message was cut short"));
    }
    Ok(())
}

/// Receives one message into `bytes`, and the descriptors attached to it,
/// each closed on exec. Returns how many bytes arrived.
pub(crate) fn receive_with_fds(socket: BorrowedFd<'_>, bytes: &mut [u8]) -> io::Result<(usize, Vec<OwnedFd>)> {
    let mut control = vec![0u8; control_space()];
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    // SAFETY: an all-zero `msghdr` is a valid empty one.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = control.len() as _;
    // SAFETY: `message` points at `iov` and `control`, both alive here.
    let received = checked(unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) })?;
    let mut fds = Vec::new();
    // SAFETY: the kernel filled `control` up to `msg_controllen`, and the
    // CMSG macros walk only inside that. Each descriptor in an SCM_RIGHTS
    // header is new to this process and owned by nothing else.
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let payload = (*header).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                let data = libc::CMSG_DATA(header);
                for index in 0..payload / size_of::<RawFd>() {
                    let fd = data.add(index * size_of::<RawFd>()).cast::<RawFd>().read_unaligned();
                    fds.push(OwnedFd::from_raw_fd(fd));
                }
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    if message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "the message was cut short"));
    }
    Ok((received as usize, fds))
}
