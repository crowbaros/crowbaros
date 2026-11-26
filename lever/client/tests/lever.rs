//! `crowbar-agent` against the Lever binary (`crowbar-lever`) and the Rust
//! scripted ACP stub harness (`crowbar-stub-harness`), over a real person
//! socket in a temporary directory. No OpenCode and no network needed.

use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
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

        // Lever reads the person's OpenCode settings: here the person has none.
        let home = self.directory.join("home");
        cmd.env("HOME", &home).env("XDG_CONFIG_HOME", home.join(".config"));
        for key in ["OPENCODE_CONFIG", "OPENCODE_CONFIG_DIR", "OPENCODE_CONFIG_CONTENT"] {
            cmd.env_remove(key);
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
fn an_act_in_the_middle_of_a_line_adds_nothing_to_a_piped_answer_and_ends_the_line_on_a_terminal() {
    let lab = Lab::with_script(json!({
        "turns": [
            { "steps": [
                { "kind": "text", "text": "Hel" },
                { "kind": "act", "id": "acp-call-1", "title": "Look", "status": "completed" },
                { "kind": "unknown", "update": { "sessionUpdate": "vendor_progress", "summary": "Halfway" } },
                { "kind": "text", "text": "lo." }
            ] },
            { "steps": [
                { "kind": "text", "text": "Hel" },
                { "kind": "act", "id": "acp-call-2", "title": "Look", "status": "completed" },
                { "kind": "text", "text": "lo." }
            ] }
        ]
    }));

    // Down a pipe, standard output is the answer and nothing else.
    let piped = lab.run(&["Hello?"], "");
    assert_eq!((piped.status, piped.out.as_str()), (0, "Hello.\n"), "{piped:?}");
    assert_eq!(piped.err, "[act] Look (done)\n[update] Halfway\n", "{piped:?}");

    // On one terminal the act would land in the answer's open line, so that
    // line is ended first.
    let (mut keys, screen) = terminal();
    let mut on_terminal = lab.agent(&["Hello again?"]);
    on_terminal
        .stdin(Stdio::null())
        .stdout(screen.try_clone().expect("the terminal again"))
        .stderr(screen);
    let mut child = on_terminal.spawn().expect("crowbar-agent runs");
    // The command holds the terminal's other end until it is dropped.
    drop(on_terminal);
    let mut shown = Vec::new();
    let mut bytes = [0u8; 4096];
    // Reading ends with an error when the last holder of the terminal has gone.
    while let Ok(count @ 1..) = keys.read(&mut bytes) {
        shown.extend_from_slice(&bytes[..count]);
    }
    assert_eq!(child.wait().expect("crowbar-agent ends").code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&shown), "Hel\r\n[act] Look (done)\r\nlo.\r\n");
}

