//! Shared subprocess and harness helpers for Lever integration tests.
#![allow(dead_code)]

pub mod fake_harness;

use serde_json::{Value, json};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub const BIN: &str = env!("CARGO_BIN_EXE_crowbar-lever");

pub const STUB_BIN: &str = env!("CARGO_BIN_EXE_crowbar-stub-harness");

pub const MAX_FRAME: usize = 4 * 1024 * 1024;

pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir();
        let root = if base.as_os_str().len() > 48 {
            PathBuf::from("/tmp")
        } else {
            base
        };
        let path = root.join(format!("cl-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct RunningLever {
    pub child: Option<Child>,
    pub state: PathBuf,
    pub surface: PathBuf,
    pub person: PathBuf,
}

impl RunningLever {
    pub fn start(dir: &Path, shell_uid: u32, person_uid: u32) -> Self {
        Self::start_with_extra(dir, shell_uid, person_uid, &[])
    }

    pub fn start_with_stub(dir: &Path, uid: u32, script: &Path, log: Option<&Path>, idle_ms: Option<u64>) -> Self {
        let mut extra: Vec<String> = vec![
            "--harness".into(),
            STUB_BIN.into(),
            "--harness-name".into(),
            "Stub".into(),
            "--harness-arg".into(),
            "--script".into(),
            "--harness-arg".into(),
            script.to_str().unwrap().into(),
        ];
        if let Some(log) = log {
            extra.push("--harness-arg".into());
            extra.push("--log".into());
            extra.push("--harness-arg".into());
            extra.push(log.to_str().unwrap().into());
        }
        if let Some(ms) = idle_ms {
            extra.push("--idle-timeout-ms".into());
            extra.push(ms.to_string());
        }
        Self::start_with_extra(dir, uid, uid, &extra)
    }

    pub fn start_with_extra(dir: &Path, shell_uid: u32, person_uid: u32, extra: &[String]) -> Self {
        Self::start_with_env(dir, shell_uid, person_uid, extra, &[])
    }

    pub fn start_with_env(dir: &Path, shell_uid: u32, person_uid: u32, extra: &[String], env: &[(&str, &str)]) -> Self {
        let state = dir.join("state");
        let surface_dir = dir.join("surface-dir");
        let person_dir = dir.join("person-dir");
        std::fs::create_dir_all(&surface_dir).unwrap();
        let surface = surface_dir.join("surface");
        let person = person_dir.join("person");

        let mut cmd = Command::new(BIN);
        cmd.args([
            "--state",
            state.to_str().unwrap(),
            "--surface-socket",
            surface.to_str().unwrap(),
            "--shell-uid",
            &shell_uid.to_string(),
            "--person-socket",
            person.to_str().unwrap(),
            "--person-uid",
            &person_uid.to_string(),
        ]);
        cmd.args(extra);
        // Lever reads the person's OpenCode settings and store: in a test, the
        // person has none, unless the test gives them some.
        let home = dir.join("lever-home");
        cmd.env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("XDG_DATA_HOME", home.join(".local/share"));
        for key in [
            "OPENCODE_CONFIG",
            "OPENCODE_CONFIG_DIR",
            "OPENCODE_CONFIG_CONTENT",
            "OPENCODE_DB",
        ] {
            cmd.env_remove(key);
        }
        cmd.envs(env.iter().copied());
        let child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn crowbar-lever");

        let mut running = Self {
            child: Some(child),
            state,
            surface,
            person,
        };
        running.wait_for_sockets();
        running
    }

    pub fn wait_for_sockets(&mut self) {
        let start_marker = format!("start: process {},", self.pid());
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let log = self.log_text();
            if let Some(pos) = log.rfind(&start_marker) {
                let tail = &log[pos..];
                if tail.contains(&format!("listening: {},", self.person.display()))
                    && tail.contains(&format!("listening: {},", self.surface.display()))
                {
                    return;
                }
            }
            if let Some(child) = self.child.as_mut()
                && let Some(status) = child.try_wait().unwrap()
            {
                let mut stderr = String::new();
                if let Some(mut s) = child.stderr.take() {
                    let _ = s.read_to_string(&mut stderr);
                }
                panic!("crowbar-lever exited early with {status}: {stderr}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting for Lever sockets");
    }

    pub fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    pub fn signal(&self, sig: libc::c_int) {
        let pid = self.pid() as libc::pid_t;
        // SAFETY: sending a signal to our own child process.
        assert_eq!(unsafe { libc::kill(pid, sig) }, 0);
    }

    pub fn wait(&mut self) -> std::process::ExitStatus {
        self.child.take().unwrap().wait().unwrap()
    }

    pub fn kill_and_wait(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn log_text(&self) -> String {
        std::fs::read_to_string(self.state.join("lever.log")).unwrap_or_default()
    }
}

impl Drop for RunningLever {
    fn drop(&mut self) {
        self.kill_and_wait();
    }
}

pub fn my_uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

pub fn encode_frame(msg: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(msg).unwrap();
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn write_frame(stream: &mut UnixStream, msg: &Value) -> io::Result<()> {
    stream.write_all(&encode_frame(msg))
}

pub fn read_frame(stream: &mut UnixStream) -> io::Result<Value> {
    read_frame_up_to(stream, MAX_FRAME)
}

pub fn read_frame_up_to(stream: &mut UnixStream, limit: usize) -> io::Result<Value> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    assert!(len <= limit, "unexpectedly large frame: {len}");
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body).unwrap())
}

pub fn connect(path: &Path) -> UnixStream {
    let stream = UnixStream::connect(path).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
    stream
}

pub fn handshake(path: &Path) -> UnixStream {
    let mut stream = connect(path);
    write_frame(&mut stream, &json!({ "version": 1 })).unwrap();
    let reply = read_frame(&mut stream).unwrap();
    assert_eq!(reply, json!({ "version": 1 }));
    stream
}

/// Sends `request` over and over, reading nothing, until the peer's writes
/// stall, and returns how many bytes went out. A write that takes part of a
/// request is carried on from where it stopped, so what the peer sent is
/// always whole requests and the start of one more.
pub fn send_until_stalled(peer: &mut UnixStream, request: &[u8]) -> usize {
    peer.set_write_timeout(Some(Duration::from_millis(500))).unwrap();
    let chunk: Vec<u8> = request.iter().copied().cycle().take(request.len() * 2048).collect();
    let mut sent = 0;
    loop {
        match peer.write(&chunk[sent % chunk.len()..]) {
            Ok(n) => sent += n,
            Err(err) if matches!(err.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return sent,
            Err(err) => panic!("Lever closed a peer that read nothing, after {sent} bytes: {err}"),
        }
        assert!(
            sent < 64 * 1024 * 1024,
            "Lever went on reading a peer that read nothing"
        );
    }
}

pub fn assert_closed_without_reply(mut stream: UnixStream) {
    let mut buf = [0u8; 64];
    match stream.read(&mut buf) {
        Ok(0) => {}
        Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe) => {}
        other => panic!("expected immediate close with no reply, got {other:?}"),
    }
}

pub fn write_script(dir: &Path, script: &Value) -> PathBuf {
    let path = dir.join("script.json");
    std::fs::write(&path, serde_json::to_string(script).unwrap()).unwrap();
    path
}

/// Reads frames until one is what is looked for, and returns it.
pub fn read_until(peer: &mut UnixStream, mut found: impl FnMut(&Value) -> bool) -> Value {
    loop {
        let frame = read_frame(peer).unwrap();
        if found(&frame) {
            return frame;
        }
    }
}

/// Sends a message, to a thread or to begin one, and returns the thread and the turn.
pub fn send(peer: &mut UnixStream, thread: Option<&str>, text: &str) -> (String, u64) {
    write_frame(
        peer,
        &json!({ "op": "send", "thread": thread, "text": text, "cwd": "/tmp" }),
    )
    .unwrap();
    let sent = read_until(peer, |frame| frame.get("reply").is_some());
    assert_eq!(sent["reply"], "sent", "{sent}");
    (
        sent["thread"].as_str().unwrap().to_string(),
        sent["turn"].as_u64().unwrap(),
    )
}

/// Reads a turn's events to its end, and returns them with the end last.
pub fn turn_events(peer: &mut UnixStream, thread: &str, turn: u64) -> Vec<Value> {
    let mut events = Vec::new();
    loop {
        let frame = read_frame(peer).unwrap();
        if frame["thread"] != thread || frame["turn"] != turn {
            continue;
        }
        let ended = ["done", "stopped", "failed", "interrupted"]
            .iter()
            .any(|end| frame["event"] == *end);
        events.push(frame);
        if ended {
            return events;
        }
    }
}

pub fn text_of(events: &[Value]) -> String {
    events
        .iter()
        .filter(|event| event["event"] == "text")
        .filter_map(|event| event["text"].as_str())
        .collect()
}

pub fn resume(peer: &mut UnixStream, thread: &str) -> Value {
    write_frame(peer, &json!({ "op": "resume", "thread": thread })).unwrap();
    read_until(peer, |frame| frame.get("reply").is_some())
}

pub fn wait_until(what: &str, mut met: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !met() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Has the harness end at once, on a thread of its own, so the next turn
/// starts a new harness process.
pub fn end_the_harness(peer: &mut UnixStream) {
    let (thread, turn) = send(peer, None, "restart the harness");
    let events = turn_events(peer, &thread, turn);
    assert_eq!(events.last().unwrap()["event"], "interrupted");
}

/// Has the database refuse a kind of write until the trigger is dropped.
pub fn refuse_writes(db: &rusqlite::Connection, what: &str) {
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch(&format!(
        "CREATE TRIGGER refused BEFORE {what} BEGIN SELECT RAISE(ABORT, 'refused by the test'); END"
    ))
    .unwrap();
}

/// Lever's arguments for the stub harness with `script`, its log, and more.
pub fn stub_args(script: &Path, log: &Path, more: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = [
        "--harness",
        STUB_BIN,
        "--harness-name",
        "OpenCode",
        "--harness-arg",
        "--script",
        "--harness-arg",
        script.to_str().unwrap(),
        "--harness-arg",
        "--log",
        "--harness-arg",
        log.to_str().unwrap(),
    ]
    .map(str::to_string)
    .to_vec();
    args.extend(more.iter().map(|s| s.to_string()));
    args
}

/// Sends `op` and reads frames until its reply, which it returns.
pub fn ask_lever(peer: &mut UnixStream, op: Value) -> Value {
    write_frame(peer, &op).unwrap();
    read_until(peer, |f| f.get("reply").is_some())
}

/// The answers the stub harness gave Lever's requests, in its log: the
/// option each chose, or `cancelled`.
pub fn stub_heard(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v.get("method").is_none())
        .filter_map(|v| v.pointer("/result/outcome").cloned())
        .map(|o| {
            o.get("optionId")
                .and_then(Value::as_str)
                .unwrap_or_else(|| o["outcome"].as_str().unwrap_or("?"))
                .to_string()
        })
        .collect()
}

/// The modes Lever set on the stub's sessions, in its log, as the harness's values.
pub fn stub_modes_set(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v["method"] == "session/set_config_option" && v["params"]["configId"] == "mode")
        .map(|v| v["params"]["value"].as_str().unwrap_or("").to_string())
        .collect()
}

