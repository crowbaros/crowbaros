//! The probe: the program the tests run inside a compartment.
//!
//! Its arguments are a list of actions. It carries each one out, as the
//! compartment's own user and from inside the sandbox, and writes one line
//! for it on its broker connection, descriptor 3: the action, a colon, and
//! what happened. A refusal is written as `refused` and the name of the
//! kernel's error.

use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpStream, UdpSocket};
use std::os::fd::FromRawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::time::Duration;

fn main() {
    // SAFETY: the Warden starts every program with its first broker
    // connection as descriptor 3, and nothing else here owns it.
    let mut broker = unsafe { fs::File::from_raw_fd(3) };
    let mut arguments = std::env::args().skip(1);
    while let Some(action) = arguments.next() {
        let mut said = action.clone();
        let mut argument = || {
            let argument = arguments
                .next()
                .unwrap_or_else(|| panic!("{action} needs another argument"));
            said.push(' ');
            said.push_str(&argument);
            argument
        };
        let happened = match action.as_str() {
            "ids" => Ok(ids()),
            "ls" => list(&argument()),
            "read" => fs::read_to_string(argument()),
            "write" => {
                let path = argument();
                fs::write(path, argument()).map(|()| String::new())
            }
            "owner" => fs::metadata(argument()).map(|file| file.uid().to_string()),
            "pids" => list("/proc").map(|names| {
                let pids: Vec<&str> = names.split(' ').filter(|name| name.parse::<u32>().is_ok()).collect();
                pids.join(" ")
            }),
            "trace" => {
                let pid = number(&argument());
                // SAFETY: plain call. A refusal leaves nothing attached.
                done(unsafe { libc::ptrace(libc::PTRACE_ATTACH, pid, 0, 0) })
            }
            "peek" => peek(number(&argument())),
            "mem" => fs::File::open(format!("/proc/{}/mem", argument())).map(|_| String::new()),
            "kill" => {
                let pid = number(&argument());
                // SAFETY: plain call.
                done(unsafe { libc::kill(pid, number(&argument())) })
            }
            "abstract-listen" => abstract_address(&argument()).and_then(|address| {
                // The socket stays open for as long as the probe runs.
                std::mem::forget(UnixListener::bind_addr(&address)?);
                Ok(String::new())
            }),
            "abstract-connect" => abstract_address(&argument())
                .and_then(|address| UnixStream::connect_addr(&address))
                .map(|_| String::new()),
            "shm-make" => {
                // SAFETY: plain call.
                done(unsafe { libc::shmget(number(&argument()), 4096, libc::IPC_CREAT | 0o666) })
            }
            "shm-find" => {
                // SAFETY: plain call.
                done(unsafe { libc::shmget(number(&argument()), 0, 0) })
            }
            "connect" => {
                let address = argument().parse().expect("an address");
                TcpStream::connect_timeout(&address, Duration::from_secs(2)).map(|_| String::new())
            }
            "send" => UdpSocket::bind("0.0.0.0:0")
                .and_then(|socket| socket.send_to(b"out", argument()))
                .map(|_| String::new()),
            "interfaces" => interfaces(),
            "daemon" => daemon(&argument()),
            "fds" => descriptors(),
            "say" => {
                // SAFETY: the action names a descriptor the probe was
                // started with, and it is used for this one write.
                let mut connection = unsafe { fs::File::from_raw_fd(number(&argument())) };
                let said = writeln!(connection, "{}", argument()).map(|()| String::new());
                std::mem::forget(connection);
                said
            }
            "env" => {
                let mut entries: Vec<String> = std::env::vars()
                    .map(|(name, value)| format!("{name}={value}"))
                    .collect();
                entries.sort();
                Ok(entries.join(" "))
            }
            "status" => status(),
            "keyring" => {
                const GET_KEYRING_ID: i32 = 0;
                const SESSION_KEYRING: i32 = -3;
                // SAFETY: plain call.
                done(unsafe { libc::syscall(libc::SYS_keyctl, GET_KEYRING_ID, SESSION_KEYRING, 0) })
            }
            "exec" => {
                use std::os::unix::process::CommandExt;
                let program = argument();
                Err(std::process::Command::new(program).args(arguments.by_ref()).exec())
            }
            "userns" => {
                // SAFETY: plain call on this process alone.
                done(unsafe { libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) })
            }
            "mount" => {
                let target = std::ffi::CString::new(argument()).expect("a path");
                // SAFETY: every pointer is a C string or null.
                done(unsafe {
                    libc::mount(
                        c"probe".as_ptr(),
                        target.as_ptr(),
                        c"tmpfs".as_ptr(),
                        0,
                        std::ptr::null(),
                    )
                })
            }
            "interface" => interface(),
            "mknod" => {
                let path = std::ffi::CString::new(argument()).expect("a path");
                let device = libc::makedev(number(&argument()) as u32, number(&argument()) as u32);
                // SAFETY: `path` is a C string.
                done(unsafe { libc::mknod(path.as_ptr(), libc::S_IFCHR | 0o666, device) })
            }
            "open" => fs::File::options()
                .read(true)
                .write(true)
                .open(argument())
                .map(|_| String::new()),
            "trace-child" => trace_child(),
            "listen" => std::net::TcpListener::bind("127.0.0.1:0").map(|_| String::new()),
            "hold" => {
                writeln!(broker, "hold: ok").expect("the broker connection");
                hold(&broker);
                return;
            }
            "exit" => std::process::exit(argument().parse().expect("an exit code")),
            _ => panic!("the probe has no action {action}"),
        };
        let happened = match happened {
            Ok(result) => format!("ok {result}"),
            Err(error) => format!("refused {}", name(&error)),
        };
        writeln!(broker, "{said}: {}", happened.trim_end()).expect("the broker connection");
    }
}

