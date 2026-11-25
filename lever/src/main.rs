//! Lever (`crowbar-lever`): the CrowbarOS service that runs from boot as the
//! person, answers the shell on its surface socket and the person's clients on
//! its person socket, and keeps its state in the person's home.

mod channel;
mod frames;
mod log;
mod store;
mod sys;

use channel::{Channel, LEVER_VERSION, Peer};
use log::{Event, Log};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;
use store::Store;
use sys::SignalPipe;

/// Most connections Lever keeps open on one socket. Each socket has its own
/// count, so the person's clients cannot use up the shell's.
const MAX_PEERS: usize = 64;
/// Most connections Lever takes from one socket in one wake, so a peer that
/// never stops connecting cannot hold the others and a stop signal back.
const MAX_ACCEPTS: usize = 32;

const USAGE: &str = "usage: crowbar-lever \
  --state <dir> \
  --surface-socket <path> --shell-uid <uid> \
  --person-socket <path> --person-uid <uid>";

struct Options {
    state: PathBuf,
    surface_socket: PathBuf,
    shell_uid: u32,
    person_socket: PathBuf,
    person_uid: u32,
}

fn parse_options(args: &[String]) -> Option<Options> {
    let mut state = None;
    let mut surface_socket = None;
    let mut shell_uid = None;
    let mut person_socket = None;
    let mut person_uid = None;

    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        let value = iter.next()?;
        if value.is_empty() {
            return None;
        }
        match flag.as_str() {
            "--state" if state.is_none() => state = Some(PathBuf::from(value)),
            "--surface-socket" if surface_socket.is_none() => surface_socket = Some(PathBuf::from(value)),
            "--shell-uid" if shell_uid.is_none() => shell_uid = Some(value.parse::<u32>().ok()?),
            "--person-socket" if person_socket.is_none() => person_socket = Some(PathBuf::from(value)),
            "--person-uid" if person_uid.is_none() => person_uid = Some(value.parse::<u32>().ok()?),
            _ => return None,
        }
    }

    Some(Options {
        state: state?,
        surface_socket: surface_socket?,
        shell_uid: shell_uid?,
        person_socket: person_socket?,
        person_uid: person_uid?,
    })
}

fn ensure_private_dir(path: &Path) -> Result<(), String> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|e| format!("could not create directory {}: {e}", path.display()))?;
    sys::chmod(path, 0o700).map_err(|e| format!("could not set mode on {}: {e}", path.display()))?;
    Ok(())
}

fn bind_socket(path: &Path, mode: u32) -> Result<UnixListener, String> {
    sys::remove_stale_socket(path).map_err(|e| format!("could not clear stale socket {}: {e}", path.display()))?;
    let listener = UnixListener::bind(path).map_err(|e| format!("could not bind socket {}: {e}", path.display()))?;
    sys::chmod(path, mode).map_err(|e| format!("could not set mode on socket {}: {e}", path.display()))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("could not set nonblocking on {}: {e}", path.display()))?;
    Ok(listener)
}

fn accept_pending(listener: &UnixListener, channel: Channel, peers: &mut Vec<Peer>, log: &mut Log) {
    let mut open = peers.iter().filter(|peer| peer.is_on(channel)).count();
    for _ in 0..MAX_ACCEPTS {
        match listener.accept() {
            Ok((stream, _)) => match sys::peer_uid(&stream) {
                Ok(uid) if uid == channel.uid => {
                    if open >= MAX_PEERS {
                        let _ = log.write(
                            Event::Connection,
                            &format!(
                                "refused {}, which already has {MAX_PEERS} connections open",
                                channel.peer
                            ),
                        );
                        continue;
                    }
                    if stream.set_nonblocking(true).is_err() {
                        let _ = log.write(
                            Event::Error,
                            &format!("could not set {} socket nonblocking", channel.peer),
                        );
                        continue;
                    }
                    let _ = log.write(Event::Connection, &format!("{} connected", channel.peer));
                    peers.push(Peer::new(stream, channel));
                    open += 1;
                }
                Ok(uid) => {
                    let _ = log.write(
                        Event::Connection,
                        &format!("refused user ID {uid}, which is not {}", channel.whose),
                    );
                }
                Err(_) => {
                    let _ = log.write(
                        Event::Connection,
                        "refused a peer whose user ID the kernel did not give",
                    );
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                let _ = log.write(Event::Error, &format!("accept failed: {e}"));
                break;
            }
        }
    }
}

fn write_marker(marker: &Path, pid: u32) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(marker)
        .map_err(|e| format!("could not open running marker {}: {e}", marker.display()))?;
    sys::fchmod(&file, 0o600).map_err(|e| format!("could not set mode on {}: {e}", marker.display()))?;
    writeln!(file, "{pid}").map_err(|e| format!("could not write running marker {}: {e}", marker.display()))?;
    Ok(())
}

fn run(options: Options) -> Result<(), String> {
    let started = Instant::now();
    ensure_private_dir(&options.state)?;
    let mut log = Log::open(options.state.join("lever.log")).map_err(|e| format!("could not open log: {e}"))?;

    let result = run_service(options, started, &mut log);
    if let Err(ref err) = result {
        let _ = log.write(Event::Error, err);
    }
    result
}