pub fn ask_script(dir: &Path) -> PathBuf {
    write_script(
        dir,
        &json!({
            "modes": true,
            "turns": [
                {
                    "match": "please run",
                    "steps": [
                        { "kind": "ask", "id": "c1", "title": "echo hi", "raw_input": { "command": "echo hi && id -un" }, "output": "hi" },
                        { "kind": "text", "text": "Ran." }
                    ]
                },
                {
                    "match": "please edit",
                    "steps": [
                        { "kind": "ask", "id": "c2", "tool_kind": "edit", "title": "/w/seed.txt",
                          "diff": { "path": "/w/seed.txt", "old": "alpha\n", "new": "beta\n" } },
                        { "kind": "text", "text": "Edited." }
                    ]
                },
                { "match": "please stall", "steps": [{ "kind": "text", "text": "Stalling" }], "stall": true },
                {
                    "match": "please switch",
                    "steps": [{ "kind": "mode_change", "mode": "build" }, { "kind": "text", "text": "Switched." }]
                }
            ]
        }),
    )
}

/// Whether a process runs, and is not a zombie.
pub fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| stat.rfind(')').map(|at| stat[at + 1..].trim_start().starts_with('Z')))
        .is_some_and(|zombie| !zombie)
}

/// What the stub harness was asked to set of `config` on its sessions, in its
/// log, in order.
pub fn stub_config_set(log: &Path, config: &str) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v["method"] == "session/set_config_option" && v["params"]["configId"] == config)
        .filter_map(|v| v["params"]["value"].as_str().map(str::to_string))
        .collect()
}

/// The prompts the stub harness was sent, in its log, each as one text.
pub fn stub_prompts(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v["method"] == "session/prompt")
        .map(|v| {
            v["params"]["prompt"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect()
}

/// Lever's arguments for the stub harness with `script`, and `more`.
pub fn stub_extra(script: &Path, more: &[&str]) -> Vec<String> {
    let mut extra: Vec<String> = [
        "--harness",
        STUB_BIN,
        "--harness-name",
        "Stub",
        "--harness-arg",
        "--script",
        "--harness-arg",
        script.to_str().unwrap(),
    ]
    .map(str::to_string)
    .to_vec();
    extra.extend(more.iter().map(|s| s.to_string()));
    extra
}

/// The harness's JSON-RPC traffic as the stub logged what it was sent.
pub fn rpc_sent(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}