/// The name of the kernel's error, for the ones the tests expect.
fn name(error: &io::Error) -> String {
    let names = [
        (libc::EPERM, "EPERM"),
        (libc::ENOENT, "ENOENT"),
        (libc::ESRCH, "ESRCH"),
        (libc::EAGAIN, "EAGAIN"),
        (libc::ENOMEM, "ENOMEM"),
        (libc::EACCES, "EACCES"),
        (libc::EINVAL, "EINVAL"),
        (libc::EROFS, "EROFS"),
        (libc::ENOSYS, "ENOSYS"),
        (libc::ENETUNREACH, "ENETUNREACH"),
        (libc::ECONNREFUSED, "ECONNREFUSED"),
        (libc::EOPNOTSUPP, "EOPNOTSUPP"),
    ];
    match error.raw_os_error() {
        Some(number) => match names.iter().find(|(known, _)| *known == number) {
            Some((_, name)) => name.to_string(),
            None => format!("E{number}"),
        },
        None => error.to_string(),
    }
}

fn number(text: &str) -> i32 {
    text.parse().unwrap_or_else(|_| panic!("{text} is not a number"))
}

/// The result of a call that returns -1 and sets `errno` when it fails.
fn done(returned: impl Into<i64>) -> io::Result<String> {
    if returned.into() == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(String::new())
    }
}

/// Reads a word of another process's memory. The address is the probe's
/// own `main`, which another copy of the probe has too.
fn peek(pid: i32) -> io::Result<String> {
    let mut word = [0u8; 8];
    let local = libc::iovec {
        iov_base: word.as_mut_ptr().cast(),
        iov_len: word.len(),
    };
    let remote = libc::iovec {
        iov_base: main as *mut libc::c_void,
        iov_len: word.len(),
    };
    // SAFETY: `local` names `word`, which is writable for its length.
    done(unsafe { libc::process_vm_readv(pid, &local, 1, &remote, 1, 0) } as i64)
}

fn abstract_address(name: &str) -> io::Result<SocketAddr> {
    use std::os::linux::net::SocketAddrExt;
    SocketAddr::from_abstract_name(name)
}

/// The names of the network interfaces.
fn interfaces() -> io::Result<String> {
    // SAFETY: the list ends with an entry whose index is 0, each name is a
    // C string, and the list is freed once.
    unsafe {
        let list = libc::if_nameindex();
        if list.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut names = Vec::new();
        let mut entry = list;
        while (*entry).if_index != 0 {
            names.push(
                std::ffi::CStr::from_ptr((*entry).if_name)
                    .to_string_lossy()
                    .into_owned(),
            );
            entry = entry.add(1);
        }
        libc::if_freenameindex(list);
        Ok(names.join(" "))
    }
}

/// The probe's open descriptors and what each is, without the one that
/// reading the list takes.
fn descriptors() -> io::Result<String> {
    let mut open = Vec::new();
    for entry in fs::read_dir("/proc/self/fd")? {
        let entry = entry?;
        let number: u32 = entry.file_name().to_string_lossy().parse().expect("a descriptor");
        let target = fs::read_link(entry.path())?.to_string_lossy().into_owned();
        if !target.starts_with("/proc/") {
            let kind = target.split_once(":[").map_or(target.as_str(), |(kind, _)| kind);
            open.push((number, kind.to_string()));
        }
    }
    open.sort();
    Ok(open
        .iter()
        .map(|(number, kind)| format!("{number}={kind}"))
        .collect::<Vec<_>>()
        .join(" "))
}

/// What the kernel says the probe may do: its capabilities, as numbers,
/// and whether it can gain privileges or make every system call.
fn status() -> io::Result<String> {
    let status = fs::read_to_string("/proc/self/status")?;
    let mut said = Vec::new();
    for key in [
        "CapInh",
        "CapPrm",
        "CapEff",
        "CapBnd",
        "CapAmb",
        "NoNewPrivs",
        "Seccomp",
    ] {
        let value = status
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
            .unwrap_or("missing")
            .trim();
        let value = u64::from_str_radix(value, 16).map_or(value.to_string(), |number| number.to_string());
        said.push(format!("{key}={value}"));
    }
    Ok(said.join(" "))
}

