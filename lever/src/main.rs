//! Lever (`crowbar-lever`): the CrowbarOS service that runs from boot as the
//! person, answers the shell on its surface socket and the person's clients on
//! its person socket, drives a harness over ACP, and keeps the record of every
//! thread in the person's home.

mod acp;
mod channel;
mod discovery;
mod frames;
mod fulcrum;
mod handoff;
mod log;
mod mcp;
mod opencode;
mod project;
mod service;
mod store;
mod sys;
mod watcher;

use acp::HarnessConfig;
use channel::{Channel, LEVER_VERSION, Peer};
use log::{Event, Log};
use service::{
    Broadcast, DEFAULT_DISCOVERY_DEBOUNCE, DEFAULT_IDLE_TIMEOUT, DEFAULT_STOP_TIMEOUT, DEFAULT_WAKE_QUIET, Effects,
    PeerReply, Service, unix_now,
};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};
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
  --person-socket <path> --person-uid <uid> \
  [--fulcrum-socket <path>] \
  [--harness <program> [--harness-arg <arg>]... [--harness-env <KEY=VALUE>]... [--harness-watch <dir>]... [--harness-name <name>]] \
  [--idle-timeout-ms <ms>] [--stop-timeout-ms <ms>] [--wake-quiet-ms <ms>] [--discovery-debounce-ms <ms>] [--discovery-timeout-ms <ms>]";

struct Options {
    state: PathBuf,
    surface_socket: PathBuf,
    shell_uid: u32,
    person_socket: PathBuf,
    person_uid: u32,
    fulcrum_socket: Option<PathBuf>,
    harness: Option<HarnessConfig>,
    idle_timeout: Duration,
    stop_timeout: Duration,
    wake_quiet: Duration,
    discovery_debounce: Duration,
    discovery_timeout: Option<Duration>,
}

