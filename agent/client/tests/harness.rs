//! `crowbar-agent` against the Harness itself: the bundle Base ships
//! (`agent/dist/harness.mjs`), started as one process as Base's service
//! starts it, with the stub Model (`agent/test/stub-model.ts`) for a Model,
//! over the real person socket.
//!
//! They need the bundle and Node.js, which Base's build root does not have,
//! so plain `cargo test` leaves them out. `base/void_base.py agent` bundles
//! the Harness and runs them, with the Node.js Base pins:
//!
//! ```sh
//! cargo test -- --include-ignored
//! ```
//!
//! `CROWBAR_AGENT_NODE` names the Node.js to run; without it, `node` is.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn uid() -> u32 {
    // SAFETY: `getuid` takes nothing and cannot fail.
    unsafe { libc::getuid() }
}

fn node() -> Command {
    let mut node = Command::new(std::env::var_os("CROWBAR_AGENT_NODE").unwrap_or_else(|| "node".into()));
    node.arg("--disable-warning=ExperimentalWarning");
    node
}

fn agent_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the client is in agent/")
        .to_path_buf()
}

/// Waits for `met`, for as long as ten seconds.
fn wait_for(what: &str, mut met: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !met() {
        assert!(Instant::now() < deadline, "waited ten seconds for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A Harness and a stub Model of a test's own, in a directory of its own.
/// Both end, and the directory goes, when the test does.
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
    /// A directory, and nothing running in it yet.
    fn empty() -> Lab {
        static COUNT: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "crowbar-agent-client-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        );
        let directory = std::env::temp_dir().join(name);
        fs::create_dir(&directory).expect("a directory for the test");
        fs::create_dir(directory.join("runtime")).expect("a runtime directory for the test");
        Lab {
            directory,
            children: Vec::new(),
        }
    }

    fn state(&self) -> PathBuf {
        self.directory.join("state")
    }

    /// Starts the bundled Harness, serving `person_uid` on the person
    /// socket, and waits until it listens on both its sockets.
    fn harness(&mut self, person_uid: u32) {
        let bundle = agent_directory().join("dist/harness.mjs");
        assert!(
            bundle.exists(),
            "{} is not there: bundle the Harness first",
            bundle.display()
        );
        let mut harness = node();
        harness
            .arg(bundle)
            .arg("--state")
            .arg(self.state())
            .arg("--surface-socket")
            .arg(self.directory.join("surface"))
            .args(["--shell-uid", &uid().to_string()])
            .arg("--person-socket")
            .arg(self.directory.join("runtime/crowbar-agent/person"))
            .args(["--person-uid", &person_uid.to_string()]);
        self.children
            .push(harness.stdin(Stdio::null()).spawn().expect("Node.js runs the Harness"));
        let log = self.state().join("harness.log");
        wait_for("the Harness to listen", || {
            fs::read_to_string(&log).is_ok_and(|log| log.matches(" listening: ").count() == 2)
        });
    }

    /// Starts a Harness that serves the tests' own user ID, and a stub
    /// Model that answers from `script`, and points the one at the other
    /// as a person would by hand.
    fn with_model(script: &str) -> Lab {
        let mut lab = Lab::empty();
        lab.harness(uid());
        fs::write(lab.directory.join("script.json"), script).expect("the stub's script");
        let mut stub = node();
        stub.arg(agent_directory().join("test/stub-model.ts"))
            .arg("--script")
            .arg(lab.directory.join("script.json"))
            .arg("--requests")
            .arg(lab.directory.join("requests.jsonl"));
        let mut stub = stub
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("Node.js runs the stub Model");
        let mut said = String::new();
        BufReader::new(stub.stdout.take().expect("the stub's output"))
            .read_line(&mut said)
            .expect("the stub says where it listens");
        lab.children.push(stub);
        let url = said
            .trim()
            .strip_prefix("listening ")
            .unwrap_or_else(|| panic!("the stub Model said {said:?}"));
        fs::write(
            lab.state().join("endpoint.json"),
            format!(r#"{{"url":"{url}","model":"stub-1"}}"#),
        )
        .expect("the endpoint file");
        lab
    }

    /// `crowbar-agent`, as a person would run it on a console: it finds the
    /// Harness through the runtime directory, and has no desktop.
    fn agent(&self, args: &[&str]) -> Command {
        let mut agent = Command::new(env!("CARGO_BIN_EXE_crowbar-agent"));
        agent
            .args(args)
            .env("XDG_RUNTIME_DIR", self.directory.join("runtime"))
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        agent
    }

    /// Runs `crowbar-agent` to its end, with `typed` for what the person types.
    fn run(&self, args: &[&str], typed: &str) -> Ran {
        Ran::of(run(self.agent(args), typed))
    }

    /// What the stub Model was sent, a request to a line.
    fn requests(&self) -> String {
        fs::read_to_string(self.directory.join("requests.jsonl")).unwrap_or_default()
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

/// How a run of `crowbar-agent` ended.
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

/// A `crowbar-agent` that is still running, with what it has written so far.
struct Running {
    child: Child,
    out: Arc<Mutex<String>>,
    err: Arc<Mutex<String>>,
}

fn collect(stream: impl Read + Send + 'static) -> Arc<Mutex<String>> {
    let collected = Arc::new(Mutex::new(String::new()));
    let into = collected.clone();
    std::thread::spawn(move || {
        let mut stream = stream;
        let mut bytes = [0u8; 4096];
        while let Ok(count @ 1..) = stream.read(&mut bytes) {
            into.lock().unwrap().push_str(&String::from_utf8_lossy(&bytes[..count]));
        }
    });
    collected
}

impl Running {
    fn start(mut command: Command) -> Running {
        let mut child = command.spawn().expect("crowbar-agent runs");
        let out = collect(child.stdout.take().expect("its output"));
        let err = collect(child.stderr.take().expect("its errors"));
        Running { child, out, err }
    }

    fn signal(&self, signal: libc::c_int) {
        // SAFETY: the process is this test's own child, which it has not waited for.
        unsafe { libc::kill(self.child.id() as libc::pid_t, signal) };
    }

    fn wait_for_out(&self, text: &str) {
        wait_for(&format!("{text:?} on standard output"), || {
            self.out.lock().unwrap().contains(text)
        });
    }

    fn wait_for_err(&self, text: &str) {
        wait_for(&format!("{text:?} on standard error"), || {
            self.err.lock().unwrap().contains(text)
        });
    }

    /// Waits for it to end, and gives its exit status, or -1 for a signal.
    fn ended(&mut self) -> i32 {
        self.child.wait().expect("crowbar-agent ends").code().unwrap_or(-1)
    }
}

/// A terminal of the test's own. What is written to the first is typed at
/// it, and the second is the terminal a client is given for its input.
fn terminal() -> (fs::File, OwnedFd) {
    let (mut keys, mut screen) = (0, 0);
    // SAFETY: `openpty` writes two descriptors, and takes nothing else here.
    let opened = unsafe {
        libc::openpty(
            &mut keys,
            &mut screen,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(opened, 0, "a terminal for the test");
    // SAFETY: both descriptors are open, and nothing else owns them.
    unsafe { (fs::File::from_raw_fd(keys), OwnedFd::from_raw_fd(screen)) }
}

/// The ID of the one conversation `--list` shows.
fn only_conversation(lab: &Lab) -> String {
    let listed = lab.run(&["--list"], "");
    let lines: Vec<&str> = listed.out.lines().collect();
    assert_eq!(lines.len(), 1, "{listed:?}");
    lines[0]
        .split_whitespace()
        .next()
        .expect("a conversation's ID")
        .to_string()
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn one_request_is_answered_on_standard_output_and_the_client_exits() {
    let lab = Lab::with_model(r#"[{"text": "Hello from the stub Model.", "pause_ms": 20}]"#);
    let ran = lab.run(&["Hello,", "Agent."], "");
    assert_eq!(
        (ran.status, ran.out.as_str(), ran.err.as_str()),
        (0, "Hello from the stub Model.\n", "")
    );
    assert!(
        lab.requests().contains(r#"{"role":"user","content":"Hello, Agent."}"#),
        "{}",
        lab.requests()
    );
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn a_conversation_goes_a_line_at_a_time_and_is_listed_and_resumed() {
    let lab =
        Lab::with_model(r#"[{"text": "The first answer."}, {"text": "The second answer."}, {"text": "The third."}]"#);
    // With no conversation yet, the list says so, and a client that leaves at once starts none.
    assert_eq!(lab.run(&[], "").status, 0);
    let none = lab.run(&["--list"], "");
    assert_eq!(
        (none.status, none.out.as_str(), none.err.as_str()),
        (0, "", "No conversations yet.\n")
    );

    let talked = lab.run(&[], "What is first?\n\nAnd second?\n");
    assert_eq!(
        (talked.status, talked.out.as_str()),
        (0, "The first answer.\nThe second answer.\n"),
        "{talked:?}"
    );

    let listed = lab.run(&["--list"], "");
    let conversation = only_conversation(&lab);
    assert_eq!(
        listed.out,
        format!("{conversation}  just now        2 turns  What is first?\n")
    );

    let resumed = lab.run(&["--resume", &conversation], "");
    assert_eq!(
        resumed.out,
        "> What is first?\nThe first answer.\n> And second?\nThe second answer.\n"
    );
    let more = lab.run(&["--resume", &conversation, "And third?"], "");
    assert_eq!((more.status, more.out.as_str()), (0, "The third.\n"), "{more:?}");
    assert!(lab.run(&["--list"], "").out.contains("3 turns"));

    let missing = lab.run(&["--resume", "nosuchid", "Hello?"], "");
    assert_eq!(
        (missing.status, missing.err.as_str()),
        (1, "crowbar-agent: There is no conversation nosuchid.\n")
    );
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn ctrl_c_stops_the_turn_and_a_client_that_goes_away_does_not() {
    let lab =
        Lab::with_model(r#"[{"text": "One two three", "stall": true}, {"text": "Four five six", "stall": true}]"#);
    // The client is ended as a closed terminal or a dropped SSH connection ends it. The turn goes on.
    let mut gone = Running::start(lab.agent(&["Count."]));
    gone.wait_for_out("One");
    gone.signal(libc::SIGTERM);
    assert_eq!(gone.ended(), -1);
    std::thread::sleep(Duration::from_millis(300));
    assert!(lab.run(&["--list"], "").out.ends_with("(working)\n"));

    // A client that resumes the conversation watches the turn, and Ctrl-C stops it.
    let conversation = only_conversation(&lab);
    let mut resumed = Running::start(lab.agent(&["--resume", &conversation]));
    resumed.wait_for_out("> Count.\nOne");
    resumed.signal(libc::SIGINT);
    resumed.wait_for_err("Stopped.\n");
    assert!(!lab.run(&["--list"], "").out.contains("(working)"));
    // Ctrl-D leaves.
    drop(resumed.child.stdin.take());
    assert_eq!(resumed.ended(), 0);
    assert_eq!(resumed.out.lock().unwrap().as_str(), "> Count.\nOne\n");

    // Ctrl-C in a turn of the client's own ends it as Ctrl-C ends any program.
    let mut own = Running::start(lab.agent(&["--resume", &conversation, "Count on."]));
    own.wait_for_out("Four");
    own.signal(libc::SIGINT);
    assert_eq!(own.ended(), 130);
    assert_eq!(
        (own.out.lock().unwrap().as_str(), own.err.lock().unwrap().as_str()),
        ("Four\n", "Stopped.\n")
    );
    let stood = lab.run(&["--resume", &conversation], "");
    assert_eq!(stood.out, "> Count.\nOne\n> Count on.\nFour\n");
    assert_eq!(stood.err, "(stopped)\n(stopped)\n");
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn what_a_model_says_reaches_the_terminal_with_nothing_that_drives_it() {
    // An answer that would clear the screen and set the clipboard.
    let lab = Lab::with_model(r#"[{"text": "Clear \u001b[2Jthis, \u001b]52;c;aGk=\u0007and that."}]"#);
    let shown = "Clear ␛[2Jthis, ␛]52;c;aGk=␇and that.\n";
    let ran = lab.run(&["Hello?"], "");
    assert_eq!((ran.status, ran.out.as_str()), (0, shown), "{ran:?}");
    // And as it is shown again, from what the Harness kept.
    let stood = lab.run(&["--resume", &only_conversation(&lab)], "");
    assert_eq!(stood.out, format!("> Hello?\n{shown}"), "{stood:?}");
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn ctrl_c_leaves_a_client_that_waits_for_a_harness_that_does_not_answer() {
    let mut lab = Lab::empty();
    lab.harness(uid());
    // The Harness is there and answers nothing, as one that is wedged.
    // SAFETY: the process is this test's own child, which it has not waited for.
    unsafe { libc::kill(lab.children[0].id() as libc::pid_t, libc::SIGSTOP) };
    let mut waiting = Running::start(lab.agent(&["--list"]));
    std::thread::sleep(Duration::from_millis(300));
    waiting.signal(libc::SIGINT);
    wait_for("the client to leave", || {
        waiting.child.try_wait().expect("crowbar-agent is waited for").is_some()
    });
    assert_eq!(waiting.ended(), 130);
    assert_eq!(waiting.err.lock().unwrap().as_str(), "");
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn ctrl_c_after_a_message_is_sent_and_before_the_harness_answers_says_it_may_still_be_answered() {
    let lab = Lab::with_model(r#"[{"text": "Here."}]"#);
    let mut talking = Running::start(lab.agent(&[]));
    // Connected, and then the Harness answers nothing more.
    std::thread::sleep(Duration::from_millis(300));
    let harness = lab.children[0].id() as libc::pid_t;
    // SAFETY: the process is this test's own child, which it has not waited for.
    unsafe { libc::kill(harness, libc::SIGSTOP) };
    let mut typing = talking.child.stdin.take().expect("its input");
    typing.write_all(b"Hello?\n").expect("typing");
    std::thread::sleep(Duration::from_millis(300));
    talking.signal(libc::SIGINT);
    wait_for("the client to leave", || {
        talking.child.try_wait().expect("crowbar-agent is waited for").is_some()
    });
    assert_eq!(talking.ended(), 130);
    assert!(
        talking
            .err
            .lock()
            .unwrap()
            .contains("The message was sent, and the Agent may still answer it"),
        "{:?}",
        talking.err
    );
    // And it is, once the Harness goes on.
    // SAFETY: as above.
    unsafe { libc::kill(harness, libc::SIGCONT) };
    wait_for("the turn to be answered", || {
        lab.run(&["--list"], "").out.contains("1 turn ")
    });
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn ctrl_c_stops_a_turn_another_client_began_while_this_one_shows_it() {
    let lab = Lab::with_model(r#"[{"text": "Here."}, {"text": "One two three", "stall": true}]"#);
    assert_eq!(lab.run(&["Hello?"], "").status, 0);
    let conversation = only_conversation(&lab);
    // One terminal has the conversation open, and waits for a line.
    let mut shown = Running::start(lab.agent(&["--resume", &conversation]));
    shown.wait_for_out("> Hello?\nHere.\n");
    // Another begins a turn in it, which the first shows as it comes.
    let mut other = Running::start(lab.agent(&["--resume", &conversation, "Count."]));
    shown.wait_for_out("\n> Count.\nOne");
    shown.signal(libc::SIGINT);
    shown.wait_for_err("Stopped.\n");
    assert!(!lab.run(&["--list"], "").out.contains("(working)"));
    assert_eq!(other.ended(), 130);
    // The first is still there, at its prompt, and Ctrl-D leaves it.
    drop(shown.child.stdin.take());
    assert_eq!(shown.ended(), 0);
    assert_eq!(shown.out.lock().unwrap().as_str(), "> Hello?\nHere.\n\n> Count.\nOne\n");
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn ctrl_d_leaves_a_client_that_shows_another_clients_turn_and_the_turn_goes_on() {
    let lab = Lab::with_model(r#"[{"text": "Here."}, {"text": "One two three", "stall": true}]"#);
    assert_eq!(lab.run(&["Hello?"], "").status, 0);
    let conversation = only_conversation(&lab);
    let mut shown = Running::start(lab.agent(&["--resume", &conversation]));
    shown.wait_for_out("> Hello?\nHere.\n");
    let mut other = Running::start(lab.agent(&["--resume", &conversation, "Count."]));
    shown.wait_for_out("\n> Count.\nOne");
    // Ctrl-D, while the other client's turn is on the screen: this client leaves, and stops nothing.
    drop(shown.child.stdin.take());
    wait_for("the client to leave", || {
        shown.child.try_wait().expect("crowbar-agent is waited for").is_some()
    });
    assert_eq!(shown.ended(), 0);
    assert_eq!(shown.err.lock().unwrap().as_str(), "");
    assert!(lab.run(&["--list"], "").out.ends_with("(working)\n"));
    other.signal(libc::SIGINT);
    assert_eq!(other.ended(), 130);
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn ctrl_d_at_a_terminal_leaves_on_the_line_the_answer_ended() {
    let lab = Lab::with_model(r#"[{"text": "Here."}, {"text": "One two three", "stall": true}]"#);
    assert_eq!(lab.run(&["Hello?"], "").status, 0);
    let conversation = only_conversation(&lab);
    // At a terminal the client prompts, and ends the prompt's line when it leaves from it.
    let (mut keys, screen) = terminal();
    let mut shown = lab.agent(&["--resume", &conversation]);
    shown.stdin(screen);
    let mut shown = Running::start(shown);
    shown.wait_for_out("> Hello?\nHere.\n");
    let mut other = Running::start(lab.agent(&["--resume", &conversation, "Count."]));
    shown.wait_for_out("\n> Count.\nOne");
    // Left while a turn is shown, there is no prompt's line to end: only the one prompt was written.
    keys.write_all(&[0x04]).expect("Ctrl-D");
    wait_for("the client to leave", || {
        shown.child.try_wait().expect("crowbar-agent is waited for").is_some()
    });
    assert_eq!(shown.ended(), 0);
    assert_eq!(shown.out.lock().unwrap().as_str(), "> Hello?\nHere.\n\n> Count.\nOne\n");
    assert_eq!(shown.err.lock().unwrap().as_str(), "> ");
    other.signal(libc::SIGINT);
    assert_eq!(other.ended(), 130);
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn ctrl_d_after_ctrl_c_still_waits_for_another_clients_turn_to_stop() {
    let lab = Lab::with_model(r#"[{"text": "Here."}, {"text": "One two three", "stall": true}]"#);
    assert_eq!(lab.run(&["Hello?"], "").status, 0);
    let conversation = only_conversation(&lab);
    let mut shown = Running::start(lab.agent(&["--resume", &conversation]));
    shown.wait_for_out("> Hello?\nHere.\n");
    let mut other = Running::start(lab.agent(&["--resume", &conversation, "Count."]));
    shown.wait_for_out("\n> Count.\nOne");
    // The Harness is slow to stop the turn: Ctrl-C has asked, and Ctrl-D comes before it has.
    let harness = lab.children[0].id() as libc::pid_t;
    // SAFETY: the process is this test's own child, which it has not waited for.
    unsafe { libc::kill(harness, libc::SIGSTOP) };
    shown.signal(libc::SIGINT);
    std::thread::sleep(Duration::from_millis(300));
    drop(shown.child.stdin.take());
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        shown.child.try_wait().unwrap().is_none(),
        "the client left before the turn it asked to stop had stopped"
    );
    // SAFETY: as above.
    unsafe { libc::kill(harness, libc::SIGCONT) };
    shown.wait_for_err("Stopped.\n");
    assert_eq!(shown.ended(), 0);
    assert_eq!(other.ended(), 130);
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn a_line_typed_during_another_clients_turn_is_sent_after_it_and_ctrl_c_drops_it() {
    let script = r#"[{"text": "Here."}, {"text": "One two three", "stall": true}, {"text": "Next."}]"#;
    // Down a pipe, where nothing drops what was written: the line waits for the turn on the screen, and is sent once.
    let lab = Lab::with_model(script);
    assert_eq!(lab.run(&["Hello?"], "").status, 0);
    let conversation = only_conversation(&lab);
    let mut shown = Running::start(lab.agent(&["--resume", &conversation]));
    shown.wait_for_out("> Hello?\nHere.\n");
    let mut other = Running::start(lab.agent(&["--resume", &conversation, "Count."]));
    shown.wait_for_out("\n> Count.\nOne");
    let mut typing = shown.child.stdin.take().expect("its input");
    typing.write_all(b"And then?\n").expect("typing");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        lab.requests().lines().count(),
        2,
        "the line was sent before the turn ended"
    );
    other.signal(libc::SIGINT);
    assert_eq!(other.ended(), 130);
    shown.wait_for_out("Next.\n");
    drop(typing);
    assert_eq!(shown.ended(), 0);
    assert_eq!(lab.requests().matches("And then?").count(), 1);

    // At a terminal, Ctrl-C drops what was typed and not yet read, as it does for any program.
    let lab = Lab::with_model(script);
    assert_eq!(lab.run(&["Hello?"], "").status, 0);
    let conversation = only_conversation(&lab);
    let (mut keys, screen) = terminal();
    let mut shown = lab.agent(&["--resume", &conversation]);
    shown.stdin(screen);
    let mut shown = Running::start(shown);
    shown.wait_for_out("> Hello?\nHere.\n");
    let mut other = Running::start(lab.agent(&["--resume", &conversation, "Count."]));
    shown.wait_for_out("\n> Count.\nOne");
    keys.write_all(b"draft of a message\n").expect("typing");
    std::thread::sleep(Duration::from_millis(300));
    // The key, which has the terminal drop the draft, and the signal the key sends a program in the foreground.
    keys.write_all(&[0x03]).expect("Ctrl-C");
    std::thread::sleep(Duration::from_millis(100));
    shown.signal(libc::SIGINT);
    shown.wait_for_err("Stopped.\n");
    assert_eq!(other.ended(), 130);
    // Ctrl-D, at the prompt it is back at.
    keys.write_all(&[0x04]).expect("Ctrl-D");
    assert_eq!(shown.ended(), 0);
    assert!(!lab.requests().contains("draft of a message"), "{}", lab.requests());
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn a_second_ctrl_c_leaves_a_sign_in_the_harness_does_not_answer() {
    let mut lab = Lab::empty();
    lab.harness(uid());
    let mut signing_in = Running::start(lab.agent(&["login", "openai"]));
    signing_in.wait_for_err("How do you want to sign in to OpenAI?");
    // SAFETY: the process is this test's own child, which it has not waited for.
    unsafe { libc::kill(lab.children[0].id() as libc::pid_t, libc::SIGSTOP) };
    signing_in.signal(libc::SIGINT);
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        signing_in.child.try_wait().unwrap().is_none(),
        "the first Ctrl-C waits for the Harness"
    );
    signing_in.signal(libc::SIGINT);
    wait_for("the client to leave", || {
        signing_in
            .child
            .try_wait()
            .expect("crowbar-agent is waited for")
            .is_some()
    });
    assert_eq!(signing_in.ended(), 130);
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn a_harness_that_serves_another_account_refuses_the_client() {
    let mut lab = Lab::empty();
    lab.harness(uid() + 1);
    // The socket is there, and takes a connection: the refusal is the Harness's own.
    UnixStream::connect(lab.directory.join("runtime/crowbar-agent/person")).expect("the person socket");
    let ran = lab.run(&["--list"], "");
    assert_eq!((ran.status, ran.out.as_str()), (1, ""));
    assert!(
        ran.err.contains("closed the connection: it serves another account"),
        "{ran:?}"
    );
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn with_no_harness_running_the_client_says_so_and_starts_none() {
    let lab = Lab::empty();
    let ran = lab.run(&["Hello?"], "");
    assert_eq!(ran.status, 1);
    let socket = lab.directory.join("runtime/crowbar-agent/person");
    assert!(
        ran.err.starts_with(&format!(
            "crowbar-agent: The Harness is not running: nothing answers at {}",
            socket.display()
        )),
        "{ran:?}"
    );
    assert!(!socket.exists() && !lab.state().exists());
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn with_no_model_signed_in_the_client_says_so_and_the_harness_goes_on() {
    let mut lab = Lab::empty();
    lab.harness(uid());
    let hint = "Sign in to one with `crowbar-agent login`, or choose one with `crowbar-agent model`.\n";
    let ran = lab.run(&["Hello?"], "");
    assert_eq!(
        (ran.status, ran.out.as_str(), ran.err),
        (1, "", format!("crowbar-agent: No Model is signed in. {hint}"))
    );
    let status = lab.run(&["status"], "");
    assert_eq!(
        (status.status, status.out),
        (1, format!("No Model is signed in. {hint}"))
    );
    // The Harness goes on, and the message that was refused started no conversation.
    let listed = lab.run(&["--list"], "");
    assert_eq!((listed.status, listed.err.as_str()), (0, "No conversations yet.\n"));
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn a_model_that_fails_fails_the_turn_with_the_reason() {
    let lab = Lab::with_model(r#"[{"status": 401, "message": "No such key"}, {"text": "Here."}]"#);
    let failed = lab.run(&["Hello?"], "");
    assert_eq!((failed.status, failed.out.as_str()), (1, ""));
    assert!(
        failed.err.starts_with("crowbar-agent: The Model gave no answer: 401") && failed.err.contains("No such key"),
        "{failed:?}"
    );
    let conversation = only_conversation(&lab);
    assert_eq!(lab.run(&["--resume", &conversation, "And now?"], "").out, "Here.\n");
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn a_conversation_that_is_piped_in_ends_as_its_newest_turn_did() {
    let lab =
        Lab::with_model(r#"[{"text": "Here."}, {"status": 401, "message": "No such key"}, {"text": "Here again."}]"#);
    let failed = lab.run(&[], "First?\nSecond?\n");
    assert_eq!((failed.status, failed.out.as_str()), (1, "Here.\n"), "{failed:?}");
    let conversation = only_conversation(&lab);
    // Resumed and left with nothing said, it ends as its newest turn did before: a failure still.
    let stood = lab.run(&["--resume", &conversation], "");
    assert_eq!(
        (stood.status, stood.out.as_str()),
        (1, "> First?\nHere.\n> Second?\n"),
        "{stood:?}"
    );
    let answered = lab.run(&["--resume", &conversation], "Third?\n");
    assert_eq!(answered.status, 0, "{answered:?}");
    // What was typed is not echoed down a pipe: the turn that failed shows its message and no answer.
    assert_eq!(
        answered.out, "> First?\nHere.\n> Second?\nHere again.\n",
        "{answered:?}"
    );
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn an_answer_that_is_cut_short_starts_over_and_the_client_says_so() {
    let lab = Lab::with_model(r#"[{"text": "Half an answer", "cut": true}, {"text": "A whole answer."}]"#);
    let ran = lab.run(&["Hello?"], "");
    assert_eq!(
        (ran.status, ran.out.as_str()),
        (0, "Half\nA whole answer.\n"),
        "{ran:?}"
    );
    assert!(
        ran.err
            .contains("The Model is asked again, and its answer starts over.\n"),
        "{ran:?}"
    );
    assert!(ran.err.contains("Trying again in 2 seconds.\n"), "{ran:?}");
    // The conversation keeps the answer that was whole.
    let conversation = only_conversation(&lab);
    assert_eq!(
        lab.run(&["--resume", &conversation], "").out,
        "> Hello?\nA whole answer.\n"
    );
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn the_person_signs_in_with_an_api_key_and_signs_out() {
    let mut lab = Lab::empty();
    lab.harness(uid());
    // How to sign in, the key, and the first Model of the list.
    let signed_in = lab.run(&["login", "openai"], "api_key\nsk-a-key-for-the-test\n1\n");
    assert_eq!(signed_in.status, 0, "{signed_in:?}");
    assert!(signed_in.err.contains(
        "How do you want to sign in to OpenAI?\n  1. Sign in with ChatGPT (oauth)\n  2. OpenAI API key (api_key)\n"
    ));
    assert!(
        signed_in.err.contains("Which Model should the Agent use?\n  1. "),
        "{signed_in:?}"
    );
    assert!(
        signed_in
            .err
            .ends_with("Signed in to OpenAI. The Agent uses Chat Latest.\n"),
        "{signed_in:?}"
    );
    assert!(
        !signed_in.err.contains("sk-a-key") && signed_in.out.is_empty(),
        "{signed_in:?}"
    );

    let kept = lab.state().join("credentials.json");
    assert_eq!(
        fs::metadata(&kept).expect("the credentials").permissions().mode() & 0o777,
        0o600
    );
    assert!(fs::read_to_string(&kept).unwrap().contains("sk-a-key-for-the-test"));

    let status = lab.run(&["status"], "");
    assert_eq!(status.status, 0, "{status:?}");
    assert!(
        status
            .out
            .starts_with("Signed in to OpenAI (openai), with an API key.\nThe Agent uses "),
        "{status:?}"
    );
    assert!(status.out.ends_with(", from OpenAI.\n"), "{status:?}");

    // The Models signed in to, with the one in use marked, and another chosen by its name.
    let models = lab.run(&["model"], "");
    let in_use: Vec<&str> = models.out.lines().filter(|line| line.starts_with("* ")).collect();
    assert_eq!(in_use.len(), 1, "{models:?}");
    let other = models
        .out
        .lines()
        .find(|line| line.starts_with("  openai/"))
        .expect("a second Model");
    let other = other.split_whitespace().next().unwrap();
    let chosen = lab.run(&["model", other], "");
    assert_eq!(chosen.status, 0, "{chosen:?}");
    assert!(lab.run(&["model"], "").out.contains(&format!("* {other}  ")));
    assert_eq!(lab.run(&["model", "openai/no-such-model"], "").status, 1);

    let signed_out = lab.run(&["logout"], "");
    assert_eq!(
        (signed_out.status, signed_out.err.as_str()),
        (0, "Signed out of OpenAI.\n")
    );
    assert!(!fs::read_to_string(&kept).unwrap().contains("sk-a-key"));
    assert_eq!(lab.run(&["status"], "").status, 1);

    // Left at the choice of a Model, by Ctrl-D: signed in all the same, and told which Models none is chosen of.
    let left = lab.run(&["login", "openai"], "api_key\nsk-a-key-for-the-test\n");
    assert_eq!(left.status, 0, "{left:?}");
    assert!(
        left.err
            .ends_with("Signed in to OpenAI. None of its Models is chosen: choose one with `crowbar-agent model`.\n"),
        "{left:?}"
    );
    assert!(
        lab.run(&["status"], "")
            .out
            .starts_with("Signed in to OpenAI (openai), with an API key.\n")
    );
}

#[test]
#[ignore = "needs the bundled Harness and Node.js: `base/void_base.py agent` runs it"]
fn a_sign_in_opens_a_browser_on_a_desktop_and_prints_the_address_without_one() {
    let mut lab = Lab::empty();
    lab.harness(uid());
    // A browser opener of the test's own, first on the path, which keeps
    // the address it was given, and then stays, as an opener does that
    // becomes the browser when none was running.
    let tools = lab.directory.join("tools");
    let opened = lab.directory.join("opened");
    fs::create_dir(&tools).unwrap();
    fs::write(
        tools.join("xdg-open"),
        format!(
            "#!/bin/sh\nprintf '%s' \"$1\" > '{}'\nexec sleep 20\n",
            opened.display()
        ),
    )
    .unwrap();
    fs::set_permissions(tools.join("xdg-open"), fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", tools.display(), std::env::var("PATH").unwrap_or_default());
    // By OAuth, with a code pasted from a browser, and then the person leaves: Ctrl-D cancels the sign-in.
    let typed = "oauth\ncopy_code\n";

    let mut console = lab.agent(&["login", "anthropic"]);
    console.env("PATH", &path);
    let console = Ran::of(run(console, typed));
    assert!(
        console
            .err
            .contains("Open this address in a browser to sign in:\n\n  https://claude.ai/oauth/authorize?"),
        "{console:?}"
    );
    assert!(
        console.err.ends_with("crowbar-agent: The sign-in was cancelled.\n"),
        "{console:?}"
    );
    assert_eq!(console.status, 130);
    assert!(!opened.exists(), "a browser was opened with no desktop to show it");

    let mut desktop = lab.agent(&["login", "anthropic"]);
    desktop.env("PATH", &path).env("WAYLAND_DISPLAY", "wayland-0");
    let began = Instant::now();
    let desktop = Ran::of(run(desktop, typed));
    assert!(desktop.err.contains("A browser was opened to sign in."), "{desktop:?}");
    // The sign-in went on, to its question and the person's leaving, while the opener was still there.
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "the client waited for the browser opener to end"
    );
    assert_eq!(desktop.status, 130, "{desktop:?}");
    assert!(
        fs::read_to_string(&opened)
            .unwrap()
            .starts_with("https://claude.ai/oauth/authorize?")
    );
    assert!(!lab.state().join("credentials.json").exists());
}
