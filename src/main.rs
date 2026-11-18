//! `crowbar-compositor`: the CrowbarOS Wayland compositor.
//!
//! It serves clients on its own wire code. Until Milestone 2 brings displays,
//! it shows nothing: it has no backend, and says so when it starts.

#![forbid(unsafe_code)]

use std::path::PathBuf;

use crowbar_compositor::{
    State,
    connections::{TrustedPart, TrustedParts},
    log::{self, Level},
};

const USAGE: &str = "\
crowbar-compositor: the CrowbarOS Wayland compositor.

It has no public socket. The shell and the Warden each connect to a path
socket of their own, and the compositor accepts a peer only if the kernel
reports that part's user ID. Every other client comes through a listener the
Warden registers for one compartment. All four of these are required:

  --shell-socket PATH    Where the shell connects.
  --shell-uid UID        The user ID the shell runs as.
  --warden-socket PATH   Where the Warden connects.
  --warden-uid UID       The user ID the Warden runs as.

Base makes the directory of each socket, and decides who can enter it.
FULCRUM_LOG sets the log level: error, warn, info, or debug.

This build has no display backend yet (docs/PLAN.md, Milestone 2): it serves
clients and shows nothing.
";

struct Args {
    parts: TrustedParts,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut shell_socket, mut shell_uid, mut warden_socket, mut warden_uid) = (None, None, None, None);
    let mut rest = args;
    while let Some(flag) = rest.next() {
        let mut value = || rest.next().ok_or(format!("{flag} needs a value"));
        let uid = |text: String| text.parse::<u32>().map_err(|_| format!("{text} is not a user ID"));
        match flag.as_str() {
            "--shell-socket" => shell_socket = Some(PathBuf::from(value()?)),
            "--shell-uid" => shell_uid = Some(uid(value()?)?),
            "--warden-socket" => warden_socket = Some(PathBuf::from(value()?)),
            "--warden-uid" => warden_uid = Some(uid(value()?)?),
            "--help" | "-h" => return Err(USAGE.into()),
            other => return Err(format!("unknown argument {other}\n\n{USAGE}")),
        }
    }
    let part = |socket: Option<PathBuf>, uid: Option<u32>, name: &str| match (socket, uid) {
        (Some(socket), Some(uid)) => Ok(TrustedPart { socket, uid }),
        _ => Err(format!("--{name}-socket and --{name}-uid are required\n\n{USAGE}")),
    };
    Ok(Args {
        parts: TrustedParts {
            shell: part(shell_socket, shell_uid, "shell")?,
            warden: part(warden_socket, warden_uid, "warden")?,
        },
    })
}

fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    log::init_from_env();
    let run = || -> std::io::Result<()> {
        crowbar_compositor::limits::raise_descriptor_limit();
        let mut state = State::new()?;
        state.listen(&args.parts)?;
        log::write(
            Level::Info,
            format_args!("compositor is up (no display backend in this build)"),
        );
        state.run()
    };
    if let Err(error) = run() {
        log::write(Level::Error, format_args!("{error}"));
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|arg| arg.to_string()))
    }

    const PARTS: [&str; 8] = [
        "--shell-socket",
        "/run/crowbar/shell/wayland",
        "--shell-uid",
        "901",
        "--warden-socket",
        "/run/crowbar/warden/wayland",
        "--warden-uid",
        "0",
    ];

    #[test]
    fn both_trusted_parts_are_read_from_the_command_line() {
        let args = parse(&PARTS).unwrap();
        assert_eq!(args.parts.shell.socket, PathBuf::from("/run/crowbar/shell/wayland"));
        assert_eq!(args.parts.shell.uid, 901);
        assert_eq!(args.parts.warden.socket, PathBuf::from("/run/crowbar/warden/wayland"));
        assert_eq!(args.parts.warden.uid, 0);
    }

    #[test]
    fn the_compositor_does_not_start_without_both_trusted_parts() {
        for missing in 0..4 {
            let mut args = PARTS.to_vec();
            args.drain(missing * 2..missing * 2 + 2);
            assert!(parse(&args).is_err(), "started without {}", PARTS[missing * 2]);
        }
    }

    /// Each opened a way in that ADR 0016 closes: a program the compositor
    /// starts, a socket any process can find, or a backend that is not a
    /// TTY.
    #[test]
    fn no_flag_starts_a_client_opens_a_socket_or_picks_a_backend() {
        for flag in [
            "--spawn",
            "--shell",
            "--shell-service",
            "--socket",
            "--public-socket",
            "--backend",
        ] {
            let mut args = PARTS.to_vec();
            args.extend([flag, "value"]);
            assert!(parse(&args).is_err(), "{flag} was accepted");
        }
    }
}
