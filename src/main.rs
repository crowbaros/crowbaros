//! `crowbar-compositor`: the CrowbarOS Wayland compositor.
//!
//! It serves clients on its own wire code, and shows them on a virtual
//! terminal. A build with `--features nested` can instead show them in a
//! window on a host session, for development; Base never builds that.

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

  --shell-control-socket PATH
                         Where the shell's control channel connects, from
                         the shell's user ID. Without it there is none.

  --backend tty          Where frames go: a virtual terminal, with the seat
                         from seatd. The default.
  --backend nested       A window on the host session, for development. Only
                         in a build with the nested feature.

Base makes the directory of each socket, and decides who can enter it.
FULCRUM_LOG sets the log level: error, warn, info, or debug.
";

/// Where frames go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackendKind {
    Tty,
    #[cfg(feature = "nested")]
    Nested,
}

struct Args {
    parts: TrustedParts,
    backend: BackendKind,
    shell_control: Option<PathBuf>,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut shell_socket, mut shell_uid, mut warden_socket, mut warden_uid) = (None, None, None, None);
    let mut backend = BackendKind::Tty;
    let mut shell_control = None;
    let mut rest = args;
    while let Some(flag) = rest.next() {
        let mut value = || rest.next().ok_or(format!("{flag} needs a value"));
        let uid = |text: String| text.parse::<u32>().map_err(|_| format!("{text} is not a user ID"));
        match flag.as_str() {
            "--shell-socket" => shell_socket = Some(PathBuf::from(value()?)),
            "--shell-uid" => shell_uid = Some(uid(value()?)?),
            "--warden-socket" => warden_socket = Some(PathBuf::from(value()?)),
            "--warden-uid" => warden_uid = Some(uid(value()?)?),
            "--shell-control-socket" => shell_control = Some(PathBuf::from(value()?)),
            "--backend" => match value()?.as_str() {
                "tty" => backend = BackendKind::Tty,
                #[cfg(feature = "nested")]
                "nested" => backend = BackendKind::Nested,
                other => return Err(format!("this build has no backend {other}\n\n{USAGE}")),
            },
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
        backend,
        shell_control,
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
        state.use_cursor_theme_from_environment();
        // Clients are accepted only once there is something to show them on.
        let name = match args.backend {
            BackendKind::Tty => {
                crowbar_compositor::backend::tty::init(&mut state)?;
                "tty"
            }
            #[cfg(feature = "nested")]
            BackendKind::Nested => {
                crowbar_compositor::backend::nested::init(&mut state)?;
                "nested"
            }
        };
        state.listen(&args.parts)?;
        if let Some(socket) = &args.shell_control {
            state.listen_for_control(socket, args.parts.shell.uid)?;
        }
        log::write(Level::Info, format_args!("compositor is up backend={name}"));
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
    fn no_flag_starts_a_client_opens_a_socket_or_picks_another_backend() {
        for flag in ["--spawn", "--shell", "--shell-service", "--socket", "--public-socket"] {
            let mut args = PARTS.to_vec();
            args.extend([flag, "value"]);
            assert!(parse(&args).is_err(), "{flag} was accepted");
        }
        for backend in ["winit", "x11", "headless", "wayland"] {
            let mut args = PARTS.to_vec();
            args.extend(["--backend", backend]);
            assert!(parse(&args).is_err(), "--backend {backend} was accepted");
        }
        let mut args = PARTS.to_vec();
        args.extend(["--backend", "tty"]);
        assert_eq!(parse(&args).map(|args| args.backend), Ok(BackendKind::Tty));
    }

    /// The nested backend makes the compositor a client of another display.
    /// Base's build has no such backend.
    #[cfg(not(feature = "nested"))]
    #[test]
    fn a_build_without_the_nested_feature_has_no_nested_backend() {
        let mut args = PARTS.to_vec();
        args.extend(["--backend", "nested"]);
        assert!(parse(&args).is_err());
    }
}
