//! The probe: the program the tests run inside a compartment.
//!
//! Its arguments are a list of actions. It carries each one out, as the
//! compartment's own user and from inside the sandbox, and writes one line
//! for it on its broker connection, descriptor 3: the action, a colon, and
//! what happened. A refusal is written as `refused` and the name of the
//! kernel's error.

use std::fs;
use std::io::{self, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::MetadataExt;

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
