//! `crowbar-agent` against the Lever binary (`crowbar-lever`) and the Rust
//! scripted ACP stub harness (`crowbar-stub-harness`), over a real person
//! socket in a temporary directory. No OpenCode and no network needed.

use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

fn uid() -> u32 {
    // SAFETY: `getuid` takes nothing and cannot fail.
    unsafe { libc::getuid() }
}

fn workspace_bin(name: &str) -> PathBuf {
    let agent = Path::new(env!("CARGO_BIN_EXE_crowbar-agent"));
    let dir = agent.parent().expect("crowbar-agent lives in target/<profile>");
    let bin = dir.join(name);
    assert!(
        bin.exists(),
        "{} is missing beside {} (run `cargo test` from `lever/`)",
        bin.display(),
        agent.display()
    );
    bin
}

/// Waits for `met`, for as long as ten seconds.
fn wait_for(what: &str, mut met: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !met() {
        assert!(Instant::now() < deadline, "waited ten seconds for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A Lever process and its temporary state/socket directory. Everything stops
/// and is removed when dropped.
struct Lab {
    directory: PathBuf,
    children: Vec<Child>,
}

impl Drop for Lab {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.directory);
    }
}

impl Lab {
    fn empty() -> Lab {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let name = format!("ca-{}-{}", std::process::id(), COUNT.fetch_add(1, Ordering::Relaxed));
        let base = std::env::temp_dir();
        let root = if base.as_os_str().len() > 48 {
            PathBuf::from("/tmp")
        } else {
            base
        };
        let directory = root.join(name);
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(directory.join("runtime")).expect("runtime directory");
        Lab {
            directory,
            children: Vec::new(),
        }
    }

    fn state(&self) -> PathBuf {
        self.directory.join("state")
    }

    fn rpc_log(&self) -> PathBuf {
        self.directory.join("rpc.jsonl")
    }

    /// Starts `crowbar-lever`, optionally wired to `crowbar-stub-harness` with `script`.
    fn start_lever(&mut self, person_uid: u32, script: Option<&Value>) {
        let mut cmd = Command::new(workspace_bin("crowbar-lever"));
        cmd.arg("--state")
            .arg(self.state())
            .arg("--surface-socket")
            .arg(self.directory.join("surface"))
            .args(["--shell-uid", &uid().to_string()])
            .arg("--person-socket")
            .arg(self.directory.join("runtime/crowbar-lever/person"))
            .args(["--person-uid", &person_uid.to_string()]);

        if let Some(script) = script {
            let script_path = self.directory.join("script.json");
            fs::write(&script_path, serde_json::to_string(script).unwrap()).expect("write stub script");
            cmd.arg("--harness")
                .arg(workspace_bin("crowbar-stub-harness"))
                .args(["--harness-name", "Stub"])
                .args(["--harness-arg", "--script"])
                .arg("--harness-arg")
                .arg(&script_path)
                .args(["--harness-arg", "--log"])
                .arg("--harness-arg")
                .arg(self.rpc_log());
        }

        let child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn crowbar-lever");
        let pid = child.id();
        self.children.push(child);

        let log = self.state().join("lever.log");
        let start_marker = format!("start: process {pid},");
        wait_for("Lever to listen", || {
            fs::read_to_string(&log).is_ok_and(|text| {
                text.rfind(&start_marker)
                    .is_some_and(|pos| text[pos..].matches(" listening: ").count() == 2)
            })
        });
    }

    fn with_script(script: Value) -> Lab {
        let mut lab = Lab::empty();
        lab.start_lever(uid(), Some(&script));
        lab
    }

    fn agent(&self, args: &[&str]) -> Command {
        let mut agent = Command::new(env!("CARGO_BIN_EXE_crowbar-agent"));
        agent
            .args(args)
            .env("XDG_RUNTIME_DIR", self.directory.join("runtime"))
            .current_dir(&self.directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        agent
    }

    fn run(&self, args: &[&str], typed: &str) -> Ran {
        Ran::of(run(self.agent(args), typed))
    }

    fn rpc_messages(&self) -> String {
        fs::read_to_string(self.rpc_log()).unwrap_or_default()
    }
}

fn run(mut command: Command, typed: &str) -> Output {
    let mut child = command.spawn().expect("crowbar-agent runs");
    child
        .stdin
        .take()
        .expect("its input")
        .write_all(typed.as_bytes())
        .expect("typing");
    child.wait_with_output().expect("crowbar-agent ends")
}

#[derive(Debug)]
struct Ran {
    status: i32,
    out: String,
    err: String,
}

impl Ran {
    fn of(output: Output) -> Ran {
        Ran {
            status: output.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}

struct Running {
    child: Child,
    out: Arc<Mutex<String>>,
    err: Arc<Mutex<String>>,
    readers: Vec<JoinHandle<()>>,
}

fn collect(stream: impl Read + Send + 'static) -> (Arc<Mutex<String>>, JoinHandle<()>) {
    let collected = Arc::new(Mutex::new(String::new()));
    let into = collected.clone();
    let reader = std::thread::spawn(move || {
        let mut stream = stream;
        let mut bytes = [0u8; 4096];
        while let Ok(count @ 1..) = stream.read(&mut bytes) {
            into.lock().unwrap().push_str(&String::from_utf8_lossy(&bytes[..count]));
        }
    });
    (collected, reader)
}

impl Running {
    fn start(mut command: Command) -> Running {
        let mut child = command.spawn().expect("crowbar-agent runs");
        let (out, out_reader) = collect(child.stdout.take().expect("its output"));
        let (err, err_reader) = collect(child.stderr.take().expect("its errors"));
        Running {
            child,
            out,
            err,
            readers: vec![out_reader, err_reader],
        }
    }

    fn signal(&self, signal: libc::c_int) {
        // SAFETY: sending a signal to our own child process.
        unsafe { libc::kill(self.child.id() as libc::pid_t, signal) };
    }

    fn wait_for_out(&self, text: &str) {
        wait_for(&format!("{text:?} on stdout"), || {
            self.out.lock().unwrap().contains(text)
        });
    }

    fn wait_for_err(&self, text: &str) {
        wait_for(&format!("{text:?} on stderr"), || {
            self.err.lock().unwrap().contains(text)
        });
    }

    fn ended(&mut self) -> i32 {
        let status = self.child.wait().expect("crowbar-agent ends");
        for reader in self.readers.drain(..) {
            reader.join().expect("collect output");
        }
        status.code().unwrap_or(-1)
    }
}

fn terminal() -> (fs::File, OwnedFd) {
    let (mut keys, mut screen) = (0, 0);
    // SAFETY: `openpty` initializes two valid file descriptors.
    let opened = unsafe {
        libc::openpty(
            &mut keys,
            &mut screen,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(opened, 0, "openpty");
    // SAFETY: both descriptors are newly opened and owned by us.
    unsafe { (fs::File::from_raw_fd(keys), OwnedFd::from_raw_fd(screen)) }
}

fn only_thread(lab: &Lab) -> String {
    let listed = lab.run(&["--list"], "");
    let lines: Vec<&str> = listed.out.lines().collect();
    assert_eq!(lines.len(), 1, "{listed:?}");
    lines[0].split_whitespace().next().expect("a thread ID").to_string()
}

#[test]
fn one_request_streams_answer_acts_and_unknown_updates_cleanly() {
    let lab = Lab::with_script(json!({
        "turns": [{
            "steps": [
                { "kind": "act", "id": "acp-call-1", "tool_kind": "execute", "title": "List files", "status": "in_progress" },
                { "kind": "act_update", "id": "acp-call-1", "status": "completed", "content": "README.md" },
                { "kind": "unknown", "update": { "sessionUpdate": "vendor_progress", "summary": "Cache warm" } },
                { "kind": "text", "text": "Hello from " },
                { "kind": "text", "text": "the stub harness." }
            ]
        }]
    }));
    let ran = lab.run(&["Hello,", "Agent."], "");
    assert_eq!(ran.status, 0, "{ran:?}");
    assert_eq!(ran.out, "Hello from the stub harness.\n");
    assert!(ran.err.contains("[act] List files (working)\n"), "{ran:?}");
    assert!(ran.err.contains("[act] List files (done): README.md\n"), "{ran:?}");
    assert!(ran.err.contains("[update] Cache warm\n"), "{ran:?}");
    assert!(
        lab.rpc_messages().contains("Hello, Agent."),
        "stub must receive prompt: {}",
        lab.rpc_messages()
    );
}

#[test]
fn a_thread_goes_a_line_at_a_time_and_is_listed_and_resumed() {
    let lab = Lab::with_script(json!({
        "turns": [
            { "steps": [{ "kind": "text", "text": "The first answer." }] },
            { "steps": [{ "kind": "text", "text": "The second answer." }] },
            { "steps": [{ "kind": "text", "text": "The third." }] }
        ]
    }));

    // Leaving the REPL immediately starts no thread.
    assert_eq!(lab.run(&[], "").status, 0);
    let none = lab.run(&["--list"], "");
    assert_eq!(
        (none.status, none.out.as_str(), none.err.as_str()),
        (0, "", "No threads yet.\n")
    );

    let talked = lab.run(&[], "What is first?\n\nAnd second?\n");
    assert_eq!(
        (talked.status, talked.out.as_str()),
        (0, "The first answer.\nThe second answer.\n"),
        "{talked:?}"
    );

    let listed = lab.run(&["--list"], "");
    let thread = only_thread(&lab);
    assert_eq!(
        listed.out,
        format!("{thread}  just now        2 turns  What is first?\n")
    );

    let resumed = lab.run(&["--resume", &thread], "");
    assert_eq!(
        resumed.out,
        "> What is first?\nThe first answer.\n> And second?\nThe second answer.\n"
    );
    let more = lab.run(&["--resume", &thread, "And third?"], "");
    assert_eq!((more.status, more.out.as_str()), (0, "The third.\n"), "{more:?}");
    assert!(lab.run(&["--list"], "").out.contains("3 turns"));

    let missing = lab.run(&["--resume", "nosuchid", "Hello?"], "");
    assert_eq!(
        (missing.status, missing.err.as_str()),
        (1, "crowbar-agent: There is no thread nosuchid.\n")
    );
}

#[test]
fn stop_flag_and_ctrl_c_stop_working_turns_and_a_departing_client_does_not() {
    let lab = Lab::with_script(json!({
        "turns": [
            { "steps": [{ "kind": "text", "text": "One" }], "stall": true },
            { "steps": [{ "kind": "text", "text": "Two" }], "stall": true }
        ]
    }));

    // A client killed by SIGTERM does not stop the turn.
    let mut gone = Running::start(lab.agent(&["Count."]));
    gone.wait_for_out("One");
    gone.signal(libc::SIGTERM);
    assert_eq!(gone.ended(), -1);
    std::thread::sleep(Duration::from_millis(150));
    assert!(lab.run(&["--list"], "").out.ends_with("(working)\n"));

    // `--stop ID` stops the working turn from another terminal.
    let thread = only_thread(&lab);
    let stopped = lab.run(&["--stop", &thread], "");
    assert_eq!((stopped.status, stopped.err.as_str()), (0, "Stopped.\n"));
    wait_for("thread to leave working state", || {
        lab.run(&["--list"], "").out.contains("(stopped)")
    });

    // Ctrl-C in a client's own turn stops it with exit status 130.
    let mut own = Running::start(lab.agent(&["--resume", &thread, "Count on."]));
    own.wait_for_out("Two");
    own.signal(libc::SIGINT);
    assert_eq!(own.ended(), 130);
    assert_eq!(
        (own.out.lock().unwrap().as_str(), own.err.lock().unwrap().as_str()),
        ("Two\n", "Stopped.\n")
    );
    let stood = lab.run(&["--resume", &thread], "");
    assert_eq!(stood.out, "> Count.\nOne\n> Count on.\nTwo\n");
    assert_eq!(stood.err, "(stopped)\n(stopped)\n");
}

#[test]
fn two_threads_work_at_the_same_time_from_separate_clients() {
    let lab = Lab::with_script(json!({
        "turns": [
            { "match": "alpha", "steps": [{ "kind": "text", "text": "Alpha started" }], "stall": true },
            { "match": "beta", "steps": [{ "kind": "text", "text": "Beta finished." }] }
        ]
    }));

    let mut first = Running::start(lab.agent(&["Start alpha"]));
    first.wait_for_out("Alpha started");

    // While alpha is working, beta runs to completion on its own thread.
    let second = lab.run(&["Run beta"], "");
    assert_eq!(
        (second.status, second.out.as_str()),
        (0, "Beta finished.\n"),
        "{second:?}"
    );

    let listed = lab.run(&["--list"], "");
    assert_eq!(listed.out.lines().count(), 2, "{listed:?}");
    assert!(listed.out.contains("(working)"), "{listed:?}");

    first.signal(libc::SIGINT);
    assert_eq!(first.ended(), 130);
}

#[test]
fn control_characters_in_answers_acts_and_notes_cannot_drive_the_terminal() {
    let lab = Lab::with_script(json!({
        "turns": [{
            "steps": [
                { "kind": "act", "id": "c1", "title": "Run \u{001b}[31mred", "status": "completed", "content": "ok\u{0007}" },
                { "kind": "unknown", "update": { "sessionUpdate": "x", "summary": "Note \u{001b}[2J" } },
                { "kind": "text", "text": "Clear \u{001b}[2Jthis, \u{001b}]52;c;aGk=\u{0007}and that." }
            ]
        }]
    }));
    let shown = "Clear ␛[2Jthis, ␛]52;c;aGk=␇and that.\n";
    let ran = lab.run(&["Hello?"], "");
    assert_eq!((ran.status, ran.out.as_str()), (0, shown), "{ran:?}");
    assert!(ran.err.contains("[act] Run ␛[31mred (done): ok␇\n"), "{ran:?}");
    assert!(ran.err.contains("[update] Note ␛[2J\n"), "{ran:?}");

    let stood = lab.run(&["--resume", &only_thread(&lab)], "");
    assert_eq!(stood.out, format!("> Hello?\n{shown}"), "{stood:?}");
}

#[test]
fn another_clients_turn_streams_live_and_ctrl_d_leaves_without_stopping_it() {
    let lab = Lab::with_script(json!({
        "turns": [
            { "steps": [{ "kind": "text", "text": "Here." }] },
            { "steps": [{ "kind": "text", "text": "One two" }], "stall": true }
        ]
    }));
    assert_eq!(lab.run(&["Hello?"], "").status, 0);
    let thread = only_thread(&lab);

    let (mut keys, screen) = terminal();
    let mut shown = lab.agent(&["--resume", &thread]);
    shown.stdin(screen);
    let mut shown = Running::start(shown);
    shown.wait_for_out("> Hello?\nHere.\n");

    let mut other = Running::start(lab.agent(&["--resume", &thread, "Count."]));
    shown.wait_for_out("\n> Count.\nOne two");

    // Ctrl-D leaves without stopping the other client's turn.
    keys.write_all(&[0x04]).expect("Ctrl-D");
    wait_for("resumed client to exit", || {
        shown.child.try_wait().expect("try_wait").is_some()
    });
    assert_eq!(shown.ended(), 0);
    assert!(lab.run(&["--list"], "").out.ends_with("(working)\n"));

    other.signal(libc::SIGINT);
    other.wait_for_err("Stopped.\n");
    assert_eq!(other.ended(), 130);
}

#[test]
fn lever_killed_mid_turn_leaves_turn_interrupted_in_client() {
    let mut lab = Lab::with_script(json!({
        "turns": [
            { "steps": [{ "kind": "text", "text": "Halfway" }], "stall": true },
            { "steps": [{ "kind": "text", "text": "Fresh answer." }] }
        ]
    }));
    let mut active = Running::start(lab.agent(&["Long job"]));
    active.wait_for_out("Halfway");

    // Kill Lever mid-turn, then restart it against the same state directory.
    let mut old_lever = lab.children.pop().expect("Lever child");
    let _ = old_lever.kill();
    let _ = old_lever.wait();
    assert_eq!(active.ended(), 1);

    lab.start_lever(
        uid(),
        Some(&json!({
            "turns": [{ "steps": [{ "kind": "text", "text": "Fresh answer." }] }]
        })),
    );
    let listed = lab.run(&["--list"], "");
    assert!(listed.out.contains("(interrupted)"), "{listed:?}");
    let thread = only_thread(&lab);
    let resumed = lab.run(&["--resume", &thread], "");
    assert_eq!(resumed.status, 1, "{resumed:?}");
    assert!(resumed.err.contains("(interrupted)"), "{resumed:?}");

    let next = lab.run(&["--resume", &thread, "Continue"], "");
    assert_eq!((next.status, next.out.as_str()), (0, "Fresh answer.\n"), "{next:?}");
}

#[test]
fn refused_account_missing_lever_no_harness_and_errors_are_reported_clearly() {
    // Missing Lever starts nothing.
    let empty = Lab::empty();
    let missing = empty.run(&["Hello?"], "");
    assert_eq!(missing.status, 1);
    assert!(missing.err.contains("Lever is not running"), "{missing:?}");

    // Lever serving another UID refuses the connection.
    let mut wrong = Lab::empty();
    wrong.start_lever(uid() + 1, None);
    UnixStream::connect(wrong.directory.join("runtime/crowbar-lever/person")).expect("person socket exists");
    let refused = wrong.run(&["--list"], "");
    assert_eq!((refused.status, refused.out.as_str()), (1, ""));
    assert!(
        refused.err.contains("closed the connection: it serves another account"),
        "{refused:?}"
    );

    // Lever running with no harness configured.
    let mut no_harness = Lab::empty();
    no_harness.start_lever(uid(), None);
    let st = no_harness.run(&["status"], "");
    assert_eq!(st.status, 1, "{st:?}");
    assert!(st.out.contains("no harness available"), "{st:?}");
    let refused_send = no_harness.run(&["Hello?"], "");
    assert_eq!(refused_send.status, 1, "{refused_send:?}");
    assert!(
        refused_send.err.contains("No harness is available."),
        "{refused_send:?}"
    );

    // Harness error and recovery on the next turn.
    let lab = Lab::with_script(json!({
        "turns": [
            { "error": { "code": -32000, "message": "Backend unavailable" } },
            { "steps": [{ "kind": "text", "text": "Recovered." }] }
        ]
    }));
    let failed = lab.run(&["First"], "");
    assert_eq!((failed.status, failed.out.as_str()), (1, ""));
    assert!(failed.err.contains("Backend unavailable"), "{failed:?}");
    let thread = only_thread(&lab);
    let ok = lab.run(&["--resume", &thread, "Second"], "");
    assert_eq!((ok.status, ok.out.as_str()), (0, "Recovered.\n"), "{ok:?}");

    let status = lab.run(&["status"], "");
    assert_eq!(status.status, 0, "{status:?}");
    assert!(status.out.contains("Stub"), "{status:?}");
}

/// Extracts every fenced ```json block from `lever/README.md`.
fn readme_json_blocks() -> Vec<Value> {
    let readme_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("client lives inside lever/")
        .join("README.md");
    let readme = fs::read_to_string(&readme_path).expect("read lever/README.md");
    let mut blocks = Vec::new();
    let mut in_json = false;
    let mut buf = String::new();
    for line in readme.lines() {
        let trimmed = line.trim();
        if !in_json && trimmed == "```json" {
            in_json = true;
            buf.clear();
        } else if in_json && trimmed == "```" {
            in_json = false;
            blocks.push(
                serde_json::from_str::<Value>(&buf)
                    .unwrap_or_else(|e| panic!("invalid JSON block in lever/README.md: {e}\n{buf}")),
            );
        } else if in_json {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    blocks
}

fn read_framed(stream: &mut UnixStream) -> Value {
    let mut hdr = [0u8; 4];
    stream.read_exact(&mut hdr).expect("read header");
    let len = u32::from_be_bytes(hdr) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).expect("read body");
    serde_json::from_slice(&body).expect("parse JSON frame")
}

fn write_framed(stream: &mut UnixStream, value: &Value) {
    let body = value.to_string();
    let len = body.len() as u32;
    stream.write_all(&len.to_be_bytes()).expect("write header");
    stream.write_all(body.as_bytes()).expect("write body");
}

#[test]
fn client_speaks_and_reads_every_message_documented_in_readme() {
    use std::collections::BTreeSet;
    use std::os::unix::net::UnixListener;

    let blocks = readme_json_blocks();
    let doc_ops: BTreeSet<String> = blocks
        .iter()
        .filter_map(|v| v.get("op").and_then(Value::as_str).map(str::to_string))
        .collect();
    let find_reply = |name: &str| -> Value {
        blocks
            .iter()
            .find(|v| v.get("reply").and_then(Value::as_str) == Some(name))
            .unwrap_or_else(|| panic!("missing reply {name:?} in lever/README.md"))
            .clone()
    };
    let find_event = |name: &str| -> Value {
        blocks
            .iter()
            .find(|v| v.get("event").and_then(Value::as_str) == Some(name))
            .unwrap_or_else(|| panic!("missing event {name:?} in lever/README.md"))
            .clone()
    };

    let lab = Lab::empty();
    let sock_dir = lab.directory.join("runtime/crowbar-lever");
    fs::create_dir_all(&sock_dir).unwrap();
    let sock_path = sock_dir.join("person");
    let listener = UnixListener::bind(&sock_path).expect("bind mock Lever socket");

    let sent_ops = Arc::new(Mutex::new(BTreeSet::<String>::new()));
    let ops_ref = sent_ops.clone();
    let reply_status = find_reply("status");
    let reply_list = find_reply("list");
    let reply_sent = find_reply("sent");
    let reply_resumed = find_reply("resumed");
    let reply_ok = find_reply("ok");
    let reply_error = find_reply("error");
    let ev_message = find_event("message");
    let ev_act = find_event("act");
    let ev_note = find_event("note");
    let ev_updated = find_event("thread-updated");
    let ev_text = find_event("text");
    let ev_done = find_event("done");
    let ev_stopped = find_event("stopped");
    let ev_failed = find_event("failed");
    let ev_interrupted = find_event("interrupted");

    let server_ops = doc_ops.clone();
    // Serve 8 short connections exercising every documented op, reply, and event.
    let server = std::thread::spawn(move || {
        for turn_case in 0..8 {
            let (mut stream, _) = listener.accept().expect("accept client");
            let ver = read_framed(&mut stream);
            assert_eq!(ver, json!({ "version": 1 }));
            write_framed(&mut stream, &json!({ "version": 1 }));

            let req = read_framed(&mut stream);
            let op = req["op"].as_str().expect("op").to_string();
            assert!(server_ops.contains(&op), "client sent undocumented op {op:?}");
            ops_ref.lock().unwrap().insert(op.clone());

            match (turn_case, op.as_str()) {
                (0, "status") => write_framed(&mut stream, &reply_status),
                (1, "list") => write_framed(&mut stream, &reply_list),
                (2, "resume") => write_framed(&mut stream, &reply_resumed),
                (3, "stop") => write_framed(&mut stream, &reply_ok),
                (4, "send") => {
                    write_framed(&mut stream, &reply_sent);
                    for ev in [&ev_message, &ev_act, &ev_note, &ev_updated, &ev_text, &ev_done] {
                        write_framed(&mut stream, ev);
                    }
                }
                (5, "send") => {
                    write_framed(&mut stream, &reply_sent);
                    write_framed(&mut stream, &ev_stopped);
                }
                (6, "send") => {
                    write_framed(&mut stream, &reply_sent);
                    write_framed(&mut stream, &ev_failed);
                }
                (7, "send") => {
                    write_framed(&mut stream, &reply_sent);
                    write_framed(&mut stream, &ev_interrupted);
                }
                _ => write_framed(&mut stream, &reply_error),
            }
            // Hold the connection open until the client closes it, as Lever does.
            let mut drain = [0u8; 1];
            let _ = stream.read(&mut drain);
        }
    });

    assert_eq!(lab.run(&["status"], "").status, 0);
    assert_eq!(lab.run(&["--list"], "").status, 0);
    let resumed = lab.run(&["--resume", "t-01a2b3c4d5e6"], "");
    assert_eq!(resumed.status, 0, "{resumed:?}");
    assert!(resumed.out.contains("Here are the files."), "{resumed:?}");
    assert!(resumed.err.contains("[act] Run ls (done): Cargo.toml"), "{resumed:?}");
    assert!(resumed.err.contains("[update] Cache warm"), "{resumed:?}");

    assert_eq!(lab.run(&["--stop", "t-01a2b3c4d5e6"], "").status, 0);

    let done = lab.run(&["List the files"], "");
    assert_eq!(done.status, 0, "{done:?}");
    assert_eq!(done.out, "Here are the files.\n");
    assert!(done.err.contains("[act] Run ls (done): Cargo.toml"), "{done:?}");
    assert!(done.err.contains("[update] Cache warm"), "{done:?}");

    let stopped = lab.run(&["Stop me"], "");
    assert_eq!(stopped.status, 130, "{stopped:?}");
    assert!(stopped.err.contains("Stopped."), "{stopped:?}");

    let failed = lab.run(&["Fail me"], "");
    assert_eq!(failed.status, 1, "{failed:?}");
    assert!(failed.err.contains("The harness reported an error."), "{failed:?}");

    let interrupted = lab.run(&["Interrupt me"], "");
    assert_eq!(interrupted.status, 1, "{interrupted:?}");
    assert!(interrupted.err.contains("Interrupted."), "{interrupted:?}");

    server.join().unwrap();
    assert_eq!(*sent_ops.lock().unwrap(), doc_ops);
}
