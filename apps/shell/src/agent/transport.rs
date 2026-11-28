// SPDX-License-Identifier: MIT
//! Lever Unix-socket transport adapter: handshake, bootstrap, reconnect backoff,
//! command writer, and runtime-directory persistence for active thread & picks.

use anyhow::{Result, bail};
use std::{
    io::{self, ErrorKind, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

use super::types::{
    AgentCommand, AgentEvent, LeverMessage, ResumedThread, StatusInfo, calm_turn_error,
};
use super::wire::{
    DeadlineIoError, FrameReadError, MAX_FRAME, SURFACE_VERSION, encode_frame, opt_str,
    parse_lever_message, read_frame, read_frame_before, write_frame_before,
};
use crate::events::{Event, Sender};

const INITIAL_BACKOFF: Duration = Duration::from_millis(200);
const MAX_BACKOFF: Duration = Duration::from_secs(5);
/// Deadline for handshake + initial `status` / `list` / `resume` before
/// reporting `Connected` to the drawer.
pub(super) const INIT_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct CommandSender {
    tx: mpsc::Sender<AgentCommand>,
}

impl CommandSender {
    pub fn dispatch(&self, command: AgentCommand) {
        let _ = self.tx.send(command);
    }

    #[cfg(test)]
    pub fn select(&self, thread: Option<String>) {
        self.dispatch(AgentCommand::Select { thread });
    }

    #[cfg(test)]
    pub fn send_message(&self, thread: Option<String>, text: String, mode: Option<String>) {
        self.dispatch(AgentCommand::Send {
            thread,
            text,
            mode,
            model: None,
            effort: None,
        });
    }

    #[cfg(test)]
    pub fn stop_turn(&self, thread: String, turn: Option<u64>) {
        self.dispatch(AgentCommand::Stop { thread, turn });
    }
}

/// Where the Model and effort the person last picked are kept: beside the
/// active thread, in the shell's runtime directory.
fn picks_path() -> Option<PathBuf> {
    default_thread_state_path().map(|path| path.with_file_name("last-picks"))
}

/// The Model and effort the person last picked, as the shell last saved them.
pub fn load_picks() -> (Option<String>, Option<String>) {
    read_picks(picks_path().as_deref())
}

pub(super) fn read_picks(path: Option<&Path>) -> (Option<String>, Option<String>) {
    let Some(value) = path
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
    else {
        return (None, None);
    };
    let model = opt_str(&value, "model").filter(|m| !m.trim().is_empty());
    let effort = model.as_ref().and_then(|_| opt_str(&value, "effort"));
    (model, effort)
}

/// Keeps the Model and effort the person last picked, so a new thread starts
/// with them after the shell restarts.
pub fn save_picks(model: Option<&str>, effort: Option<&str>) {
    write_picks(picks_path().as_deref(), model, effort);
}

pub(super) fn write_picks(path: Option<&Path>, model: Option<&str>, effort: Option<&str>) {
    let Some(path) = path else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let body = serde_json::json!({ "model": model, "effort": effort }).to_string();
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn default_thread_state_path() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(|dir| {
            PathBuf::from(dir)
                .join("crowbaros-shell")
                .join("active-thread")
        })
}

pub(super) fn load_saved_thread(path: Option<&Path>) -> Option<String> {
    let path = path?;
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(super) fn save_active_thread(path: Option<&Path>, thread: Option<&str>) {
    let Some(path) = path else {
        return;
    };
    match thread {
        Some(tid) if !tid.trim().is_empty() => {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
            if std::fs::write(&tmp, format!("{tid}\n")).is_ok()
                && std::fs::rename(&tmp, path).is_err()
            {
                let _ = std::fs::remove_file(&tmp);
            }
        }
        _ => {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn is_routine_connect_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
            matches!(io.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused)
        })
    })
}

/// Starts the background client for `CROWBAR_LEVER_SOCKET`, if configured.
pub fn start_from_env(events: Sender) -> Option<CommandSender> {
    let path = std::env::var_os("CROWBAR_LEVER_SOCKET").map(PathBuf::from)?;
    Some(start_with_options(
        path,
        events,
        INIT_DEADLINE,
        default_thread_state_path(),
    ))
}

/// Starts the background client for a specific surface socket path.
#[cfg(test)]
pub(super) fn start(path: PathBuf, events: Sender) -> CommandSender {
    start_with_options(path, events, INIT_DEADLINE, None)
}

pub(super) fn start_with_options(
    path: PathBuf,
    events: Sender,
    init_deadline: Duration,
    state_path: Option<PathBuf>,
) -> CommandSender {
    start_with_stream_clone(
        path,
        events,
        init_deadline,
        state_path,
        UnixStream::try_clone,
    )
}

pub(super) fn start_with_stream_clone(
    path: PathBuf,
    events: Sender,
    init_deadline: Duration,
    state_path: Option<PathBuf>,
    mut clone_stream: impl FnMut(&UnixStream) -> io::Result<UnixStream> + Send + 'static,
) -> CommandSender {
    let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>();
    let writer: Arc<Mutex<Option<UnixStream>>> = Arc::new(Mutex::new(None));
    let initial_thread = load_saved_thread(state_path.as_deref());
    let active_thread: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(initial_thread));

    // Writer thread: forwards UI commands to the currently connected stream.
    let writer_for_cmds = writer.clone();
    let events_for_cmds = events.clone();
    let active_for_cmds = active_thread.clone();
    let path_for_cmds = state_path.clone();
    std::thread::spawn(move || {
        for cmd in cmd_rx {
            if let AgentCommand::Select { thread } = &cmd {
                if let Ok(mut active) = active_for_cmds.lock() {
                    *active = thread.clone();
                }
                save_active_thread(path_for_cmds.as_deref(), thread.as_deref());
                if thread.is_none() {
                    continue;
                }
            }
            let op = match &cmd {
                AgentCommand::Select { .. } => "resume",
                AgentCommand::Lifecycle { .. } => "lifecycle",
                AgentCommand::MakeProject { .. } => "make-project",
                AgentCommand::ListDirs { .. } => "list-dirs",
                AgentCommand::Send { .. } => "send",
                AgentCommand::SetModel { .. } => "set-model",
                AgentCommand::SetEffort { .. } => "set-effort",
                AgentCommand::FetchEfforts { .. } => "efforts",
                AgentCommand::FetchCommands { .. } => "commands",
                AgentCommand::Stop { .. } => "stop",
                AgentCommand::Answer { .. } => "answer",
                AgentCommand::Unqueue { .. } => "unqueue",
                AgentCommand::SetMode { .. } => "set-mode",
                AgentCommand::SetDefaultMode { .. } => "set-default-mode",
                AgentCommand::SetFreeModels { .. } => "set-free-models",
                AgentCommand::FetchModes { .. } => "models",
                AgentCommand::RefreshStatus => "status",
            };
            let request = match cmd {
                AgentCommand::Select { thread } => {
                    serde_json::json!({"op":"resume","thread":thread})
                }
                AgentCommand::Lifecycle { thread, delete } => serde_json::json!({
                    "op": if delete { "delete" } else { "archive" },
                    "thread": thread
                }),
                AgentCommand::MakeProject {
                    thread,
                    name,
                    place,
                    path,
                } => {
                    let mut req = serde_json::json!({"op": "make-project", "thread": thread});
                    if let Some(name) = name {
                        req["name"] = serde_json::Value::String(name);
                    }
                    if let Some(place) = place {
                        req["place"] = serde_json::Value::String(place);
                    }
                    if let Some(path) = path {
                        req["path"] = serde_json::Value::String(path);
                    }
                    req
                }
                AgentCommand::ListDirs { path } => {
                    let mut req = serde_json::json!({"op": "list-dirs"});
                    if let Some(path) = path {
                        req["path"] = serde_json::Value::String(path);
                    }
                    req
                }
                AgentCommand::Send {
                    thread,
                    text,
                    mode,
                    model,
                    effort,
                } => {
                    let mut request = match thread {
                        Some(thread) => {
                            serde_json::json!({ "op": "send", "thread": thread, "text": text })
                        }
                        None => serde_json::json!({ "op": "send", "thread": null, "text": text }),
                    };
                    for (key, value) in [("mode", mode), ("model", model), ("effort", effort)] {
                        if let Some(value) = value {
                            request[key] = serde_json::Value::String(value);
                        }
                    }
                    request
                }
                AgentCommand::SetModel { thread, model } => {
                    serde_json::json!({ "op": "set-model", "thread": thread, "model": model })
                }
                AgentCommand::SetEffort { thread, effort } => {
                    serde_json::json!({ "op": "set-effort", "thread": thread, "effort": effort })
                }
                AgentCommand::FetchEfforts { model } => {
                    serde_json::json!({ "op": "efforts", "model": model })
                }
                AgentCommand::FetchCommands { thread } => match thread {
                    Some(thread) => serde_json::json!({ "op": "commands", "thread": thread }),
                    None => serde_json::json!({ "op": "commands" }),
                },
                AgentCommand::Answer {
                    thread,
                    request,
                    answer,
                } => serde_json::json!({
                    "op": "answer", "thread": thread, "request": request, "answer": answer
                }),
                AgentCommand::Unqueue { thread, queued } => {
                    serde_json::json!({ "op": "unqueue", "thread": thread, "queued": queued })
                }
                AgentCommand::SetMode { thread, mode } => {
                    serde_json::json!({ "op": "set-mode", "thread": thread, "mode": mode })
                }
                AgentCommand::SetDefaultMode { mode } => {
                    serde_json::json!({ "op": "set-default-mode", "mode": mode })
                }
                AgentCommand::SetFreeModels { enabled } => {
                    serde_json::json!({ "op": "set-free-models", "enabled": enabled })
                }
                AgentCommand::FetchModes { refresh } => {
                    if refresh {
                        serde_json::json!({ "op": "models", "refresh": true })
                    } else {
                        serde_json::json!({ "op": "models" })
                    }
                }
                AgentCommand::Stop { thread, turn } => match turn {
                    Some(turn) => {
                        serde_json::json!({ "op": "stop", "thread": thread, "turn": turn })
                    }
                    None => serde_json::json!({ "op": "stop", "thread": thread }),
                },
                AgentCommand::RefreshStatus => serde_json::json!({ "op": "status" }),
            };
            let frame = encode_frame(&request);
            let write_outcome = {
                let mut guard = match writer_for_cmds.lock() {
                    Ok(g) => g,
                    Err(_) => break,
                };
                match guard.as_mut() {
                    Some(stream) => match stream.write_all(&frame) {
                        Ok(()) => Some(Ok(())),
                        Err(e) => {
                            log::warn!("Agent surface socket write failed: {e}");
                            if let Some(dead) = guard.take() {
                                let _ = dead.shutdown(std::net::Shutdown::Both);
                            }
                            Some(Err(e))
                        }
                    },
                    None => None,
                }
            };
            if !matches!(write_outcome, Some(Ok(()))) {
                match op {
                    "send" | "resume" | "lifecycle" | "make-project" | "list-dirs" => {
                        let _ = events_for_cmds.send(Event::Agent(AgentEvent::Error {
                            code: "disconnected".into(),
                            message: "The Agent is not reachable right now.".into(),
                        }));
                    }
                    "stop" => {
                        let _ = events_for_cmds.send(Event::Agent(AgentEvent::Error {
                            code: "disconnected".into(),
                            message: "Could not send Stop — the Agent is not reachable right now."
                                .into(),
                        }));
                    }
                    "answer" | "set-mode" | "set-default-mode" | "set-free-models" | "models"
                    | "set-model" | "set-effort" | "efforts" => {
                        let _ = events_for_cmds.send(Event::Agent(AgentEvent::Error {
                            code: "disconnected".into(),
                            message: "The Agent is not reachable right now.".into(),
                        }));
                    }
                    _ => {}
                }
            }
        }
    });

    // Reader / reconnect thread.
    std::thread::spawn(move || {
        let mut backoff = INITIAL_BACKOFF;
        let mut skipped_resume: Option<(String, SkipResumeReason)> = None;
        let mut last_bootstrap_warn: Option<String> = None;
        let mut allow_list_fallback = active_thread.lock().ok().is_none_or(|g| g.is_none());
        loop {
            let existing_thread = active_thread.lock().ok().and_then(|g| g.clone());
            match connect_and_bootstrap(
                &path,
                existing_thread,
                allow_list_fallback,
                skipped_resume.as_ref().map(|(t, r)| (t.as_str(), *r)),
                init_deadline,
            ) {
                Ok(BootstrapResult {
                    mut stream,
                    status,
                    resumed,
                    preserved_thread,
                    resume_error,
                }) => {
                    backoff = INITIAL_BACKOFF;
                    last_bootstrap_warn = None;
                    let _ = stream
                        .write_all(&encode_frame(&serde_json::json!({"op":"list","watch":true})));
                    if matches!(skipped_resume, Some((_, SkipResumeReason::TimedOut))) {
                        skipped_resume = None;
                    }
                    let Ok(write_half) = clone_stream(&stream) else {
                        std::thread::sleep(backoff);
                        continue;
                    };
                    allow_list_fallback = false;
                    let mut retry_resume = preserved_thread.clone();
                    let mut resubscribing = None;
                    let _ = write_half.set_write_timeout(Some(Duration::from_secs(5)));
                    let current_tid = resumed
                        .as_ref()
                        .map(|r| r.thread.clone())
                        .or_else(|| preserved_thread.clone());
                    if let Ok(mut guard) = active_thread.lock() {
                        *guard = current_tid.clone();
                    }
                    save_active_thread(state_path.as_deref(), current_tid.as_deref());
                    if let Ok(mut guard) = writer.lock() {
                        *guard = Some(write_half);
                    }
                    log::info!("SHELL agent-socket connected=true");
                    if events
                        .send(Event::Agent(AgentEvent::Connected {
                            status,
                            resumed,
                            preserved_thread,
                            resume_error,
                        }))
                        .is_err()
                    {
                        break;
                    }
                    loop {
                        match read_frame(&mut stream) {
                            Ok(message) => {
                                if let Some(parsed) = parse_lever_message(&message) {
                                    if let Some(mut event) = parsed.into_agent_event() {
                                        if let AgentEvent::Resumed(r) = &event
                                            && resubscribing.as_deref() == Some(r.thread.as_str())
                                        {
                                            resubscribing = None;
                                            event = AgentEvent::Recovered(r.clone());
                                        }
                                        match &event {
                                            AgentEvent::Sent { thread, .. } => {
                                                skipped_resume = None;
                                                if let Ok(mut guard) = active_thread.lock() {
                                                    *guard = Some(thread.clone());
                                                }
                                                save_active_thread(
                                                    state_path.as_deref(),
                                                    Some(thread.as_str()),
                                                );
                                            }
                                            AgentEvent::Lifecycle { thread, .. } => {
                                                if let Ok(mut active) = active_thread.lock()
                                                    && active.as_deref() == Some(thread)
                                                {
                                                    *active = None;
                                                    save_active_thread(state_path.as_deref(), None);
                                                }
                                            }
                                            AgentEvent::Recovered(r) => {
                                                let Ok(active) = active_thread.lock() else {
                                                    continue;
                                                };
                                                if active.as_deref() != Some(r.thread.as_str()) {
                                                    continue;
                                                }
                                                skipped_resume = None;
                                            }
                                            AgentEvent::Resumed(r) => {
                                                skipped_resume = None;
                                                if let Ok(mut guard) = active_thread.lock() {
                                                    *guard = Some(r.thread.clone());
                                                }
                                                save_active_thread(
                                                    state_path.as_deref(),
                                                    Some(r.thread.as_str()),
                                                );
                                            }
                                            AgentEvent::Error { code, .. }
                                                if code == "working" =>
                                            {
                                                if let Some(thread) = retry_resume.take()
                                                    && let Ok(active) = active_thread.lock()
                                                    && active.as_deref() == Some(thread.as_str())
                                                    && let Ok(mut guard) = writer.lock()
                                                    && let Some(stream) = guard.as_mut()
                                                {
                                                    if stream
                                                        .write_all(&encode_frame(
                                                            &serde_json::json!({
                                                                "op": "resume", "thread": thread,
                                                            }),
                                                        ))
                                                        .is_ok()
                                                    {
                                                        resubscribing = Some(thread);
                                                    } else {
                                                        let _ = stream
                                                            .shutdown(std::net::Shutdown::Both);
                                                    }
                                                }
                                            }
                                            AgentEvent::Error { code, .. }
                                                if code == "no-thread" =>
                                            {
                                                if let Ok(mut guard) = active_thread.lock() {
                                                    *guard = None;
                                                }
                                                save_active_thread(state_path.as_deref(), None);
                                            }
                                            _ => {}
                                        }
                                        if events.send(Event::Agent(event)).is_err() {
                                            return;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                log::info!("SHELL agent-socket connected=false reason={e}");
                                break;
                            }
                        }
                    }
                    if let Ok(mut guard) = writer.lock() {
                        *guard = None;
                    }
                    if events.send(Event::Agent(AgentEvent::Disconnected)).is_err() {
                        break;
                    }
                }
                Err(BootstrapError::ResumeFrameTooLong { thread, bytes }) => {
                    log::warn!(
                        "Agent thread {thread} resume reply exceeded {MAX_FRAME} bytes ({bytes} bytes); reconnecting without restoring its history"
                    );
                    skipped_resume = Some((thread, SkipResumeReason::TooLong));
                    if let Ok(mut guard) = active_thread.lock() {
                        *guard = None;
                    }
                    save_active_thread(state_path.as_deref(), None);
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(BootstrapError::ResumeTimedOut { thread }) => {
                    log::warn!(
                        "Timed out restoring Agent thread {thread}; reconnecting while keeping thread selection"
                    );
                    skipped_resume = Some((thread, SkipResumeReason::TimedOut));
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                Err(BootstrapError::Fatal(err)) => {
                    if !is_routine_connect_error(&err) {
                        let msg = err.to_string();
                        if last_bootstrap_warn.as_deref() != Some(msg.as_str()) {
                            log::warn!("Agent surface bootstrap failed: {msg}");
                            last_bootstrap_warn = Some(msg);
                        }
                    }
                }
            }
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    });

    CommandSender { tx: cmd_tx }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SkipResumeReason {
    TooLong,
    TimedOut,
}

enum ResumeOutcome {
    Resumed(ResumedThread),
    NotFound,
    RecoverableError(String),
}

enum ResumeError {
    TooLong(usize),
    TimedOut,
    Fatal(anyhow::Error),
}

enum BootstrapError {
    ResumeFrameTooLong { thread: String, bytes: usize },
    ResumeTimedOut { thread: String },
    Fatal(anyhow::Error),
}

struct BootstrapResult {
    stream: UnixStream,
    status: StatusInfo,
    resumed: Option<ResumedThread>,
    preserved_thread: Option<String>,
    resume_error: Option<String>,
}

fn bootstrap_error_copy(code: &str, message: &str) -> String {
    if code == "storage-error" {
        "Could not load the previous thread right now. Try again in a moment.".into()
    } else {
        calm_turn_error(message)
    }
}

fn resume_thread_before(
    stream: &mut UnixStream,
    thread_id: &str,
    deadline: Instant,
) -> std::result::Result<ResumeOutcome, ResumeError> {
    if let Err(e) = write_frame_before(
        stream,
        &serde_json::json!({ "op": "resume", "thread": thread_id }),
        deadline,
    ) {
        return Err(match e {
            DeadlineIoError::TimedOut => ResumeError::TimedOut,
            DeadlineIoError::Other(err) => ResumeError::Fatal(err),
        });
    }
    let frame = match read_frame_before(stream, deadline) {
        Ok(f) => f,
        Err(FrameReadError::TooLong(bytes)) => return Err(ResumeError::TooLong(bytes)),
        Err(FrameReadError::TimedOut) => return Err(ResumeError::TimedOut),
        Err(FrameReadError::Other(e)) => return Err(ResumeError::Fatal(e)),
    };
    match parse_lever_message(&frame) {
        Some(LeverMessage::Resumed(r)) => Ok(ResumeOutcome::Resumed(r)),
        Some(LeverMessage::Error { code, .. }) if code == "no-thread" => {
            Ok(ResumeOutcome::NotFound)
        }
        Some(LeverMessage::Error { code, message }) => {
            log::warn!("Lever could not resume thread {thread_id} ({code}): {message}");
            Ok(ResumeOutcome::RecoverableError(bootstrap_error_copy(
                &code, &message,
            )))
        }
        other => Err(ResumeError::Fatal(anyhow::anyhow!(
            "Unexpected resume reply from Lever: {other:?}"
        ))),
    }
}

fn read_bootstrap_frame(stream: &mut UnixStream, deadline: Instant) -> Result<serde_json::Value> {
    match read_frame_before(stream, deadline) {
        Ok(frame) => Ok(frame),
        Err(FrameReadError::TooLong(bytes)) => bail!("Lever frame too long ({bytes} bytes)"),
        Err(FrameReadError::TimedOut) => bail!("Timed out waiting for Lever"),
        Err(FrameReadError::Other(err)) => Err(err),
    }
}

fn connect_and_bootstrap(
    path: &PathBuf,
    existing_thread: Option<String>,
    allow_list_fallback: bool,
    skip_resume: Option<(&str, SkipResumeReason)>,
    init_deadline: Duration,
) -> std::result::Result<BootstrapResult, BootstrapError> {
    let deadline = Instant::now() + init_deadline;
    let mut stream = UnixStream::connect(path).map_err(|e| BootstrapError::Fatal(e.into()))?;

    write_frame_before(
        &mut stream,
        &serde_json::json!({ "version": SURFACE_VERSION }),
        deadline,
    )
    .map_err(|e| BootstrapError::Fatal(e.into()))?;
    let reply = read_bootstrap_frame(&mut stream, deadline).map_err(BootstrapError::Fatal)?;
    if reply != serde_json::json!({ "version": SURFACE_VERSION }) {
        return Err(BootstrapError::Fatal(anyhow::anyhow!(
            "Lever did not speak surface protocol version {SURFACE_VERSION}"
        )));
    }

    write_frame_before(&mut stream, &serde_json::json!({ "op": "status" }), deadline)
        .map_err(|e| BootstrapError::Fatal(e.into()))?;
    let status_frame =
        read_bootstrap_frame(&mut stream, deadline).map_err(BootstrapError::Fatal)?;
    let status = match parse_lever_message(&status_frame) {
        Some(LeverMessage::Status(status)) => status,
        other => {
            return Err(BootstrapError::Fatal(anyhow::anyhow!(
                "Unexpected initial status reply from Lever: {other:?}"
            )));
        }
    };

    let mut resumed = None;
    let mut preserved_thread = None;
    let mut resume_error = None;
    if matches!(skip_resume, Some((_, SkipResumeReason::TooLong))) {
        resume_error = Some(
            "Could not load the previous thread's history because it is too large. Start a new thread or inspect it from a terminal."
                .into(),
        );
    } else if let Some(tid) = existing_thread {
        match skip_resume {
            Some((skipped, SkipResumeReason::TimedOut)) if skipped == tid => {
                preserved_thread = Some(tid);
                resume_error = Some(
                    "Timed out loading the previous thread's history. Your thread is kept — send a message to continue."
                        .into(),
                );
            }
            _ => match resume_thread_before(&mut stream, &tid, deadline) {
                Ok(ResumeOutcome::Resumed(r)) => resumed = Some(r),
                Ok(ResumeOutcome::NotFound) => {}
                Ok(ResumeOutcome::RecoverableError(msg)) => {
                    preserved_thread = Some(tid);
                    resume_error = Some(msg);
                }
                Err(ResumeError::TooLong(bytes)) => {
                    return Err(BootstrapError::ResumeFrameTooLong { thread: tid, bytes });
                }
                Err(ResumeError::TimedOut) => {
                    return Err(BootstrapError::ResumeTimedOut { thread: tid });
                }
                Err(ResumeError::Fatal(e)) => return Err(BootstrapError::Fatal(e)),
            },
        }
    } else if allow_list_fallback {
        write_frame_before(&mut stream, &serde_json::json!({ "op": "list" }), deadline)
            .map_err(|e| BootstrapError::Fatal(e.into()))?;
        let list_frame =
            read_bootstrap_frame(&mut stream, deadline).map_err(BootstrapError::Fatal)?;
        match parse_lever_message(&list_frame) {
            Some(LeverMessage::List { threads }) => {
                if let Some(chosen) = threads
                    .iter()
                    .find(|t| t.waiting)
                    .or_else(|| threads.iter().find(|t| t.working))
                    .or_else(|| threads.first())
                {
                    match skip_resume {
                        Some((skipped, SkipResumeReason::TimedOut))
                            if skipped == chosen.thread =>
                        {
                            resume_error = Some(
                                "Timed out loading the previous thread's history. Send a message to start a new thread."
                                    .into(),
                            );
                        }
                        _ => match resume_thread_before(&mut stream, &chosen.thread, deadline) {
                            Ok(ResumeOutcome::Resumed(r)) => resumed = Some(r),
                            Ok(ResumeOutcome::NotFound) => {}
                            Ok(ResumeOutcome::RecoverableError(msg)) => resume_error = Some(msg),
                            Err(ResumeError::TooLong(bytes)) => {
                                return Err(BootstrapError::ResumeFrameTooLong {
                                    thread: chosen.thread.clone(),
                                    bytes,
                                });
                            }
                            Err(ResumeError::TimedOut) => {
                                return Err(BootstrapError::ResumeTimedOut {
                                    thread: chosen.thread.clone(),
                                });
                            }
                            Err(ResumeError::Fatal(e)) => return Err(BootstrapError::Fatal(e)),
                        },
                    }
                }
            }
            Some(LeverMessage::Error { code, message }) => {
                resume_error = Some(bootstrap_error_copy(&code, &message));
            }
            other => {
                return Err(BootstrapError::Fatal(anyhow::anyhow!(
                    "Unexpected list reply from Lever: {other:?}"
                )));
            }
        }
    }

    if let Err(e) = write_frame_before(&mut stream, &serde_json::json!({ "op": "watch" }), deadline)
    {
        let e: anyhow::Error = e.into();
        log::warn!("Asking Lever to watch threads: {e}");
    }

    stream
        .set_read_timeout(None)
        .map_err(|e| BootstrapError::Fatal(e.into()))?;
    Ok(BootstrapResult {
        stream,
        status,
        resumed,
        preserved_thread,
        resume_error,
    })
}