/// Asks the kernel for a second network interface, of the simplest kind.
fn interface() -> io::Result<String> {
    const IFLA_IFNAME: u16 = 3;
    const IFLA_LINKINFO: u16 = 18;
    const IFLA_INFO_KIND: u16 = 1;
    fn attribute(kind: u16, payload: &[u8]) -> Vec<u8> {
        let mut attribute = Vec::new();
        attribute.extend(((4 + payload.len()) as u16).to_ne_bytes());
        attribute.extend(kind.to_ne_bytes());
        attribute.extend(payload);
        attribute.resize(attribute.len().next_multiple_of(4), 0);
        attribute
    }
    // An `ifinfomsg` of zeroes, then the name and the kind of the link.
    let mut body = vec![0u8; 16];
    body.extend(attribute(IFLA_IFNAME, b"probe0\0"));
    body.extend(attribute(IFLA_LINKINFO, &attribute(IFLA_INFO_KIND, b"dummy\0")));
    let flags = libc::NLM_F_REQUEST | libc::NLM_F_CREATE | libc::NLM_F_EXCL | libc::NLM_F_ACK;
    let mut message = Vec::new();
    message.extend(((16 + body.len()) as u32).to_ne_bytes());
    message.extend(libc::RTM_NEWLINK.to_ne_bytes());
    message.extend((flags as u16).to_ne_bytes());
    message.extend(1u32.to_ne_bytes());
    message.extend(0u32.to_ne_bytes());
    message.extend(body);

    // SAFETY: plain calls, and both buffers are valid for their lengths.
    unsafe {
        let socket = libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        );
        if socket == -1 {
            return Err(io::Error::last_os_error());
        }
        let mut answer = [0u8; 256];
        let sent = libc::send(socket, message.as_ptr().cast(), message.len(), 0);
        let received = match sent {
            -1 => -1,
            _ => libc::recv(socket, answer.as_mut_ptr().cast(), answer.len(), 0),
        };
        let failed = io::Error::last_os_error();
        libc::close(socket);
        if received < 20 {
            return Err(failed);
        }
        // The answer is a header, and then the error as a negative number.
        match i32::from_ne_bytes(answer[16..20].try_into().expect("four bytes")) {
            0 => Ok(String::new()),
            error => Err(io::Error::from_raw_os_error(-error)),
        }
    }
}

/// Traces a child of the probe's own.
fn trace_child() -> io::Result<String> {
    // SAFETY: the probe has one thread. The child only waits to be ended.
    unsafe {
        let child = libc::fork();
        if child == -1 {
            return Err(io::Error::last_os_error());
        }
        if child == 0 {
            libc::pause();
            libc::_exit(0);
        }
        let traced = done(libc::ptrace(libc::PTRACE_ATTACH, child, 0, 0));
        libc::kill(child, libc::SIGKILL);
        libc::waitpid(child, std::ptr::null_mut(), 0);
        traced
    }
}

/// Leaves a process behind that tries to get away: it forks twice, starts a
/// session of its own, and then writes to `path` for as long as it lives.
fn daemon(path: &str) -> io::Result<String> {
    // SAFETY: the probe has one thread, so each child may run any code.
    unsafe {
        let child = libc::fork();
        if child == -1 {
            return Err(io::Error::last_os_error());
        }
        if child == 0 {
            libc::setsid();
            if libc::fork() != 0 {
                libc::_exit(0);
            }
            let mut file = fs::File::options()
                .append(true)
                .create(true)
                .open(path)
                .expect("the file");
            loop {
                let _ = file.write_all(b".");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        libc::waitpid(child, std::ptr::null_mut(), 0);
    }
    Ok(String::new())
}

/// Answers `ping` with `pong` until the broker closes the connection.
fn hold(broker: &fs::File) {
    let mut answers = broker.try_clone().expect("the broker connection");
    for line in BufReader::new(broker).lines() {
        match line {
            Ok(line) if line == "ping" => writeln!(answers, "pong").expect("the broker connection"),
            _ => break,
        }
    }
}

/// The user, the group, and the other groups the probe runs as.
fn ids() -> String {
    let mut groups = [0; 64];
    // SAFETY: plain calls, and `groups` has room for the count given.
    let (uid, gid, count) = unsafe {
        (
            libc::geteuid(),
            libc::getegid(),
            libc::getgroups(64, groups.as_mut_ptr()),
        )
    };
    format!("uid={uid} gid={gid} groups={:?}", &groups[..count.max(0) as usize])
}

/// The names in a directory, in order.
fn list(directory: &str) -> io::Result<String> {
    let mut names = fs::read_dir(directory)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<Vec<_>>>()?;
    names.sort();
    Ok(names.join(" "))
}
