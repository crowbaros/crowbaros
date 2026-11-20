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

It has two kinds of client, the shell and the person's apps, and each
connects to a path socket of its own. The compositor accepts a peer only if
the kernel reports the user ID given for that socket. All four of these are
required:

  --shell-socket PATH    Where the shell connects.
  --shell-uid UID        The user ID the shell runs as.
  --apps-socket PATH     Where every app connects. Base links it into the
                         person's runtime directory for their apps to find.
  --apps-uid UID         The person's user ID, which every app runs as.

  --shell-control-socket PATH
                         Where the shell's control channel connects, from
                         the shell's user ID. Without it there is none.

  --lock-after SECONDS   Locks the session after this long without input,
                         unless a shown surface inhibits idle. Without it,
                         only the shell and the confirm chord lock it.

  --unlock-socket PATH   Where the unlock authority connects, from
  --unlock-uid UID       --unlock-uid alone. With it, only the authority
                         unlocks the session, after the confirm chord: the
                         shell's word is not enough. Without it, the shell's
                         request unlocks.

  --start-locked         Starts with the session locked. It needs the unlock
                         authority.

  --layout PATH          Where the layout of displays is kept: each
                         monitor's mode, place, and scale. Without it, the
                         layout lasts until the compositor stops.

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
    layout: Option<PathBuf>,
    lock_after: Option<std::time::Duration>,
    unlock: Option<TrustedPart>,
    start_locked: bool,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Args, String> {
    let (mut shell_socket, mut shell_uid, mut apps_socket, mut apps_uid) = (None, None, None, None);
    let mut backend = BackendKind::Tty;
    let mut shell_control = None;
    let mut layout = None;
    let mut lock_after = None;
    let (mut unlock_socket, mut unlock_uid) = (None, None);
    let mut start_locked = false;
    let mut rest = args;
    while let Some(flag) = rest.next() {
        let mut value = || rest.next().ok_or(format!("{flag} needs a value"));
        let uid = |text: String| text.parse::<u32>().map_err(|_| format!("{text} is not a user ID"));
        match flag.as_str() {
            "--shell-socket" => shell_socket = Some(PathBuf::from(value()?)),
            "--shell-uid" => shell_uid = Some(uid(value()?)?),
            "--apps-socket" => apps_socket = Some(PathBuf::from(value()?)),
            "--apps-uid" => apps_uid = Some(uid(value()?)?),
            "--shell-control-socket" => shell_control = Some(PathBuf::from(value()?)),
            "--layout" => layout = Some(PathBuf::from(value()?)),
            "--unlock-socket" => unlock_socket = Some(PathBuf::from(value()?)),
            "--unlock-uid" => unlock_uid = Some(uid(value()?)?),
            "--start-locked" => start_locked = true,
            "--lock-after" => {
                let text = value()?;
                let seconds = text
                    .parse::<u64>()
                    .ok()
                    .filter(|seconds| *seconds > 0)
                    .ok_or(format!("{text} is not a number of seconds"))?;
                lock_after = Some(std::time::Duration::from_secs(seconds));
            }
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
    let unlock = match (unlock_socket, unlock_uid) {
        (Some(socket), Some(uid)) => Some(TrustedPart { socket, uid }),
        (None, None) => None,
        _ => return Err(format!("--unlock-socket and --unlock-uid come together\n\n{USAGE}")),
    };
    if start_locked && unlock.is_none() {
        return Err(format!("--start-locked needs the unlock authority\n\n{USAGE}"));
    }
    Ok(Args {
        parts: TrustedParts {
            shell: part(shell_socket, shell_uid, "shell")?,
            apps: part(apps_socket, apps_uid, "apps")?,
        },
        backend,
        shell_control,
        layout,
        lock_after,
        unlock,
        start_locked,
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
        // The layout is read before any monitor is lit, which takes it.
        if let Some(layout) = &args.layout {
            state.keep_layout_in(layout);
        }
        state.lock_after_idle(args.lock_after);
        if args.start_locked {
            state.lock("start");
        }
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
        if let Some(authority) = &args.unlock {
            state.listen_for_unlock_authority(&authority.socket, authority.uid)?;
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
        "--apps-socket",
        "/run/crowbar-person/wayland-0",
        "--apps-uid",
        "1003",
    ];

    #[test]
    fn both_sockets_are_read_from_the_command_line() {
        let args = parse(&PARTS).unwrap();
        assert_eq!(args.parts.shell.socket, PathBuf::from("/run/crowbar/shell/wayland"));
        assert_eq!(args.parts.shell.uid, 901);
        assert_eq!(args.parts.apps.socket, PathBuf::from("/run/crowbar-person/wayland-0"));
        assert_eq!(args.parts.apps.uid, 1003);
    }

    #[test]
    fn the_compositor_does_not_start_without_both_sockets() {
        for missing in 0..4 {
            let mut args = PARTS.to_vec();
            args.drain(missing * 2..missing * 2 + 2);
            assert!(parse(&args).is_err(), "started without {}", PARTS[missing * 2]);
        }
    }

    /// Each would open another way in: a program the compositor starts, a
    /// socket with no user ID checked, or a backend that is not a TTY.
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

    /// The unlock authority is a socket and a user ID together, and starting
    /// locked needs it: otherwise nothing could unlock but the shell's word.
    #[test]
    fn starting_locked_needs_the_whole_unlock_authority() {
        let with = |extra: &[&'static str]| {
            let mut args = PARTS.to_vec();
            args.extend_from_slice(extra);
            parse(&args)
        };
        assert!(with(&["--start-locked"]).is_err());
        assert!(with(&["--unlock-socket", "/run/unlock"]).is_err());
        assert!(with(&["--unlock-uid", "0"]).is_err());
        let args = with(&["--unlock-socket", "/run/unlock", "--unlock-uid", "0", "--start-locked"]).unwrap();
        assert!(args.start_locked);
        let authority = args.unlock.unwrap();
        assert_eq!((authority.socket, authority.uid), (PathBuf::from("/run/unlock"), 0));
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
