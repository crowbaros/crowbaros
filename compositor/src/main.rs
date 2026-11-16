use std::path::PathBuf;

use crowbar_compositor::{
    State, backend,
    connections::{TrustedPart, TrustedParts},
};
use smithay::reexports::{calloop::EventLoop, wayland_server::Display};

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

  --backend tty|winit    Take a TTY, or draw into a window on a host session.
                         Default: winit when WAYLAND_DISPLAY or DISPLAY is
                         set, and tty otherwise.
";

struct Args {
    backend: String,
    parts: TrustedParts,
}

fn parse_args(args: impl Iterator<Item = String>, nested: bool) -> Result<Args, String> {
    let mut backend = if nested { "winit" } else { "tty" }.to_owned();
    let (mut shell_socket, mut shell_uid, mut warden_socket, mut warden_uid) = (None, None, None, None);
    let mut rest = args;
    while let Some(flag) = rest.next() {
        let mut value = || rest.next().ok_or(format!("{flag} needs a value"));
        let uid = |text: String| text.parse::<u32>().map_err(|_| format!("{text} is not a user ID"));
        match flag.as_str() {
            "--backend" => backend = value()?,
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
        backend,
        parts: TrustedParts {
            shell: part(shell_socket, shell_uid, "shell")?,
            warden: part(warden_socket, warden_uid, "warden")?,
        },
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let nested = std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some();
    let args = match parse_args(std::env::args().skip(1), nested) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    init_logging();

    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    let display: Display<State> = Display::new()?;
    let mut state = State::new(&mut event_loop, display);

    match args.backend.as_str() {
        "winit" => backend::winit::init(&mut event_loop, &mut state)?,
        "tty" => backend::tty::init(&mut event_loop, &mut state)?,
        other => return Err(format!("unknown backend {other}").into()),
    }
    // Clients are accepted only once there is something to show them on.
    state.listen(&args.parts)?;
    tracing::info!(backend = %args.backend, "compositor is up");

    event_loop.run(None, &mut state, |state| {
        let _ = state.display_handle.flush_clients();
    })?;
    Ok(())
}

fn init_logging() {
    use std::io::IsTerminal;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,crowbar_compositor=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(std::io::stderr().is_terminal())
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, String> {
        parse_args(args.iter().map(|arg| arg.to_string()), false)
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
        assert_eq!(args.backend, "tty");
    }

    #[test]
    fn the_compositor_does_not_start_without_both_trusted_parts() {
        for missing in 0..4 {
            let mut args = PARTS.to_vec();
            args.drain(missing * 2..missing * 2 + 2);
            assert!(parse(&args).is_err(), "started without {}", PARTS[missing * 2]);
        }
    }

    /// The spike took these. Each opened a way in that ADR 0016 closes: a
    /// program the compositor starts, or a socket any process can find.
    #[test]
    fn no_flag_starts_a_client_or_opens_a_socket() {
        for flag in ["--spawn", "--shell", "--shell-service", "--socket", "--public-socket"] {
            let mut args = PARTS.to_vec();
            args.extend([flag, "value"]);
            assert!(parse(&args).is_err(), "{flag} was accepted");
        }
    }
}