fn run_service(options: Options, started: Instant, log: &mut Log) -> Result<(), String> {
    let pid = std::process::id();
    log.write(Event::Start, &format!("process {pid}, Lever {LEVER_VERSION}"))
        .map_err(|e| format!("could not write start log: {e}"))?;

    let marker = options.state.join("running");
    if let Ok(before) = std::fs::read_to_string(&marker) {
        let before = before.trim();
        if !before.is_empty() {
            let _ = log.write(
                Event::Restart,
                &format!("the Lever before this one, process {before}, ended without stopping"),
            );
        }
    }

    let signals = SignalPipe::install().map_err(|e| format!("could not install signal pipe: {e}"))?;
    write_marker(&marker, pid)?;

    let store = Store::open(&options.state.join("lever.sqlite"))?;

    if let Some(parent) = options.person_socket.parent() {
        ensure_private_dir(parent)?;
    }

    let person_channel = Channel {
        peer: "the person",
        whose: "the person's",
        uid: options.person_uid,
    };
    let surface_channel = Channel {
        peer: "the shell",
        whose: "the shell's",
        uid: options.shell_uid,
    };

    let person_listener = bind_socket(&options.person_socket, 0o600)?;
    let _ = log.write(
        Event::Listening,
        &format!(
            "{}, for user ID {}, {} ms after the process started",
            options.person_socket.display(),
            person_channel.uid,
            started.elapsed().as_millis()
        ),
    );

    let surface_listener = match bind_socket(&options.surface_socket, 0o660) {
        Ok(listener) => listener,
        Err(err) => {
            drop(person_listener);
            let _ = std::fs::remove_file(&options.person_socket);
            return Err(err);
        }
    };
    let _ = log.write(
        Event::Listening,
        &format!(
            "{}, for user ID {}, {} ms after the process started",
            options.surface_socket.display(),
            surface_channel.uid,
            started.elapsed().as_millis()
        ),
    );

    let mut peers: Vec<Peer> = Vec::new();
    let stop_sig = loop {
        if let Some(sig) = signals.take_signal() {
            break sig;
        }

        let mut pollfds = Vec::with_capacity(3 + peers.len());
        pollfds.push(libc::pollfd {
            fd: signals.fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        pollfds.push(libc::pollfd {
            fd: person_listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        pollfds.push(libc::pollfd {
            fd: surface_listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        });
        for peer in &peers {
            let mut events = 0;
            if peer.wants_read() {
                events |= libc::POLLIN;
            }
            if peer.wants_write() {
                events |= libc::POLLOUT;
            }
            pollfds.push(libc::pollfd {
                fd: peer.fd(),
                events,
                revents: 0,
            });
        }

        sys::poll(&mut pollfds, -1).map_err(|e| format!("poll failed: {e}"))?;

        if pollfds[0].revents & libc::POLLIN != 0
            && let Some(sig) = signals.take_signal()
        {
            break sig;
        }

        if pollfds[1].revents & libc::POLLIN != 0 {
            accept_pending(&person_listener, person_channel, &mut peers, log);
        }
        if pollfds[2].revents & libc::POLLIN != 0 {
            accept_pending(&surface_listener, surface_channel, &mut peers, log);
        }

        // Process existing peers (indices 3..3+old_len), keeping newly accepted peers.
        let existing_len = pollfds.len() - 3;
        let mut keep = vec![true; peers.len()];
        for (idx, pfd) in pollfds[3..3 + existing_len].iter().enumerate() {
            let revents = pfd.revents;
            if revents == 0 {
                continue;
            }
            let peer = &mut peers[idx];
            let mut alive = true;
            if revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 && peer.wants_read() {
                alive = peer.on_readable(log);
            }
            if alive && (revents & libc::POLLOUT != 0) && peer.wants_write() {
                alive = peer.on_writable();
            }
            if revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                alive = false;
            }
            // A peer that has hung up while Lever was not reading it, because
            // it had sent all it would or had replies waiting, can take
            // nothing more.
            if revents & libc::POLLHUP != 0 && !peer.wants_read() {
                alive = false;
            }
            if !alive {
                peer.on_gone(log);
            }
            keep[idx] = alive;
        }
        let mut iter = keep.into_iter();
        peers.retain(|_| iter.next().unwrap_or(true));
    };

    // Clean stop: close active peers, stop listening, remove sockets, close DB,
    // remove running marker, and log the stop signal.
    peers.clear();
    drop(surface_listener);
    drop(person_listener);
    let _ = std::fs::remove_file(&options.surface_socket);
    let _ = std::fs::remove_file(&options.person_socket);
    store.close()?;
    let _ = std::fs::remove_file(&marker);
    let _ = log.write(Event::Stop, sys::signal_name(stop_sig));
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(options) = parse_options(&args) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if let Err(err) = run(options) {
        eprintln!("crowbar-lever: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
