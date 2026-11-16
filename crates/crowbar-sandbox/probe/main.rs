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
    if returned.into() == -1 { Err(io::Error::last_os_error()) } else { Ok(String::new()) }
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
            names.push(std::ffi::CStr::from_ptr((*entry).if_name).to_string_lossy().into_owned());
            entry = entry.add(1);
        }
        libc::if_freenameindex(list);
        Ok(names.join(" "))
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
            let mut file = fs::File::options().append(true).create(true).open(path).expect("the file");
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