#[test]
fn an_act_that_ends_after_the_next_turn_began_is_still_shown() {
    let lab = Lab::with_script(json!({
        "turns": [
            {
                "steps": [
                    { "kind": "act", "id": "acp-call-1", "title": "Look", "status": "in_progress" },
                    { "kind": "text", "text": "First." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 400 },
                    { "kind": "act_update", "id": "acp-call-1", "status": "completed", "content": "found" }
                ]
            },
            { "steps": [
                { "kind": "text", "text": "Second" },
                { "kind": "pause_ms", "ms": 1000 },
                { "kind": "text", "text": "." }
            ] }
        ]
    }));
    let ran = lab.run(&[], "One\nTwo\n");
    assert_eq!((ran.status, ran.out.as_str()), (0, "First.\nSecond.\n"), "{ran:?}");
    assert_eq!(
        ran.err, "[act] Look (working)\n[act] Look (done): found\n",
        "the act's end came while the second turn was watched: {ran:?}"
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
    assert_eq!(
        (stopped.status, stopped.err.as_str()),
        (0, "Asked the Agent to stop.\n")
    );
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
fn ctrl_d_leaves_a_turn_that_was_working_when_its_thread_was_resumed() {
    let lab = Lab::with_script(json!({
        "turns": [{ "steps": [{ "kind": "text", "text": "One two" }], "stall": true }]
    }));
    let mut other = Running::start(lab.agent(&["Count."]));
    other.wait_for_out("One two");
    let thread = only_thread(&lab);

    let (mut keys, screen) = terminal();
    let mut shown = lab.agent(&["--resume", &thread]);
    shown.stdin(screen);
    let mut shown = Running::start(shown);
    shown.wait_for_out("> Count.\nOne two");

    // Ctrl-D leaves, and the turn goes on.
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
    let reply_models = find_reply("models");
    let reply_free_models = find_reply("free-models");
    let reply_model_set = find_reply("model-set");
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
    let ev_request = find_event("request");
    let reply_activity = find_reply("activity");
    let reply_mode_set = find_reply("mode-set");
    let reply_default_mode = find_reply("default-mode");

    let server_ops = doc_ops.clone();
    // Serve 16 short connections exercising every documented op, reply (including error), and event.
    let server = std::thread::spawn(move || {
        for turn_case in 0..16 {
            let (mut stream, _) = listener.accept().expect("accept client");
            let ver = read_framed(&mut stream);
            assert_eq!(ver, json!({ "version": 1 }));
            write_framed(&mut stream, &json!({ "version": 1 }));

            let req = read_framed(&mut stream);
            let op = req["op"].as_str().expect("op").to_string();
            assert!(server_ops.contains(&op), "client sent undocumented op {op:?}");
            ops_ref.lock().unwrap().insert(op.clone());

            match (turn_case, op.as_str()) {
                (0, "status") => {
                    write_framed(&mut stream, &reply_status);
                    let watch = read_framed(&mut stream);
                    assert_eq!(watch, json!({ "op": "watch" }));
                    ops_ref.lock().unwrap().insert("watch".to_string());
                    write_framed(&mut stream, &reply_activity);
                }
                (1, "list") => write_framed(&mut stream, &reply_list),
                (2, "resume") => write_framed(&mut stream, &reply_resumed),
                (3, "stop") => write_framed(&mut stream, &reply_ok),
                (4, "send") => {
                    write_framed(&mut stream, &reply_sent);
                    for ev in [
                        &ev_message,
                        &ev_act,
                        &ev_request,
                        &ev_note,
                        &ev_updated,
                        &ev_text,
                        &ev_done,
                    ] {
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
                (8, "resume") => write_framed(&mut stream, &reply_error),
                // One more turn of a thread is sent to it at once: its record is not asked for first.
                (9, "send") => {
                    assert_eq!(req["thread"], "t-01a2b3c4d5e6");
                    write_framed(&mut stream, &reply_sent);
                    write_framed(&mut stream, &ev_text);
                    write_framed(&mut stream, &ev_done);
                }
                (10, "models") => write_framed(&mut stream, &reply_models),
                (11, "set-free-models") => write_framed(&mut stream, &reply_free_models),
                (12, "set-model") => write_framed(&mut stream, &reply_model_set),
                (13, "answer") => write_framed(&mut stream, &reply_ok),
                (14, "set-mode") => write_framed(&mut stream, &reply_mode_set),
                (15, "set-default-mode") => write_framed(&mut stream, &reply_default_mode),
                other => panic!("unexpected connection {other:?}"),
            }
            // Hold the connection open until the client closes it, as Lever does.
            let mut drain = [0u8; 1];
            let _ = stream.read(&mut drain);
        }
    });

    let status = lab.run(&["status"], "");
    assert_eq!(status.status, 0, "{status:?}");
    assert!(status.out.contains("New threads start in Ask first."), "{status:?}");
    assert!(status.out.contains("Working: t-01a2b3c4d5e6."), "{status:?}");
    assert_eq!(lab.run(&["--list"], "").status, 0);
    let resumed = lab.run(&["--resume", "t-01a2b3c4d5e6"], "");
    assert_eq!(resumed.status, 0, "{resumed:?}");
    assert!(resumed.out.contains("Here are the files."), "{resumed:?}");
    assert!(resumed.err.contains("[act] Run ls (done): Cargo.toml"), "{resumed:?}");
    assert!(resumed.err.contains("[request] answered: once"), "{resumed:?}");
    assert!(resumed.err.contains("[update] Cache warm"), "{resumed:?}");

    assert_eq!(lab.run(&["--stop", "t-01a2b3c4d5e6"], "").status, 0);

    let done = lab.run(&["List the files"], "");
    assert_eq!(done.status, 0, "{done:?}");
    assert_eq!(done.out, "Here are the files.\n");
    assert!(done.err.contains("[act] Run ls (done): Cargo.toml"), "{done:?}");
    assert!(done.err.contains("[request] Run: echo hi"), "{done:?}");
    assert!(
        done.err
            .contains("Answer with: crowbar-agent --resume t-01a2b3c4d5e6 --answer r-01a2b3c4d5e6 <once|always|no>"),
        "{done:?}"
    );
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

    let errored = lab.run(&["--resume", "missing"], "");
    assert_eq!(errored.status, 1, "{errored:?}");
    assert!(errored.err.contains("This request is not supported."), "{errored:?}");

    let more = lab.run(&["--resume", "t-01a2b3c4d5e6", "And then?"], "");
    assert_eq!(
        (more.status, more.out.as_str()),
        (0, "Here are the files.\n"),
        "{more:?}"
    );

    let models = lab.run(&["models"], "");
    assert_eq!(models.status, 0, "{models:?}");
    assert!(models.out.contains("stub/stub-1"), "{models:?}");
    assert!(models.out.contains("(default)"), "{models:?}");

    let free = lab.run(&["free-models", "on"], "");
    assert_eq!(free.status, 0, "{free:?}");
    assert!(free.err.contains("turned on"), "{free:?}");

    let set_m = lab.run(&["--resume", "t-01a2b3c4d5e6", "--model", "stub/stub-1"], "");
    assert_eq!(set_m.status, 0, "{set_m:?}");
    assert!(set_m.err.contains("stub/stub-1"), "{set_m:?}");

    let answered = lab.run(
        &["--resume", "t-01a2b3c4d5e6", "--answer", "r-01a2b3c4d5e6", "once"],
        "",
    );
    assert_eq!(
        (answered.status, answered.err.as_str()),
        (0, "Answered once.\n"),
        "{answered:?}"
    );
    let moded = lab.run(&["--resume", "t-01a2b3c4d5e6", "--mode", "full-access"], "");
    assert_eq!(
        (moded.status, moded.err.as_str()),
        (0, "t-01a2b3c4d5e6 is now in Full access.\n"),
        "{moded:?}"
    );
    let defaulted = lab.run(&["default-mode", "full-access"], "");
    assert_eq!(
        (defaulted.status, defaulted.err.as_str()),
        (0, "New threads start in Full access.\n"),
        "{defaulted:?}"
    );

    server.join().unwrap();
    assert_eq!(*sent_ops.lock().unwrap(), doc_ops);
}

#[test]
fn choice_of_free_models_model_selection_and_login_work_from_the_terminal() {
    let lab = Lab::with_script(json!({
        "no_models_when_free_disabled": true,
        "models": [
            { "id": "stub/stub-1", "name": "Stub One" },
            { "id": "stub/stub-2", "name": "Stub Two" }
        ],
        "default_model": "stub/stub-1",
        "turns": [
            { "steps": [{ "kind": "text", "text": "Reply from {model}" }] },
            { "steps": [{ "kind": "text", "text": "Next from {model}" }] }
        ]
    }));

    // Before choosing, free hosted Models are off, no Models are listed, and a message is refused.
    let before = lab.run(&["free-models"], "");
    assert_eq!((before.status, before.out.as_str()), (0, "Free hosted Models: off.\n"));

    let empty_models = lab.run(&["models"], "");
    assert_eq!(empty_models.status, 0, "{empty_models:?}");
    assert!(empty_models.err.contains("No Models are available"), "{empty_models:?}");

    let refused = lab.run(&["Hello"], "");
    assert_eq!(refused.status, 1, "{refused:?}");
    assert!(refused.err.contains("No Model is available"), "{refused:?}");

    // Piped stdin must never be interpreted as opting into hosted Models.
    let piped = lab.run(&[], "1\nHello\n");
    assert_eq!(piped.status, 1, "{piped:?}");
    assert!(piped.err.contains("No Model is available"), "{piped:?}");
    let still_off = lab.run(&["free-models"], "");
    assert_eq!(
        (still_off.status, still_off.out.as_str()),
        (0, "Free hosted Models: off.\n")
    );

    // Interactive `crowbar-agent` on a terminal offers the choice; picking 1 enables free hosted Models and answers.
    let (mut keys, screen) = terminal();
    let mut cmd = lab.agent(&[]);
    cmd.stdin(Stdio::from(screen));
    let mut running = Running::start(cmd);
    running.wait_for_err("Choice [1/2]: ");
    {
        let err = running.err.lock().unwrap();
        assert!(err.contains("1. Use OpenCode's free hosted Models"), "{err}");
        assert!(err.contains("2. Sign in to a provider"), "{err}");
    }
    keys.write_all(b"1\n").unwrap();
    keys.flush().unwrap();
    running.wait_for_err("Enabled OpenCode's free hosted Models.");
    keys.write_all(b"Hello\n").unwrap();
    keys.flush().unwrap();
    running.wait_for_out("Reply from stub/stub-1\n");
    keys.write_all(&[0x04]).unwrap();
    keys.flush().unwrap();
    assert_eq!(running.ended(), 0);
    assert_eq!(*running.out.lock().unwrap(), "Reply from stub/stub-1\n");

    // `models` now lists both Models and marks the default.
    let listed = lab.run(&["models"], "");
    assert_eq!(listed.status, 0, "{listed:?}");
    assert!(listed.out.contains("stub/stub-1  Stub One  (default)"), "{listed:?}");
    assert!(listed.out.contains("stub/stub-2  Stub Two"), "{listed:?}");

    // Switch the thread's Model and verify the next turn uses it.
    let thread = only_thread(&lab);
    let set = lab.run(&["--resume", &thread, "--model", "stub/stub-2"], "");
    assert_eq!(set.status, 0, "{set:?}");
    let next = lab.run(&["--resume", &thread, "Again"], "");
    assert_eq!(
        (next.status, next.out.as_str()),
        (0, "Next from stub/stub-2\n"),
        "{next:?}"
    );

    // `crowbar-agent login` runs `opencode auth login` directly, holding and relaying no credential.
    let fake_bin = lab.directory.join("bin");
    fs::create_dir_all(&fake_bin).unwrap();
    let login_log = lab.directory.join("login-args.txt");
    let fake_opencode = fake_bin.join("opencode");
    fs::write(
        &fake_opencode,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", login_log.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake_opencode, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let mut cmd = lab.agent(&["login"]);
    let path_env = format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap_or_default());
    cmd.env("PATH", path_env);
    let ran = Ran::of(run(cmd, ""));
    assert_eq!(ran.status, 0, "{ran:?}");
    assert_eq!(fs::read_to_string(&login_log).unwrap(), "auth\nlogin\n");
}

fn asking_script() -> Value {
    json!({
        "modes": true,
        "turns": [
            {
                "match": "please run",
                "steps": [
                    { "kind": "ask", "id": "c1", "title": "echo hi", "raw_input": { "command": "echo hi && id -un" }, "output": "hi" },
                    { "kind": "text", "text": "Ran." }
                ]
            }
        ]
    })
}

#[test]
fn a_request_shows_its_command_and_is_answered_from_another_terminal() {
    let lab = Lab::with_script(asking_script());
    let mut waiting = Running::start(lab.agent(&["please run it"]));
    waiting.wait_for_err("[request] Run: echo hi && id -un");
    waiting.wait_for_err("Answer with: crowbar-agent --resume ");
    let hint = waiting
        .err
        .lock()
        .unwrap()
        .lines()
        .find(|line| line.contains("Answer with:"))
        .unwrap()
        .to_string();
    let words: Vec<&str> = hint.split_whitespace().collect();
    let (thread, request) = (words[4].to_string(), words[6].to_string());
    assert!(thread.starts_with("t-") && request.starts_with("r-"), "{hint}");

    let listed = lab.run(&["--list"], "");
    assert!(listed.out.contains("(waiting for you)"), "{listed:?}");
    let status = lab.run(&["status"], "");
    assert!(
        status.out.contains(&format!("Waiting for you: {thread}.")),
        "{status:?}"
    );

    let wrong = lab.run(&["--resume", &thread, "--answer", &request, "maybe"], "");
    assert_eq!(wrong.status, 1, "{wrong:?}");
    assert!(wrong.err.contains("does not offer maybe"), "{wrong:?}");
    let answered = lab.run(&["--resume", &thread, "--answer", &request, "once"], "");
    assert_eq!(
        (answered.status, answered.err.as_str()),
        (0, "Answered once.\n"),
        "{answered:?}"
    );

    assert_eq!(waiting.ended(), 0);
    assert_eq!(waiting.out.lock().unwrap().as_str(), "Ran.\n");
    assert!(waiting.err.lock().unwrap().contains("[request] answered: once"));
    assert!(lab.rpc_messages().contains(r#""optionId":"once""#));
}

#[test]
fn a_request_is_answered_at_the_prompt_on_a_terminal() {
    let lab = Lab::with_script(asking_script());
    let (mut keys, screen) = terminal();
    let mut agent = lab.agent(&["please run it"]);
    agent
        .stdin(screen.try_clone().expect("the terminal again"))
        .stdout(Stdio::piped())
        .stderr(screen);
    let mut child = agent.spawn().expect("crowbar-agent runs");
    drop(agent);
    let mut shown = Vec::new();
    let mut bytes = [0u8; 4096];
    let read_until = |keys: &mut fs::File, shown: &mut Vec<u8>, bytes: &mut [u8], text: &str| {
        while !String::from_utf8_lossy(shown).contains(text) {
            let count = keys.read(bytes).expect("the terminal shows more");
            assert!(count > 0, "the terminal closed before {text:?}");
            shown.extend_from_slice(&bytes[..count]);
        }
    };
    read_until(&mut keys, &mut shown, &mut bytes, "Answer [once/always/no]: ");
    // Something it does not offer is asked again; then always.
    keys.write_all(b"maybe\n").unwrap();
    read_until(&mut keys, &mut shown, &mut bytes, "maybe\r\nAnswer [once/always/no]: ");
    keys.write_all(b"always\n").unwrap();
    read_until(&mut keys, &mut shown, &mut bytes, "[request] answered: always");
    let mut out = String::new();
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    assert_eq!(child.wait().expect("crowbar-agent ends").code(), Some(0));
    assert_eq!(out, "Ran.\n");
}

#[test]
fn a_thread_s_mode_and_the_starting_mode_are_set_from_the_terminal() {
    let lab = Lab::with_script(asking_script());
    let shown = lab.run(&["default-mode"], "");
    assert_eq!(
        (shown.status, shown.out.as_str()),
        (0, "New threads start in Ask first.\n")
    );

    // A thread started in Full access asks nothing.
    let full = lab.run(&["--mode", "full-access", "please run it"], "");
    assert_eq!((full.status, full.out.as_str()), (0, "Ran.\n"), "{full:?}");
    assert!(!full.err.contains("[request]"), "{full:?}");
    let thread = only_thread(&lab);

    let set = lab.run(&["--resume", &thread, "--mode", "plan"], "");
    assert_eq!(
        (set.status, set.err.clone()),
        (0, format!("{thread} is now in Plan.\n"))
    );
    let bad = lab.run(&["--resume", &thread, "--mode", "yolo"], "");
    assert_eq!(bad.status, 1, "{bad:?}");
    assert!(bad.err.contains("There is no mode yolo."), "{bad:?}");

    let set = lab.run(&["default-mode", "full-access"], "");
    assert_eq!(
        (set.status, set.err.as_str()),
        (0, "New threads start in Full access.\n")
    );
    let next = lab.run(&["please run it again"], "");
    assert_eq!((next.status, next.out.as_str()), (0, "Ran.\n"), "{next:?}");
    let bad = lab.run(&["default-mode", "plan"], "");
    assert_eq!(bad.status, 1, "{bad:?}");
}

/// Reads what the terminal shows until it shows `text`, for ten seconds at most.
fn screen_until(keys: &mut fs::File, shown: &mut Vec<u8>, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut bytes = [0u8; 4096];
    while !String::from_utf8_lossy(shown).contains(text) {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(
            !left.is_zero(),
            "the terminal never showed {text:?}: {}",
            String::from_utf8_lossy(shown)
        );
        let mut poll = libc::pollfd {
            fd: keys.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        if unsafe { libc::poll(&mut poll, 1, left.as_millis() as libc::c_int) } <= 0 {
            continue;
        }
        let count = keys.read(&mut bytes).expect("the terminal shows more");
        assert!(count > 0, "the terminal closed before {text:?}");
        shown.extend_from_slice(&bytes[..count]);
    }
}

#[test]
fn requests_that_wait_after_their_turn_are_each_asked_for_and_then_the_thread_takes_a_message() {
    use std::os::unix::net::UnixListener;

    let lab = Lab::empty();
    let sock_dir = lab.directory.join("runtime/crowbar-lever");
    fs::create_dir_all(&sock_dir).unwrap();
    let listener = UnixListener::bind(sock_dir.join("person")).expect("bind a Lever of the test's");
    let request = |id: &str, command: &str, state: &str| {
        json!({
            "request": id, "kind": "command", "title": command, "detail": command, "state": state,
            "answers": [{ "answer": "once", "name": "Allow once" }, { "answer": "no", "name": "Reject" }]
        })
    };
    let waiting = [
        request("r-1", "make", "waiting"),
        request("r-2", "make test", "waiting"),
    ];
    // A thread whose turn is done, and two requests that came after it.
    let lever = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("crowbar-agent connects");
        assert_eq!(read_framed(&mut stream), json!({ "version": 1 }));
        write_framed(&mut stream, &json!({ "version": 1 }));
        assert_eq!(read_framed(&mut stream), json!({ "op": "resume", "thread": "t-1" }));
        write_framed(
            &mut stream,
            &json!({
                "reply": "resumed", "thread": "t-1", "title": "Build", "cwd": "/tmp",
                "started": 1, "updated": 2, "working": false, "mode": "ask-first",
                "turns": [{
                    "turn": 1, "message": "build it", "answer": "Built.", "state": "done",
                    "acts": [], "requests": waiting, "notes": []
                }]
            }),
        );
        let mut answers = Vec::new();
        for _ in 0..2 {
            let answer = read_framed(&mut stream);
            write_framed(&mut stream, &json!({ "reply": "ok" }));
            let id = answer["request"].as_str().unwrap().to_string();
            let mut ended = request(&id, "", "answered");
            ended["answer"] = answer["answer"].clone();
            ended["event"] = json!("request");
            ended["thread"] = json!("t-1");
            ended["turn"] = json!(1);
            write_framed(&mut stream, &ended);
            answers.push((id, answer["answer"].as_str().unwrap().to_string()));
        }
        // Lever stays until crowbar-agent leaves.
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
        answers
    });

    let (mut keys, screen) = terminal();
    let mut agent = lab.agent(&["--resume", "t-1"]);
    agent
        .stdin(screen.try_clone().expect("the terminal again"))
        .stdout(Stdio::null())
        .stderr(screen);
    let mut child = agent.spawn().expect("crowbar-agent runs");
    drop(agent);
    let mut shown = Vec::new();
    screen_until(&mut keys, &mut shown, "[request] Run: make\r\nAnswer [once/no]: ");
    keys.write_all(b"once\n").unwrap();
    // The next is asked for when Lever has the answer to the one before.
    screen_until(
        &mut keys,
        &mut shown,
        "[request] answered: once\r\n[request] Run: make test\r\nAnswer [once/no]: ",
    );
    keys.write_all(b"no\n").unwrap();
    screen_until(&mut keys, &mut shown, "[request] answered: no\r\n> ");
    keys.write_all(&[4]).unwrap();
    assert_eq!(child.wait().expect("crowbar-agent ends").code(), Some(0));
    assert_eq!(
        lever.join().unwrap(),
        [
            ("r-1".to_string(), "once".to_string()),
            ("r-2".to_string(), "no".to_string())
        ]
    );
}

#[test]
fn a_request_that_comes_while_the_prompt_waits_is_asked_for_there() {
    let lab = Lab::with_script(json!({
        "modes": true,
        "turns": [{
            "steps": [{ "kind": "text", "text": "Done here." }],
            "after": [
                { "kind": "pause_ms", "ms": 400 },
                { "kind": "ask", "id": "c7", "title": "make", "raw_input": { "command": "make" } }
            ]
        }]
    }));
    let (mut keys, screen) = terminal();
    let mut agent = lab.agent(&[]);
    agent
        .stdin(screen.try_clone().expect("the terminal again"))
        .stdout(Stdio::null())
        .stderr(screen);
    let mut child = agent.spawn().expect("crowbar-agent runs");
    drop(agent);
    let mut shown = Vec::new();
    screen_until(&mut keys, &mut shown, "> ");
    keys.write_all(b"hello\n").unwrap();
    // The turn ends, the prompt comes back, and then the harness asks.
    screen_until(
        &mut keys,
        &mut shown,
        "[request] Run: make\r\nAnswer [once/always/no]: ",
    );
    keys.write_all(b"once\n").unwrap();
    screen_until(&mut keys, &mut shown, "[request] answered: once\r\n> ");
    keys.write_all(&[4]).unwrap();
    assert_eq!(child.wait().expect("crowbar-agent ends").code(), Some(0));
    assert!(lab.rpc_messages().contains(r#""optionId":"once""#));
}

#[test]
fn a_request_that_waits_when_its_turn_ends_is_still_asked_for_before_the_prompt() {
    use std::os::unix::net::UnixListener;

    let lab = Lab::empty();
    let sock_dir = lab.directory.join("runtime/crowbar-lever");
    fs::create_dir_all(&sock_dir).unwrap();
    let listener = UnixListener::bind(sock_dir.join("person")).expect("bind a Lever of the test's");
    let request = |state: &str| {
        json!({
            "event": "request", "thread": "t-1", "turn": 1, "request": "r-1", "kind": "command",
            "title": "make", "detail": "make", "state": state,
            "answers": [{ "answer": "once", "name": "Allow once" }, { "answer": "no", "name": "Reject" }]
        })
    };
    let lever = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("crowbar-agent connects");
        assert_eq!(read_framed(&mut stream), json!({ "version": 1 }));
        write_framed(&mut stream, &json!({ "version": 1 }));
        assert_eq!(read_framed(&mut stream)["op"], "models");
        write_framed(
            &mut stream,
            &json!({ "reply": "models", "supported": false, "models": [], "free_hosted_models": false, "modes": [] }),
        );
        assert_eq!(read_framed(&mut stream)["op"], "send");
        write_framed(&mut stream, &json!({ "reply": "sent", "thread": "t-1", "turn": 1 }));
        // The harness asks, and its turn ends while the request waits.
        write_framed(&mut stream, &request("waiting"));
        write_framed(&mut stream, &json!({ "event": "done", "thread": "t-1", "turn": 1 }));
        let answer = read_framed(&mut stream);
        write_framed(&mut stream, &json!({ "reply": "ok" }));
        let mut answered = request("answered");
        answered["answer"] = json!("once");
        write_framed(&mut stream, &answered);
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
        answer
    });

    let (mut keys, screen) = terminal();
    let mut agent = lab.agent(&[]);
    agent
        .stdin(screen.try_clone().expect("the terminal again"))
        .stdout(Stdio::null())
        .stderr(screen);
    let mut child = agent.spawn().expect("crowbar-agent runs");
    drop(agent);
    let mut shown = Vec::new();
    screen_until(&mut keys, &mut shown, "> ");
    keys.write_all(b"go\n").unwrap();
    screen_until(&mut keys, &mut shown, "Answer [once/no]: ");
    // Give the turn's end time to come before the answer is typed.
    std::thread::sleep(Duration::from_millis(300));
    keys.write_all(b"once\n").unwrap();
    screen_until(&mut keys, &mut shown, "[request] answered: once\r\n> ");
    keys.write_all(&[4]).unwrap();
    assert_eq!(child.wait().expect("crowbar-agent ends").code(), Some(0));
    assert_eq!(
        lever.join().unwrap(),
        json!({ "op": "answer", "thread": "t-1", "request": "r-1", "answer": "once" })
    );
}

#[test]
fn a_turn_another_client_begins_while_a_request_is_asked_shows_and_ctrl_c_stops_only_the_requests_turn() {
    use std::os::unix::net::UnixListener;

    let lab = Lab::empty();
    let sock_dir = lab.directory.join("runtime/crowbar-lever");
    fs::create_dir_all(&sock_dir).unwrap();
    let listener = UnixListener::bind(sock_dir.join("person")).expect("bind a Lever of the test's");
    let request = |id: &str, turn: u64, state: &str| {
        json!({
            "event": "request", "thread": "t-1", "turn": turn, "request": id, "kind": "command",
            "title": "make", "detail": "make", "state": state,
            "answers": [{ "answer": "once", "name": "Allow once" }, { "answer": "no", "name": "Reject" }]
        })
    };
    let lever = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("crowbar-agent connects");
        assert_eq!(read_framed(&mut stream), json!({ "version": 1 }));
        write_framed(&mut stream, &json!({ "version": 1 }));
        assert_eq!(read_framed(&mut stream)["op"], "resume");
        let mut waiting = request("r-1", 1, "waiting");
        for key in ["event", "thread", "turn"] {
            waiting.as_object_mut().unwrap().remove(key);
        }
        write_framed(
            &mut stream,
            &json!({
                "reply": "resumed", "thread": "t-1", "title": "Build", "cwd": "/tmp",
                "started": 1, "updated": 2, "working": false, "mode": "ask-first",
                "turns": [{ "turn": 1, "message": "build it", "answer": "Built.", "state": "done",
                            "acts": [], "requests": [waiting], "notes": [] }]
            }),
        );
        // Another client begins turn 2 while the request is asked for.
        std::thread::sleep(Duration::from_millis(300));
        for event in [
            json!({ "event": "message", "thread": "t-1", "turn": 2, "text": "from elsewhere" }),
            json!({ "event": "text", "thread": "t-1", "turn": 2, "text": "Two." }),
            json!({ "event": "done", "thread": "t-1", "turn": 2 }),
        ] {
            write_framed(&mut stream, &event);
        }
        let answer = read_framed(&mut stream);
        write_framed(&mut stream, &json!({ "reply": "ok" }));
        let mut answered = request("r-1", 1, "answered");
        answered["answer"] = json!("once");
        write_framed(&mut stream, &answered);
        // At the prompt, a request of turn 2 comes, and Ctrl-C stops only that turn.
        std::thread::sleep(Duration::from_millis(300));
        write_framed(&mut stream, &request("r-2", 2, "waiting"));
        // And turn 3 begins elsewhere and fails while it is asked for.
        std::thread::sleep(Duration::from_millis(200));
        write_framed(
            &mut stream,
            &json!({ "event": "message", "thread": "t-1", "turn": 3, "text": "third" }),
        );
        write_framed(
            &mut stream,
            &json!({ "event": "failed", "thread": "t-1", "turn": 3, "message": "No more." }),
        );
        let stop = read_framed(&mut stream);
        write_framed(&mut stream, &json!({ "reply": "ok" }));
        write_framed(&mut stream, &request("r-2", 2, "withdrawn"));
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
        (answer, stop)
    });

    let (mut keys, screen) = terminal();
    let mut agent = lab.agent(&["--resume", "t-1"]);
    agent
        .stdin(screen.try_clone().expect("the terminal again"))
        .stdout(Stdio::piped())
        .stderr(screen);
    let mut child = agent.spawn().expect("crowbar-agent runs");
    drop(agent);
    let mut shown = Vec::new();
    screen_until(&mut keys, &mut shown, "Answer [once/no]: ");
    std::thread::sleep(Duration::from_millis(600));
    keys.write_all(b"once\n").unwrap();
    screen_until(&mut keys, &mut shown, "[request] answered: once\r\n> ");
    shown.clear();
    screen_until(&mut keys, &mut shown, "Answer [once/no]: ");
    screen_until(&mut keys, &mut shown, "The Agent gave no answer: No more.");
    // SAFETY: sending Ctrl-C's signal to our own child process.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    screen_until(&mut keys, &mut shown, "[request] withdrawn");
    keys.write_all(&[4]).unwrap();
    let mut out = String::new();
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    // The newest turn, followed while the request was asked for, failed.
    assert_eq!(child.wait().expect("crowbar-agent ends").code(), Some(1));
    let (answer, stop) = lever.join().unwrap();
    assert_eq!(answer["answer"], "once");
    assert_eq!(stop, json!({ "op": "stop", "thread": "t-1", "turn": 2 }));
    assert!(out.contains("> from elsewhere") && out.contains("Two."), "{out:?}");
}

#[test]
fn ctrl_c_turns_down_the_request_asked_by_stopping_its_turn_and_says_how_to_answer_the_next() {
    use std::os::unix::net::UnixListener;

    let lab = Lab::empty();
    let sock_dir = lab.directory.join("runtime/crowbar-lever");
    fs::create_dir_all(&sock_dir).unwrap();
    let listener = UnixListener::bind(sock_dir.join("person")).expect("bind a Lever of the test's");
    let request = |id: &str, turn: u64, state: &str| {
        json!({
            "event": "request", "thread": "t-1", "turn": turn, "request": id, "kind": "command",
            "title": id, "detail": format!("make {id}"), "state": state,
            "answers": [{ "answer": "once", "name": "Allow once" }, { "answer": "no", "name": "Reject" }]
        })
    };
    let lever = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("crowbar-agent connects");
        assert_eq!(read_framed(&mut stream), json!({ "version": 1 }));
        write_framed(&mut stream, &json!({ "version": 1 }));
        assert_eq!(read_framed(&mut stream)["op"], "resume");
        let recorded = |id: &str| {
            let mut r = request(id, 0, "waiting");
            for key in ["event", "thread", "turn"] {
                r.as_object_mut().unwrap().remove(key);
            }
            r
        };
        let turn = |n: u64, id: &str| {
            json!({ "turn": n, "message": "m", "answer": "", "state": "done", "acts": [],
                    "requests": [recorded(id)], "notes": [] })
        };
        write_framed(
            &mut stream,
            &json!({
                "reply": "resumed", "thread": "t-1", "title": "Build", "cwd": "/tmp",
                "started": 1, "updated": 2, "working": false, "mode": "ask-first",
                "turns": [turn(1, "r-1"), turn(2, "r-2")]
            }),
        );
        // Another client begins turn 3 while r-1 is asked for.
        std::thread::sleep(Duration::from_millis(300));
        write_framed(
            &mut stream,
            &json!({ "event": "message", "thread": "t-1", "turn": 3, "text": "from elsewhere" }),
        );
        let stop = read_framed(&mut stream);
        write_framed(&mut stream, &json!({ "reply": "ok" }));
        write_framed(&mut stream, &request("r-1", 1, "withdrawn"));
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
        stop
    });

    let (mut keys, screen) = terminal();
    let mut agent = lab.agent(&["--resume", "t-1"]);
    agent
        .stdin(screen.try_clone().expect("the terminal again"))
        .stdout(Stdio::null())
        .stderr(screen);
    let mut child = agent.spawn().expect("crowbar-agent runs");
    drop(agent);
    let mut shown = Vec::new();
    screen_until(&mut keys, &mut shown, "[request] Run: make r-1\r\nAnswer [once/no]: ");
    std::thread::sleep(Duration::from_millis(600));
    // SAFETY: sending Ctrl-C's signal to our own child process.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
    // The other request says how to answer it, and the prompt comes back.
    screen_until(
        &mut keys,
        &mut shown,
        "Answer with: crowbar-agent --resume t-1 --answer r-2 <once|no>\r\n> ",
    );
    screen_until(&mut keys, &mut shown, "[request] withdrawn");
    keys.write_all(&[4]).unwrap();
    assert_eq!(child.wait().expect("crowbar-agent ends").code(), Some(0));
    assert_eq!(
        lever.join().unwrap(),
        json!({ "op": "stop", "thread": "t-1", "turn": 1 })
    );
}

#[test]
fn ctrl_d_while_a_request_waits_leaves_with_how_a_followed_turn_ended() {
    use std::os::unix::net::UnixListener;

    let lab = Lab::empty();
    let sock_dir = lab.directory.join("runtime/crowbar-lever");
    fs::create_dir_all(&sock_dir).unwrap();
    let listener = UnixListener::bind(sock_dir.join("person")).expect("bind a Lever of the test's");
    let lever = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("crowbar-agent connects");
        assert_eq!(read_framed(&mut stream), json!({ "version": 1 }));
        write_framed(&mut stream, &json!({ "version": 1 }));
        assert_eq!(read_framed(&mut stream)["op"], "resume");
        let waiting = json!({ "request": "r-1", "kind": "command", "title": "make", "detail": "make", "state": "waiting",
                              "answers": [{ "answer": "once", "name": "Allow once" }] });
        write_framed(
            &mut stream,
            &json!({
                "reply": "resumed", "thread": "t-1", "title": "Build", "cwd": "/tmp",
                "started": 1, "updated": 2, "working": false, "mode": "ask-first",
                "turns": [{ "turn": 1, "message": "m", "answer": "", "state": "done",
                            "acts": [], "requests": [waiting], "notes": [] }]
            }),
        );
        std::thread::sleep(Duration::from_millis(300));
        write_framed(
            &mut stream,
            &json!({ "event": "message", "thread": "t-1", "turn": 2, "text": "elsewhere" }),
        );
        write_framed(
            &mut stream,
            &json!({ "event": "failed", "thread": "t-1", "turn": 2, "message": "No more." }),
        );
        let mut rest = Vec::new();
        let _ = stream.read_to_end(&mut rest);
    });

    let (mut keys, screen) = terminal();
    let mut agent = lab.agent(&["--resume", "t-1"]);
    agent
        .stdin(screen.try_clone().expect("the terminal again"))
        .stdout(Stdio::null())
        .stderr(screen);
    let mut child = agent.spawn().expect("crowbar-agent runs");
    drop(agent);
    let mut shown = Vec::new();
    screen_until(&mut keys, &mut shown, "The Agent gave no answer: No more.");
    keys.write_all(&[4]).unwrap();
    assert_eq!(child.wait().expect("crowbar-agent ends").code(), Some(1));
    lever.join().unwrap();
}