fn parse_options(args: &[String]) -> Option<Options> {
    let mut state = None;
    let mut surface_socket = None;
    let mut shell_uid = None;
    let mut person_socket = None;
    let mut person_uid = None;
    let mut fulcrum_socket = None;
    let mut harness_program = None;
    let mut harness_name = None;
    let mut harness_args = Vec::new();
    let mut harness_env = Vec::new();
    let mut harness_watch = Vec::new();
    let mut idle_timeout = None;
    let mut stop_timeout = None;
    let mut wake_quiet = None;
    let mut discovery_debounce = None;
    let mut discovery_timeout = None;

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
            "--fulcrum-socket" if fulcrum_socket.is_none() => fulcrum_socket = Some(PathBuf::from(value)),
            "--harness" if harness_program.is_none() => harness_program = Some(value.clone()),
            "--harness-name" if harness_name.is_none() => harness_name = Some(value.clone()),
            "--harness-arg" => harness_args.push(value.clone()),
            "--harness-env" => {
                let (k, v) = value.split_once('=')?;
                if k.is_empty() {
                    return None;
                }
                harness_env.push((k.to_string(), v.to_string()));
            }
            "--harness-watch" => harness_watch.push(PathBuf::from(value)),
            "--stop-timeout-ms" if stop_timeout.is_none() => {
                stop_timeout = Some(Duration::from_millis(value.parse::<u64>().ok()?));
            }
            "--idle-timeout-ms" if idle_timeout.is_none() => {
                idle_timeout = Some(Duration::from_millis(value.parse::<u64>().ok()?));
            }
            "--wake-quiet-ms" if wake_quiet.is_none() => {
                wake_quiet = Some(Duration::from_millis(value.parse::<u64>().ok()?));
            }
            "--discovery-debounce-ms" if discovery_debounce.is_none() => {
                discovery_debounce = Some(Duration::from_millis(value.parse::<u64>().ok()?));
            }
            "--discovery-timeout-ms" if discovery_timeout.is_none() => {
                discovery_timeout = Some(Duration::from_millis(value.parse::<u64>().ok()?));
            }
            _ => return None,
        }
    }

    let harness = match harness_program {
        Some(program) => {
            let name = harness_name.unwrap_or_else(|| {
                Path::new(&program)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("harness")
                    .to_string()
            });
            Some(HarnessConfig {
                id: "harness-1".to_string(),
                name,
                program,
                args: harness_args,
                env: harness_env,
                watch_paths: harness_watch,
            })
        }
        None if harness_name.is_some()
            || !harness_args.is_empty()
            || !harness_env.is_empty()
            || !harness_watch.is_empty() =>
        {
            return None;
        }
        None => None,
    };

    Some(Options {
        state: state?,
        surface_socket: surface_socket?,
        shell_uid: shell_uid?,
        person_socket: person_socket?,
        person_uid: person_uid?,
        fulcrum_socket,
        harness,
        idle_timeout: idle_timeout.unwrap_or(DEFAULT_IDLE_TIMEOUT),
        stop_timeout: stop_timeout.unwrap_or(DEFAULT_STOP_TIMEOUT),
        wake_quiet: wake_quiet.unwrap_or(DEFAULT_WAKE_QUIET),
        discovery_debounce: discovery_debounce.unwrap_or(DEFAULT_DISCOVERY_DEBOUNCE),
        discovery_timeout,
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

fn accept_pending(
    listener: &UnixListener,
    channel: Channel,
    peers: &mut Vec<Peer>,
    next_peer_id: &mut u64,
    log: &mut Log,
) {
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
                    let id = *next_peer_id;
                    *next_peer_id += 1;
                    peers.push(Peer::new(id, stream, channel));
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

fn deliver_replies(replies: &[PeerReply], peers: &mut [Peer], keep: &[bool], log: &mut Log) {
    for reply in replies {
        for (idx, peer) in peers.iter_mut().enumerate() {
            if !keep[idx] || peer.id() != reply.peer_id {
                continue;
            }
            peer.deliver_reply(&reply.message, reply.subscribe.clone());
            if !peer.on_writable() {
                peer.on_gone(log);
                peer.lose();
            }
            break;
        }
    }
}

fn deliver_broadcasts(broadcasts: &[Broadcast], peers: &mut [Peer], keep: &[bool], log: &mut Log, service: &Service) {
    if broadcasts.is_empty() {
        return;
    }
    let list_changed = broadcasts.iter().any(|b| {
        matches!(
            b.message["event"].as_str(),
            Some(
                "request"
                    | "message"
                    | "thread-updated"
                    | "subagent"
                    | "done"
                    | "stopped"
                    | "failed"
                    | "interrupted"
                    | "archived"
                    | "deleted"
            )
        )
    });
    let snapshot = if list_changed && peers.iter().any(|p| p.is_subscribed(service::THREAD_LIST_SUBSCRIPTION)) {
        service.thread_list_event().map(|mut snapshot| {
            snapshot["finished"] = serde_json::json!(broadcasts.iter().any(|b| {
                matches!(
                    b.message["event"].as_str(),
                    Some("done" | "stopped" | "failed" | "interrupted")
                ) || (b.message["event"].as_str() == Some("subagent")
                    && matches!(
                        b.message["state"].as_str(),
                        Some("done" | "stopped" | "failed" | "interrupted")
                    ))
            }));
            snapshot
        })
    } else {
        None
    };
    for (idx, peer) in peers.iter_mut().enumerate() {
        if !keep[idx] {
            continue;
        }
        let mut queued = false;
        for b in broadcasts {
            if peer.is_subscribed(&b.thread_id) {
                peer.queue_event(&b.message);
                queued = true;
            }
        }
        if peer.is_subscribed(service::THREAD_LIST_SUBSCRIPTION)
            && let Some(snapshot) = &snapshot
        {
            peer.queue_event(snapshot);
            queued = true;
        }
        // A peer that cannot be sent to is dropped at its next turn in the
        // loop, where what it had sent is carried out first.
        if queued && !(peer.on_writable() && peer.keeps_up(log)) {
            peer.on_gone(log);
            peer.lose();
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

/// Maximum safe byte length for a Linux `sockaddr_un.sun_path` (108 bytes
/// including the trailing NUL).
const MAX_UNIX_SOCKET_PATH: usize = 104;

/// Resolves an absolute, `SUN_LEN`-safe path for Lever's private MCP tool
/// socket. Prefers `<canonical state>/tools.sock` so a relative `--state`
/// works regardless of a harness session's working directory; falls back to
/// placing the socket beside `person_socket` when `--state` is too long for a
/// Unix socket pathname.
fn resolve_tool_socket_path(state_dir: &Path, person_socket: &Path) -> Result<PathBuf, String> {
    let abs_state = std::fs::canonicalize(state_dir)
        .map_err(|e| format!("could not resolve state directory {}: {e}", state_dir.display()))?;
    let state_candidate = abs_state.join("tools.sock");
    if state_candidate.as_os_str().as_encoded_bytes().len() <= MAX_UNIX_SOCKET_PATH {
        return Ok(state_candidate);
    }

    let person_parent = match person_socket.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => std::env::current_dir().map_err(|e| format!("could not determine current directory: {e}"))?,
    };
    let abs_person_dir = std::fs::canonicalize(&person_parent)
        .map_err(|e| format!("could not resolve socket directory {}: {e}", person_parent.display()))?;
    let abs_person_socket = person_socket
        .file_name()
        .map(|name| abs_person_dir.join(name))
        .unwrap_or_else(|| abs_person_dir.clone());

    let sibling = abs_person_dir.join("tools.sock");
    if sibling != abs_person_socket && sibling.as_os_str().as_encoded_bytes().len() <= MAX_UNIX_SOCKET_PATH {
        return Ok(sibling);
    }
    let pid_sibling = abs_person_dir.join(format!("lever-tools-{}.sock", std::process::id()));
    if pid_sibling != abs_person_socket && pid_sibling.as_os_str().as_encoded_bytes().len() <= MAX_UNIX_SOCKET_PATH {
        return Ok(pid_sibling);
    }
    Err(format!(
        "could not find a short enough path for the MCP tool socket (tried {} and {})",
        state_candidate.display(),
        sibling.display()
    ))
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
    sys::become_subreaper().map_err(|e| format!("could not become the parent of what a harness leaves: {e}"))?;
    write_marker(&marker, pid)?;

    if let Some(parent) = options.person_socket.parent().filter(|p| !p.as_os_str().is_empty()) {
        ensure_private_dir(parent)?;
    }

    let tool_socket = resolve_tool_socket_path(&options.state, &options.person_socket)?;
    let store = Store::open(&options.state.join("lever.sqlite"), unix_now())?;
    let mut service = Service::new(
        store,
        options.harness,
        options.fulcrum_socket,
        tool_socket.clone(),
        options.idle_timeout,
        options.stop_timeout,
        options.wake_quiet,
        options.discovery_debounce,
    );
    if let Some(timeout) = options.discovery_timeout {
        service.set_live_list_timeout(timeout);
    }

    let person_channel = Channel {
        peer: "the person",
        whose: "the person's",
        uid: options.person_uid,
        is_person: true,
        is_tool: false,
    };
    let surface_channel = Channel {
        peer: "the shell",
        whose: "the shell's",
        uid: options.shell_uid,
        is_person: false,
        is_tool: false,
    };
    let tool_channel = Channel {
        peer: "an MCP tool",
        whose: "the person's",
        uid: options.person_uid,
        is_person: true,
        is_tool: true,
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

    let tool_listener = match bind_socket(&tool_socket, 0o600) {
        Ok(listener) => listener,
        Err(err) => {
            drop(surface_listener);
            drop(person_listener);
            let _ = std::fs::remove_file(&options.surface_socket);
            let _ = std::fs::remove_file(&options.person_socket);
            return Err(err);
        }
    };

    let mut peers: Vec<Peer> = Vec::new();
    let mut next_peer_id: u64 = 1;
    let stop_sig = loop {
        if let Some(sig) = signals.take_signal() {
            break sig;
        }
        let woke = service.on_wake(log);
        let everyone = vec![true; peers.len()];
        deliver_replies(&woke.replies, &mut peers, &everyone, log);
        deliver_broadcasts(&woke.broadcasts, &mut peers, &everyone, log, &service);

        let harness_stdout_fd = service.harness_stdout_fd();
        let harness_stdin_fd = service.harness_stdin_fd();
        let watcher_fd = service.watcher_fd();
        let discovery_fd = service.discovery_fd();

        let mut pollfds = Vec::with_capacity(8 + peers.len());
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
        pollfds.push(libc::pollfd {
            fd: tool_listener.as_raw_fd(),
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
        let existing_len = peers.len();
        let stdout_idx = harness_stdout_fd.map(|fd| {
            let i = pollfds.len();
            pollfds.push(libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
            i
        });
        let stdin_idx = harness_stdin_fd.map(|fd| {
            let i = pollfds.len();
            pollfds.push(libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            });
            i
        });
        let watcher_idx = watcher_fd.map(|fd| {
            let i = pollfds.len();
            pollfds.push(libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
            i
        });
        let discovery_idx = discovery_fd.map(|fd| {
            let i = pollfds.len();
            pollfds.push(libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            });
            i
        });

        // A peer whose requests wait, and who has made room for their
        // replies, is answered now and not at the next thing that happens.
        let timeout_ms = if peers.iter().any(|peer| peer.is_held() || peer.is_lost()) {
            0
        } else {
            service.poll_timeout_ms()
        };
        sys::poll(&mut pollfds, timeout_ms).map_err(|e| format!("poll failed: {e}"))?;

        if pollfds[0].revents & libc::POLLIN != 0
            && let Some(sig) = signals.take_signal()
        {
            break sig;
        }

        if pollfds[1].revents & libc::POLLIN != 0 {
            accept_pending(&person_listener, person_channel, &mut peers, &mut next_peer_id, log);
        }
        if pollfds[2].revents & libc::POLLIN != 0 {
            accept_pending(&surface_listener, surface_channel, &mut peers, &mut next_peer_id, log);
        }
        if pollfds[3].revents & libc::POLLIN != 0 {
            accept_pending(&tool_listener, tool_channel, &mut peers, &mut next_peer_id, log);
        }

        let mut keep = vec![true; peers.len()];

        // The harness first, and what it said to who is watching, before any
        // peer is heard: an update that is already here belongs to the turn
        // it came in, not to one a peer is about to begin, and a peer that
        // resumes a thread reads the record with that update in it once.
        let mut effects = Effects::default();
        if let Some(i) = stdout_idx {
            let revents = pollfds[i].revents;
            if revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                let got = service.on_harness_readable(log);
                effects.broadcasts.extend(got.broadcasts);
                effects.replies.extend(got.replies);
            }
        }
        // Its input next, while this is still the harness that was polled: a
        // peer below may have Lever start another.
        if let Some(i) = stdin_idx {
            let revents = pollfds[i].revents;
            if revents & (libc::POLLOUT | libc::POLLERR | libc::POLLHUP) != 0 {
                let got = service.on_harness_writable(log);
                effects.broadcasts.extend(got.broadcasts);
                effects.replies.extend(got.replies);
            }
        }
        if let Some(i) = watcher_idx
            && pollfds[i].revents & libc::POLLIN != 0
        {
            service.on_store_changed();
        }
        if let Some(i) = discovery_idx
            && pollfds[i].revents & libc::POLLIN != 0
        {
            let got = service.on_discovery_ready(log);
            effects.broadcasts.extend(got.broadcasts);
            effects.replies.extend(got.replies);
        }
        deliver_replies(&effects.replies, &mut peers, &keep, log);
        deliver_broadcasts(&effects.broadcasts, &mut peers, &keep, log, &service);
        let mut broadcasts: Vec<Broadcast> = Vec::new();

        // Process existing peers (indices 4..4+existing_len), keeping newly
        // accepted peers. What one peer's request has to tell who is
        // watching is told before the next peer is heard, so a peer that
        // resumes a thread is not then told of a turn its record has.
        for (idx, pfd) in pollfds[4..4 + existing_len].iter().enumerate() {
            let revents = pfd.revents;
            let peer = &mut peers[idx];
            if !keep[idx] || (revents == 0 && !peer.is_held() && !peer.is_lost()) {
                continue;
            }
            broadcasts.clear();
            let mut alive = !peer.is_lost();
            if alive && revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 && peer.wants_read() {
                let (still_alive, peer_broadcasts) = peer.on_readable(&mut service, log);
                alive = still_alive;
                broadcasts.extend(peer_broadcasts);
            }
            if alive && (revents & libc::POLLOUT != 0) && peer.wants_write() {
                alive = peer.on_writable();
            }
            // Not while this peer has something to tell who is watching:
            // that is told first, and its next request is answered at the
            // next pass.
            if alive && broadcasts.is_empty() && peer.is_held() {
                broadcasts.extend(peer.dispatch(&mut service, log));
                alive = peer.on_writable();
            }
            if revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                alive = false;
            }
            if revents & libc::POLLHUP != 0 && !peer.wants_read() {
                alive = false;
            }
            if !alive {
                broadcasts.extend(peer.finish_requests(&mut service, log));
                service.on_peer_gone(peer.id());
                peer.on_gone(log);
            }
            keep[idx] = alive;
            deliver_broadcasts(&broadcasts, &mut peers, &keep, log, &service);
        }

        let mut iter = keep.into_iter();
        peers.retain(|_| iter.next().unwrap_or(true));
    };

    // Clean stop: close active peers, stop harness, stop listening, remove sockets,
    // close DB, remove running marker, and log the stop signal.
    peers.clear();
    drop(tool_listener);
    drop(surface_listener);
    drop(person_listener);
    let _ = std::fs::remove_file(&tool_socket);
    let _ = std::fs::remove_file(&options.surface_socket);
    let _ = std::fs::remove_file(&options.person_socket);
    service.shutdown(log)?;
    let _ = std::fs::remove_file(&marker);
    let _ = log.write(Event::Stop, sys::signal_name(stop_sig));
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("mcp-bridge") && args.len() == 1 {
        if let Err(err) = mcp::run_stdio_bridge() {
            eprintln!("crowbar-lever mcp-bridge: {err}");
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_socket_is_always_absolute_and_falls_back_when_state_path_is_long() {
        let base = Path::new("/tmp").join(format!("lever-toolsock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let short_state = base.join("s");
        let sock_dir = base.join("r");
        std::fs::create_dir_all(&short_state).unwrap();
        std::fs::create_dir_all(&sock_dir).unwrap();
        let person_sock = sock_dir.join("p.sock");

        // Relative state path resolves to an absolute path under the canonical state dir.
        let cwd = std::env::current_dir().unwrap();
        let rel = if let Ok(r) = short_state.strip_prefix(&cwd) {
            r.to_path_buf()
        } else {
            let mut back = PathBuf::new();
            for _ in cwd.components().skip(1) {
                back.push("..");
            }
            back.join(short_state.strip_prefix("/").unwrap())
        };
        assert!(rel.is_relative());
        let resolved_rel = resolve_tool_socket_path(&rel, &person_sock).unwrap();
        assert!(resolved_rel.is_absolute(), "{}", resolved_rel.display());
        assert_eq!(
            resolved_rel,
            std::fs::canonicalize(&short_state).unwrap().join("tools.sock")
        );

        // Long state path (> SUN_LEN) falls back to a short absolute path beside person_socket.
        let long_state = base.join("a".repeat(40)).join("b".repeat(40)).join("c".repeat(40));
        std::fs::create_dir_all(&long_state).unwrap();
        let resolved_long = resolve_tool_socket_path(&long_state, &person_sock).unwrap();
        assert!(resolved_long.is_absolute(), "{}", resolved_long.display());
        assert!(
            resolved_long.as_os_str().as_encoded_bytes().len() <= MAX_UNIX_SOCKET_PATH,
            "too long: {}",
            resolved_long.display()
        );
        assert_eq!(
            resolved_long.parent().unwrap(),
            std::fs::canonicalize(&sock_dir).unwrap()
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
