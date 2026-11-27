use serde_json::{Value, json};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_crowbar-lever");
const STUB_BIN: &str = env!("CARGO_BIN_EXE_crowbar-stub-harness");
const MAX_FRAME: usize = 4 * 1024 * 1024;

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
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

struct RunningLever {
    child: Option<Child>,
    state: PathBuf,
    surface: PathBuf,
    person: PathBuf,
}

impl RunningLever {
    fn start(dir: &Path, shell_uid: u32, person_uid: u32) -> Self {
        Self::start_with_extra(dir, shell_uid, person_uid, &[])
    }

    fn start_with_stub(dir: &Path, uid: u32, script: &Path, log: Option<&Path>, idle_ms: Option<u64>) -> Self {
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

    fn start_with_extra(dir: &Path, shell_uid: u32, person_uid: u32, extra: &[String]) -> Self {
        Self::start_with_env(dir, shell_uid, person_uid, extra, &[])
    }

    fn start_with_env(dir: &Path, shell_uid: u32, person_uid: u32, extra: &[String], env: &[(&str, &str)]) -> Self {
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
        // Lever reads the person's OpenCode settings: in a test, the person
        // has none, unless the test gives them some.
        let home = dir.join("lever-home");
        cmd.env("HOME", &home).env("XDG_CONFIG_HOME", home.join(".config"));
        for key in ["OPENCODE_CONFIG", "OPENCODE_CONFIG_DIR", "OPENCODE_CONFIG_CONTENT"] {
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

    fn wait_for_sockets(&mut self) {
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

    fn pid(&self) -> u32 {
        self.child.as_ref().unwrap().id()
    }

    fn signal(&self, sig: libc::c_int) {
        let pid = self.pid() as libc::pid_t;
        // SAFETY: sending a signal to our own child process.
        assert_eq!(unsafe { libc::kill(pid, sig) }, 0);
    }

    fn wait(&mut self) -> std::process::ExitStatus {
        self.child.take().unwrap().wait().unwrap()
    }

    fn kill_and_wait(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(self.state.join("lever.log")).unwrap_or_default()
    }
}

impl Drop for RunningLever {
    fn drop(&mut self) {
        self.kill_and_wait();
    }
}

fn my_uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

fn encode_frame(msg: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(msg).unwrap();
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

fn write_frame(stream: &mut UnixStream, msg: &Value) -> io::Result<()> {
    stream.write_all(&encode_frame(msg))
}

fn read_frame(stream: &mut UnixStream) -> io::Result<Value> {
    read_frame_up_to(stream, MAX_FRAME)
}

fn read_frame_up_to(stream: &mut UnixStream, limit: usize) -> io::Result<Value> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    assert!(len <= limit, "unexpectedly large frame: {len}");
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body).unwrap())
}

fn connect(path: &Path) -> UnixStream {
    let stream = UnixStream::connect(path).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
    stream
}

fn handshake(path: &Path) -> UnixStream {
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
fn send_until_stalled(peer: &mut UnixStream, request: &[u8]) -> usize {
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

fn assert_closed_without_reply(mut stream: UnixStream) {
    let mut buf = [0u8; 64];
    match stream.read(&mut buf) {
        Ok(0) => {}
        Err(e) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe) => {}
        other => panic!("expected immediate close with no reply, got {other:?}"),
    }
}

#[test]
fn both_sockets_speak_version_one_and_answer_status() {
    let dir = TempDir::new();
    let uid = my_uid();
    let mut lever = RunningLever::start(&dir.0, uid, uid);

    for path in [&lever.surface, &lever.person] {
        let mut peer = handshake(path);
        write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
        let reply = read_frame(&mut peer).unwrap();
        assert_eq!(
            reply,
            json!({
                "reply": "status",
                "version": env!("CARGO_PKG_VERSION"),
                "harness_started": false,
                "harnesses": [],
                "free_hosted_models": false,
                "models_chosen": false,
                "default_mode": "ask-first"
            })
        );
    }

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn peer_uid_is_checked_on_each_socket_independently() {
    let uid = my_uid();
    let wrong_uid = uid.checked_add(1).unwrap();

    // Surface expects another UID; person expects ours.
    let dir1 = TempDir::new();
    let mut lever1 = RunningLever::start(&dir1.0, wrong_uid, uid);
    {
        let mut surface = connect(&lever1.surface);
        let _ = write_frame(&mut surface, &json!({ "version": 1 }));
        assert_closed_without_reply(surface);

        let mut person = handshake(&lever1.person);
        write_frame(&mut person, &json!({ "op": "status" })).unwrap();
        assert_eq!(read_frame(&mut person).unwrap()["reply"], "status");
    }
    lever1.signal(libc::SIGTERM);
    assert_eq!(lever1.wait().code(), Some(0));
    assert!(
        lever1.log_text().contains(&format!("refused user ID {uid}")),
        "expected refusal in log: {}",
        lever1.log_text()
    );

    // Surface expects ours; person expects another UID.
    let dir2 = TempDir::new();
    let mut lever2 = RunningLever::start(&dir2.0, uid, wrong_uid);
    {
        let mut person = connect(&lever2.person);
        let _ = write_frame(&mut person, &json!({ "version": 1 }));
        assert_closed_without_reply(person);

        let mut surface = handshake(&lever2.surface);
        write_frame(&mut surface, &json!({ "op": "status" })).unwrap();
        assert_eq!(read_frame(&mut surface).unwrap()["reply"], "status");
    }
    lever2.signal(libc::SIGTERM);
    assert_eq!(lever2.wait().code(), Some(0));
    assert!(
        lever2.log_text().contains(&format!("refused user ID {uid}")),
        "expected refusal in log: {}",
        lever2.log_text()
    );
}

#[test]
fn framing_handles_split_writes_pipelining_bad_version_and_oversized_frames() {
    let dir = TempDir::new();
    let uid = my_uid();
    let mut lever = RunningLever::start(&dir.0, uid, uid);

    // Split bytes: send handshake and two status requests one byte at a time.
    {
        let mut peer = connect(&lever.surface);
        let mut bytes = encode_frame(&json!({ "version": 1 }));
        bytes.extend_from_slice(&encode_frame(&json!({ "op": "status" })));
        bytes.extend_from_slice(&encode_frame(&json!({ "op": "status" })));
        for chunk in bytes.chunks(1) {
            peer.write_all(chunk).unwrap();
        }
        assert_eq!(read_frame(&mut peer).unwrap(), json!({ "version": 1 }));
        assert_eq!(read_frame(&mut peer).unwrap()["reply"], "status");
        assert_eq!(read_frame(&mut peer).unwrap()["reply"], "status");
    }

    // Bad version handshakes close immediately without replying.
    for bad in [
        json!({ "version": 2 }),
        json!({ "version": 1, "extra": true }),
        json!([1]),
        json!({ "op": "status" }),
    ] {
        let mut peer = connect(&lever.person);
        write_frame(&mut peer, &bad).unwrap();
        assert_closed_without_reply(peer);
    }

    // Oversized length header after a valid status frame: earlier frame is answered, then closed.
    {
        let mut peer = handshake(&lever.surface);
        let mut payload = encode_frame(&json!({ "op": "status" }));
        payload.extend_from_slice(&((MAX_FRAME as u32) + 1).to_be_bytes());
        peer.write_all(&payload).unwrap();
        assert_eq!(read_frame(&mut peer).unwrap()["reply"], "status");
        assert_closed_without_reply(peer);
    }

    // Unknown op gets a structured error reply and leaves the connection alive.
    {
        let mut peer = handshake(&lever.person);
        write_frame(
            &mut peer,
            &json!({ "op": "unknown-secret-op", "text": "DO_NOT_LOG_THIS_SECRET" }),
        )
        .unwrap();
        let err = read_frame(&mut peer).unwrap();
        assert_eq!(err["reply"], "error");
        assert_eq!(err["code"], "unsupported");

        write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
        assert_eq!(read_frame(&mut peer).unwrap()["reply"], "status");
    }

    // A request that is not an object with a string op gets a structured
    // error reply and leaves the connection alive.
    {
        let mut peer = handshake(&lever.person);
        for bad in [
            json!(["status"]),
            json!({ "op": 7 }),
            json!({ "text": "DO_NOT_LOG_THIS_SECRET" }),
        ] {
            write_frame(&mut peer, &bad).unwrap();
            let err = read_frame(&mut peer).unwrap();
            assert_eq!(err["reply"], "error");
            assert_eq!(err["code"], "invalid-request");
        }
        write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
        assert_eq!(read_frame(&mut peer).unwrap()["reply"], "status");
    }

    // A frame that is not JSON closes the connection without a reply.
    {
        let mut peer = handshake(&lever.surface);
        let body = b"DO_NOT_LOG_THIS_SECRET";
        let mut payload = (body.len() as u32).to_be_bytes().to_vec();
        payload.extend_from_slice(body);
        peer.write_all(&payload).unwrap();
        assert_closed_without_reply(peer);
    }

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    let log = lever.log_text();
    assert!(
        !log.contains("DO_NOT_LOG_THIS_SECRET"),
        "peer payload leaked into log: {log}"
    );
    assert!(!log.contains("unknown-secret-op"), "peer op leaked into log: {log}");
    assert!(
        !log.contains(&(MAX_FRAME + 1).to_string()),
        "peer length header leaked into log: {log}"
    );
}

#[test]
fn state_is_private_database_is_wal_and_clean_stop_removes_sockets() {
    let dir = TempDir::new();
    let uid = my_uid();
    let mut lever = RunningLever::start(&dir.0, uid, uid);

    let state_meta = std::fs::metadata(&lever.state).unwrap();
    assert_eq!(state_meta.permissions().mode() & 0o777, 0o700);
    assert_eq!(state_meta.uid(), uid);

    let mut names = Vec::new();
    for entry in std::fs::read_dir(&lever.state).unwrap() {
        let entry = entry.unwrap();
        let meta = entry.metadata().unwrap();
        assert_eq!(
            meta.permissions().mode() & 0o777,
            0o600,
            "{} must be mode 600",
            entry.path().display()
        );
        assert_eq!(meta.uid(), uid);
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    assert!(
        names.iter().any(|n| n == "lever.sqlite"),
        "missing lever.sqlite in {names:?}"
    );
    assert!(names.iter().any(|n| n == "lever.log"), "missing lever.log in {names:?}");
    assert!(
        names.iter().any(|n| n == "running"),
        "missing running marker in {names:?}"
    );

    // Person socket directory is 0700 and socket is 0600; surface socket is 0660.
    assert_eq!(
        std::fs::metadata(lever.person.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&lever.person).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(&lever.surface).unwrap().permissions().mode() & 0o777,
        0o660
    );

    // Hold an open connection when SIGTERM arrives; Lever closes sockets and exits 0.
    let active = handshake(&lever.surface);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    assert_closed_without_reply(active);
    assert!(!lever.surface.exists(), "surface socket must be removed on clean stop");
    assert!(!lever.person.exists(), "person socket must be removed on clean stop");
    assert!(
        !lever.state.join("running").exists(),
        "running marker must be removed on clean stop"
    );

    let log = lever.log_text();
    assert!(log.contains("start:"), "missing start event: {log}");
    assert!(log.contains("listening:"), "missing listening event: {log}");
    assert!(log.contains("connection:"), "missing connection event: {log}");
    assert!(log.contains("stop: SIGTERM"), "missing stop event: {log}");
    assert!(!log.contains("restart:"), "clean start should not log restart: {log}");

    // SQLite database persists and is in WAL mode.
    let conn = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    let mode: String = conn.pragma_query_value(None, "journal_mode", |r| r.get(0)).unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "wal");
}

#[test]
fn restart_after_kill_replaces_stale_sockets_and_logs_restart_and_sigint_stops_cleanly() {
    let dir = TempDir::new();
    let uid = my_uid();
    let mut first = RunningLever::start(&dir.0, uid, uid);
    let old_pid = first.pid();
    first.kill_and_wait();
    assert!(first.surface.exists(), "SIGKILL leaves stale socket behind");

    let mut second = RunningLever::start(&dir.0, uid, uid);
    let mut peer = handshake(&second.surface);
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["reply"], "status");

    second.signal(libc::SIGINT);
    assert_eq!(second.wait().code(), Some(0));

    let log = second.log_text();
    assert!(
        log.contains(&format!(
            "restart: the Lever before this one, process {old_pid}, ended without stopping"
        )),
        "expected restart entry for {old_pid}: {log}"
    );
    assert!(log.contains("stop: SIGINT"), "expected SIGINT stop entry: {log}");
}

#[test]
fn replies_reach_a_peer_that_has_sent_all_it_will() {
    // More replies than a socket holds, and fewer than the mark at which
    // Lever stops reading a peer, so some still wait inside Lever when it
    // finds the peer has stopped sending.
    const REQUESTS: usize = 7_000;

    let dir = TempDir::new();
    let uid = my_uid();
    let mut lever = RunningLever::start(&dir.0, uid, uid);

    // The peer sends its version and its requests, closes its sending side,
    // and reads nothing until Lever has seen that.
    let mut peer = connect(&lever.person);
    let mut bytes = encode_frame(&json!({ "version": 1 }));
    let request = encode_frame(&json!({ "op": "status" }));
    for _ in 0..REQUESTS {
        bytes.extend_from_slice(&request);
    }
    peer.write_all(&bytes).unwrap();
    peer.shutdown(std::net::Shutdown::Write).unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    while !lever.log_text().contains("connection: the person left") {
        assert!(
            Instant::now() < deadline,
            "Lever did not see the peer stop sending: {}",
            lever.log_text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    assert_eq!(read_frame(&mut peer).unwrap(), json!({ "version": 1 }));
    for n in 0..REQUESTS {
        let reply = read_frame(&mut peer).unwrap_or_else(|err| panic!("reply {n} of {REQUESTS} was lost: {err}"));
        assert_eq!(reply["reply"], "status");
    }
    assert_closed_without_reply(peer);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_peer_that_reads_nothing_is_held_and_loses_nothing_it_sent() {
    let dir = TempDir::new();
    let uid = my_uid();
    let mut lever = RunningLever::start(&dir.0, uid, uid);

    let mut peer = handshake(&lever.person);
    let request = encode_frame(&json!({ "op": "status" }));

    // Requests go out and nothing is read. Lever stops reading the peer once
    // enough replies wait for it, so the peer's writes stall. A Lever that
    // took every request would have this peer send without end, or close it.
    let sent = send_until_stalled(&mut peer, &request);

    // Lever still answers the next peer.
    let mut next = handshake(&lever.person);
    write_frame(&mut next, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut next).unwrap()["reply"], "status");

    // When the peer reads, every request it sent whole is answered.
    for n in 0..sent / request.len() {
        let reply = read_frame(&mut peer).unwrap_or_else(|err| panic!("reply {n} was lost: {err}"));
        assert_eq!(reply["reply"], "status");
    }

    // So is the one the stall cut short, once the rest of it is sent.
    peer.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
    peer.write_all(&request[sent % request.len()..]).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["reply"], "status");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    assert!(
        !lever.log_text().contains("connection: closed"),
        "a peer that read nothing was closed: {}",
        lever.log_text()
    );
}

#[test]
fn a_peer_that_keeps_sending_and_reading_is_answered_beside_the_others() {
    const REQUESTS: usize = 400_000;

    let dir = TempDir::new();
    let uid = my_uid();
    let mut lever = RunningLever::start(&dir.0, uid, uid);

    let busy = handshake(&lever.person);
    busy.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    busy.set_write_timeout(Some(Duration::from_secs(20))).unwrap();
    let mut busy_writer = busy.try_clone().unwrap();
    let mut busy_reader = busy;

    // Far more requests than Lever could answer in one wake, so the shell
    // below is answered while they are still coming.
    let writer = std::thread::spawn(move || {
        let request = encode_frame(&json!({ "op": "status" }));
        let chunk: Vec<u8> = request.iter().copied().cycle().take(request.len() * 1000).collect();
        for _ in 0..REQUESTS / 1000 {
            busy_writer.write_all(&chunk)?;
        }
        Ok::<(), io::Error>(())
    });
    let answered = std::sync::Arc::new(AtomicUsize::new(0));
    let counted = answered.clone();
    let reader = std::thread::spawn(move || {
        for _ in 0..REQUESTS {
            let reply = read_frame(&mut busy_reader)?;
            assert_eq!(reply["reply"], "status");
            counted.fetch_add(1, Ordering::Relaxed);
        }
        Ok::<UnixStream, io::Error>(busy_reader)
    });

    // Once the busy peer is under way, the shell is answered within its
    // usual time.
    let deadline = Instant::now() + Duration::from_secs(20);
    while answered.load(Ordering::Relaxed) < 1000 {
        assert!(Instant::now() < deadline, "the busy peer was not answered");
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut shell = handshake(&lever.surface);
    write_frame(&mut shell, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut shell).unwrap()["reply"], "status");

    writer.join().unwrap().expect("the busy peer's requests were all taken");
    let mut busy = reader.join().unwrap().expect("the busy peer's replies all came");

    // The busy peer was served, not closed.
    write_frame(&mut busy, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut busy).unwrap()["reply"], "status");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    assert!(
        !lever.log_text().contains("connection: closed"),
        "a peer that read its replies was closed: {}",
        lever.log_text()
    );
}

#[test]
fn each_socket_keeps_its_own_count_of_connections() {
    const MAX_PEERS: usize = 64;

    let dir = TempDir::new();
    let uid = my_uid();
    let mut lever = RunningLever::start(&dir.0, uid, uid);

    let mut held: Vec<UnixStream> = (0..MAX_PEERS).map(|_| handshake(&lever.person)).collect();

    // One more on the person socket is closed without a reply.
    let mut extra = connect(&lever.person);
    let _ = write_frame(&mut extra, &json!({ "version": 1 }));
    assert_closed_without_reply(extra);

    // The surface socket has its own count, so the shell is still answered,
    // and so is every connection the person socket already holds.
    let mut shell = handshake(&lever.surface);
    write_frame(&mut shell, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut shell).unwrap()["reply"], "status");
    for peer in &mut held {
        write_frame(peer, &json!({ "op": "status" })).unwrap();
        assert_eq!(read_frame(peer).unwrap()["reply"], "status");
    }

    // When one leaves, the person socket takes another.
    drop(held.pop());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut peer = connect(&lever.person);
        let _ = write_frame(&mut peer, &json!({ "version": 1 }));
        if matches!(read_frame(&mut peer), Ok(reply) if reply == json!({ "version": 1 })) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the person socket took no connection after one left"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    assert!(
        lever.log_text().contains(&format!(
            "refused the person, which already has {MAX_PEERS} connections open"
        )),
        "expected the refusal in the log: {}",
        lever.log_text()
    );
}

/// How many times Lever's log says one of the person's connections went.
fn departures(lever: &RunningLever) -> usize {
    lever
        .log_text()
        .lines()
        .filter(|line| {
            line.ends_with("connection: the person left") || line.ends_with("connection: the person disconnected")
        })
        .count()
}

fn wait_for_departures(lever: &RunningLever, wanted: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while departures(lever) < wanted {
        assert!(
            Instant::now() < deadline,
            "expected {wanted} departures in the log: {}",
            lever.log_text()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn each_peer_that_goes_is_logged_once() {
    let dir = TempDir::new();
    let uid = my_uid();
    let mut lever = RunningLever::start(&dir.0, uid, uid);
    let request = encode_frame(&json!({ "op": "status" }));

    // A peer Lever has stopped reading, because replies wait for it, closes.
    {
        let mut held = handshake(&lever.person);
        send_until_stalled(&mut held, &request);
    }
    wait_for_departures(&lever, 1);

    // A peer sends requests and closes without reading their replies.
    {
        let mut hasty = handshake(&lever.person);
        let requests: Vec<u8> = request.iter().copied().cycle().take(request.len() * 100).collect();
        hasty.write_all(&requests).unwrap();
    }
    wait_for_departures(&lever, 2);

    // A peer states its version and closes.
    drop(handshake(&lever.person));
    wait_for_departures(&lever, 3);

    // Lever kept none of them: it still answers, and stops cleanly.
    let mut next = handshake(&lever.person);
    write_frame(&mut next, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut next).unwrap()["reply"], "status");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    assert_eq!(departures(&lever), 3, "{}", lever.log_text());
}

#[test]
fn thread_streams_answer_and_acts_and_persists_across_restart() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "turns": [{
                "steps": [
                    { "kind": "text", "text": "Checking " },
                    { "kind": "act", "id": "acp-call-1", "tool_kind": "execute", "title": "Run ls", "status": "in_progress" },
                    { "kind": "act_update", "id": "acp-call-1", "status": "completed", "content": "Cargo.toml\nsrc" },
                    { "kind": "text", "text": "done." },
                    { "kind": "title", "title": "Inspect workspace" }
                ]
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);

    // Before any thread runs, harness is configured but not started.
    let mut peer = handshake(&lever.person);
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut peer).unwrap();
    assert_eq!(status["reply"], "status");
    assert_eq!(status["harness_started"], false);
    assert_eq!(status["harnesses"][0]["name"], "Stub");

    // Start a thread with a message.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "List the files", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent = read_frame(&mut peer).unwrap();
    assert_eq!(sent["reply"], "sent");
    let thread_id = sent["thread"].as_str().unwrap().to_string();
    assert_eq!(sent["turn"], 1);

    let mut events = Vec::new();
    loop {
        let ev = read_frame(&mut peer).unwrap();
        let is_done = ev["event"] == "done";
        events.push(ev);
        if is_done {
            break;
        }
    }
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "message" && e["text"] == "List the files")
    );
    assert!(
        events
            .iter()
            .any(|e| e["event"] == "act" && e["title"] == "Run ls" && e["state"] == "done")
    );
    let combined: String = events
        .iter()
        .filter(|e| e["event"] == "text")
        .filter_map(|e| e["text"].as_str())
        .collect();
    assert_eq!(combined, "Checking done.");

    // Harness is now started.
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], true);

    // List shows the thread with its updated title.
    write_frame(&mut peer, &json!({ "op": "list" })).unwrap();
    let listed = read_frame(&mut peer).unwrap();
    assert_eq!(listed["reply"], "list");
    assert_eq!(listed["threads"].as_array().unwrap().len(), 1);
    assert_eq!(listed["threads"][0]["thread"], thread_id);
    assert_eq!(listed["threads"][0]["title"], "Inspect workspace");
    assert_eq!(listed["threads"][0]["state"], "done");
    assert_eq!(listed["threads"][0]["working"], false);

    // Stop Lever cleanly, restart, and read the thread from the database with no harness running.
    drop(peer);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    let mut restarted = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&restarted.person);
    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    let resumed = read_frame(&mut peer).unwrap();
    assert_eq!(resumed["reply"], "resumed");
    assert_eq!(resumed["thread"], thread_id);
    assert_eq!(resumed["title"], "Inspect workspace");
    let turn = &resumed["turns"][0];
    assert_eq!(turn["message"], "List the files");
    assert_eq!(turn["answer"], "Checking done.");
    assert_eq!(turn["state"], "done");
    assert_eq!(turn["acts"][0]["title"], "Run ls");
    assert_eq!(turn["acts"][0]["state"], "done");
    assert_eq!(turn["acts"][0]["detail"], "Cargo.toml\nsrc");

    // Reading the record did not start the harness.
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], false);

    restarted.signal(libc::SIGTERM);
    assert_eq!(restarted.wait().code(), Some(0));
}

#[test]
fn unknown_update_is_kept_as_json_and_shown_plainly_and_post_turn_updates_are_recorded() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    let during_unknown = json!({
        "sessionUpdate": "vendor_metric",
        "summary": "Index cache warmed",
        "count": 42
    });
    let after_unknown = json!({
        "sessionUpdate": "post_turn_audit",
        "summary": "Background check finished"
    });
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "turns": [{
                "steps": [
                    { "kind": "act", "id": "acp-late-call", "tool_kind": "read", "title": "Read file", "status": "in_progress" },
                    { "kind": "unknown", "update": during_unknown },
                    { "kind": "text", "text": "Primary answer." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 30 },
                    { "kind": "unknown", "update": after_unknown },
                    { "kind": "act_update", "id": "acp-late-call", "status": "completed", "content": "finished after turn" }
                ]
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "Check cache", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent = read_frame(&mut peer).unwrap();
    let thread_id = sent["thread"].as_str().unwrap().to_string();

    // Collect live events through `done` and the two post-turn updates.
    let mut saw_during_note = false;
    let mut saw_done = false;
    let mut saw_after_note = false;
    let mut saw_after_act = false;
    while !(saw_done && saw_after_note && saw_after_act) {
        let ev = read_frame(&mut peer).unwrap();
        if ev["event"] == "note" && ev["text"] == "Index cache warmed" {
            saw_during_note = true;
        } else if ev["event"] == "done" {
            saw_done = true;
        } else if ev["event"] == "note" && ev["text"] == "Background check finished" {
            assert!(saw_done, "post-turn note arrived before done");
            saw_after_note = true;
        } else if ev["event"] == "act" && ev["detail"] == "finished after turn" && ev["state"] == "done" {
            assert!(saw_done, "post-turn act update arrived before done");
            saw_after_act = true;
        }
    }
    assert!(saw_during_note);

    // Resume returns both notes and the post-turn act update.
    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    let resumed = read_frame(&mut peer).unwrap();
    let turn = &resumed["turns"][0];
    assert_eq!(turn["notes"].as_array().unwrap().len(), 2);
    assert_eq!(turn["notes"][0]["text"], "Index cache warmed");
    assert_eq!(turn["notes"][1]["text"], "Background check finished");
    assert_eq!(turn["acts"][0]["state"], "done");
    assert_eq!(turn["acts"][0]["detail"], "finished after turn");

    // Verify the database kept the JSON of each unknown update.
    let conn = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    let mut stmt = conn
        .prepare("SELECT raw_json, plain_text, after_turn FROM notes WHERE thread_id = ?1 ORDER BY seq ASC")
        .unwrap();
    let rows: Vec<(String, String, i64)> = stmt
        .query_map([&thread_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(serde_json::from_str::<Value>(&rows[0].0).unwrap(), during_unknown);
    assert_eq!(rows[0].1, "Index cache warmed");
    assert_eq!(rows[0].2, 0);
    assert_eq!(serde_json::from_str::<Value>(&rows[1].0).unwrap(), after_unknown);
    assert_eq!(rows[1].1, "Background check finished");
    assert_eq!(rows[1].2, 1);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn two_threads_work_concurrently_on_one_harness_and_stop_cancels_the_turn() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    let rpc_log = dir.0.join("rpc.jsonl");
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "turns": [
                {
                    "match": "slow task",
                    "steps": [{ "kind": "text", "text": "Working slowly..." }],
                    "stall": true
                },
                {
                    "match": "fast task",
                    "steps": [{ "kind": "text", "text": "Fast result." }]
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log), None);
    let mut peer_a = handshake(&lever.person);
    let mut peer_b = handshake(&lever.person);

    // Thread A starts and stalls mid-turn.
    write_frame(
        &mut peer_a,
        &json!({ "op": "send", "text": "Run slow task", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent_a = read_frame(&mut peer_a).unwrap();
    let thread_a = sent_a["thread"].as_str().unwrap().to_string();
    loop {
        let ev = read_frame(&mut peer_a).unwrap();
        if ev["event"] == "text" && ev["text"] == "Working slowly..." {
            break;
        }
    }

    // While Thread A is still working, Thread B starts and finishes independently.
    write_frame(
        &mut peer_b,
        &json!({ "op": "send", "text": "Run fast task", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent_b = read_frame(&mut peer_b).unwrap();
    let thread_b = sent_b["thread"].as_str().unwrap().to_string();
    assert_ne!(thread_a, thread_b);

    let mut b_text = String::new();
    loop {
        let ev = read_frame(&mut peer_b).unwrap();
        if ev["event"] == "text" {
            b_text.push_str(ev["text"].as_str().unwrap());
        }
        if ev["event"] == "done" {
            break;
        }
    }
    assert_eq!(b_text, "Fast result.");

    // List shows Thread A working and Thread B done.
    write_frame(&mut peer_b, &json!({ "op": "list" })).unwrap();
    let listed = read_frame(&mut peer_b).unwrap();
    let threads = listed["threads"].as_array().unwrap();
    let row_a = threads.iter().find(|t| t["thread"] == thread_a).unwrap();
    let row_b = threads.iter().find(|t| t["thread"] == thread_b).unwrap();
    assert_eq!(row_a["working"], true);
    assert_eq!(row_b["working"], false);

    // Stop Thread A and verify it finishes as `stopped` and sent `session/cancel` to the harness.
    write_frame(&mut peer_a, &json!({ "op": "stop", "thread": thread_a, "turn": 1 })).unwrap();
    let mut saw_ok = false;
    let mut saw_stopped = false;
    while !(saw_ok && saw_stopped) {
        let frame = read_frame(&mut peer_a).unwrap();
        if frame["reply"] == "ok" {
            saw_ok = true;
        } else if frame["event"] == "stopped" && frame["thread"] == thread_a {
            saw_stopped = true;
        }
    }

    // Verify one harness process served both threads and received `session/cancel`.
    let rpc_lines: Vec<Value> = std::fs::read_to_string(&rpc_log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        rpc_lines.iter().filter(|m| m["method"] == "initialize").count(),
        1,
        "one harness process must serve every thread"
    );
    assert_eq!(rpc_lines.iter().filter(|m| m["method"] == "session/new").count(), 2);
    assert_eq!(rpc_lines.iter().filter(|m| m["method"] == "session/prompt").count(), 2);
    assert_eq!(
        rpc_lines.iter().filter(|m| m["method"] == "session/cancel").count(),
        1,
        "stop must send session/cancel to the harness"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn no_harness_id_or_message_type_appears_on_levers_sockets() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    let forbidden = [
        "acp-secret-session-",
        "acp-secret-tool-99",
        "session/update",
        "session/new",
        "session/prompt",
        "session/cancel",
        "sessionUpdate",
        "sessionId",
        "toolCallId",
        "agent_message_chunk",
        "tool_call",
        "tool_call_update",
        "stopReason",
        "end_turn",
        "in_progress",
        "vendor_secret_tag",
        "acp-secret-tool-99-777",
        "-777",
    ];
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "session_prefix": "acp-secret-session-",
            "turns": [
                {
                    "steps": [
                        { "kind": "act", "id": "acp-secret-tool-99", "tool_kind": "execute", "title": "Run check", "status": "in_progress" },
                        { "kind": "act_update", "id": "acp-secret-tool-99", "status": "completed", "content": "ok" },
                        // An act with nothing to show but the harness's own raw value.
                        { "kind": "act", "id": "acp-secret-tool-100", "tool_kind": "other", "title": "Probe", "status": "completed",
                          "raw_output": { "sessionId": "acp-secret-session-1", "toolCallId": "acp-secret-tool-100", "exit": 0 } },
                        { "kind": "unknown", "update": { "sessionUpdate": "vendor_secret_tag", "summary": "Plain note" } },
                        // An update that names a tool call Lever has seen no act for.
                        // Its ID begins with one Lever does know, and all of it is taken out.
                        { "kind": "unknown", "update": { "sessionUpdate": "vendor_secret_tag", "toolCallId": "acp-secret-tool-99-777",
                          "message": "Waiting on acp-secret-tool-99-777" } },
                        { "kind": "text", "text": "All good." }
                    ]
                },
                {
                    "error": { "code": -32000, "message": "failed for acp-secret-session-1 and acp-secret-tool-99" }
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let mut seen_frames: Vec<String> = Vec::new();
    let mut record = |frame: Value| -> Value {
        seen_frames.push(frame.to_string());
        frame
    };

    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    record(read_frame(&mut peer).unwrap());

    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "First", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent = record(read_frame(&mut peer).unwrap());
    let thread_id = sent["thread"].as_str().unwrap().to_string();
    loop {
        let ev = record(read_frame(&mut peer).unwrap());
        if ev["event"] == "done" {
            break;
        }
    }

    // Trigger an error whose raw message mentions harness IDs.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Second" }),
    )
    .unwrap();
    record(read_frame(&mut peer).unwrap());
    loop {
        let ev = record(read_frame(&mut peer).unwrap());
        if ev["event"] == "failed" {
            break;
        }
    }

    write_frame(&mut peer, &json!({ "op": "list" })).unwrap();
    record(read_frame(&mut peer).unwrap());

    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    record(read_frame(&mut peer).unwrap());

    assert!(!seen_frames.is_empty());
    assert!(
        seen_frames
            .iter()
            .any(|frame| frame.contains("\"text\":\"Waiting on [redacted]\"")),
        "{seen_frames:?}"
    );
    for frame in &seen_frames {
        for bad in &forbidden {
            assert!(
                !frame.contains(bad),
                "harness token {bad:?} leaked into Lever socket frame: {frame}"
            );
        }
    }

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn harness_starts_on_demand_stops_when_idle_and_restarts_for_next_turn() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    let rpc_log = dir.0.join("rpc.jsonl");
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            // What the harness replays of a session as it loads it. Lever has
            // all of it in its own record already.
            "load_replay": [
                { "kind": "text", "text": "REPLAYED ANSWER" },
                { "kind": "act", "id": "acp-replayed-call", "title": "Replayed act", "status": "completed" },
                { "kind": "unknown", "update": { "sessionUpdate": "replayed_note", "summary": "Replayed note" } }
            ],
            "turns": [
                { "steps": [{ "kind": "text", "text": "First turn." }] },
                { "steps": [{ "kind": "text", "text": "Second turn." }] }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log), Some(200));
    let mut peer = handshake(&lever.person);

    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], false);

    // Turn 1 starts the harness.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "SUPER_SECRET_PERSON_TEXT", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent = read_frame(&mut peer).unwrap();
    let thread_id = sent["thread"].as_str().unwrap().to_string();
    while read_frame(&mut peer).unwrap()["event"] != "done" {}

    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], true);

    // Wait for the idle timeout to stop the harness.
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
        if read_frame(&mut peer).unwrap()["harness_started"] == false {
            break;
        }
        assert!(Instant::now() < deadline, "harness did not stop after idle timeout");
        std::thread::sleep(Duration::from_millis(30));
    }

    // Turn 2 on the same thread starts a fresh harness and loads the session.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Again" }),
    )
    .unwrap();
    let sent2 = read_frame(&mut peer).unwrap();
    assert_eq!(sent2["turn"], 2);
    while read_frame(&mut peer).unwrap()["event"] != "done" {}

    let rpc_text = std::fs::read_to_string(&rpc_log).unwrap();
    assert_eq!(
        rpc_text.matches("\"method\":\"initialize\"").count(),
        2,
        "harness must have started twice: {rpc_text}"
    );
    assert!(
        rpc_text.contains("\"method\":\"session/load\""),
        "resuming on a restarted harness should call session/load: {rpc_text}"
    );

    // The session was picked up again, so nothing says it began afresh, and
    // what the harness replayed as it loaded is in the record once.
    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    let resumed = read_frame(&mut peer).unwrap();
    // A stub that starts again begins its script again, so both turns got
    // the script's first answer.
    for turn in resumed["turns"].as_array().unwrap() {
        assert_eq!(turn["answer"], "First turn.", "{resumed}");
        assert_eq!(turn["acts"], json!([]), "{resumed}");
        assert_eq!(turn["notes"], json!([]), "{resumed}");
    }

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    let log = lever.log_text();
    assert!(log.contains("after idle timeout"), "expected idle stop in log: {log}");
    assert!(
        !log.contains("SUPER_SECRET_PERSON_TEXT"),
        "person text leaked to log: {log}"
    );
}

#[test]
fn harness_crash_and_lever_kill_record_interrupted_turns() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "turns": [
                {
                    "match": "crash now",
                    "steps": [{ "kind": "text", "text": "Half" }],
                    "crash": true
                },
                {
                    "match": "stall for kill",
                    "steps": [{ "kind": "text", "text": "Waiting" }],
                    "stall": true
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);

    // Case 1: harness crashes mid-turn.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "Please crash now", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent = read_frame(&mut peer).unwrap();
    let thread_id = sent["thread"].as_str().unwrap().to_string();
    loop {
        let ev = read_frame(&mut peer).unwrap();
        if ev["event"] == "interrupted" && ev["thread"] == thread_id && ev["turn"] == 1 {
            break;
        }
    }
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], false);

    // Case 2: Lever itself is killed mid-turn.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Please stall for kill" }),
    )
    .unwrap();
    let sent2 = read_frame(&mut peer).unwrap();
    assert_eq!(sent2["turn"], 2);
    loop {
        let ev = read_frame(&mut peer).unwrap();
        if ev["event"] == "text" && ev["text"] == "Waiting" {
            break;
        }
    }
    drop(peer);
    lever.kill_and_wait();

    // Restart Lever: turn 2 is recorded as interrupted, and turn 1 remains interrupted.
    let mut restarted = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&restarted.person);
    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    let resumed = read_frame(&mut peer).unwrap();
    assert_eq!(resumed["turns"][0]["state"], "interrupted");
    assert_eq!(resumed["turns"][1]["state"], "interrupted");
    assert_eq!(resumed["working"], false);

    restarted.signal(libc::SIGTERM);
    assert_eq!(restarted.wait().code(), Some(0));
}

/// Extracts every fenced ```json block from `lever/README.md`.
fn readme_json_blocks() -> Vec<Value> {
    let readme =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md")).expect("read lever/README.md");
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

#[test]
fn lever_speaks_every_message_documented_in_readme() {
    use std::collections::BTreeSet;

    let blocks = readme_json_blocks();
    let doc_ops: BTreeSet<String> = blocks
        .iter()
        .filter_map(|v| v.get("op").and_then(Value::as_str).map(str::to_string))
        .collect();
    let doc_replies: BTreeSet<String> = blocks
        .iter()
        .filter_map(|v| v.get("reply").and_then(Value::as_str).map(str::to_string))
        .collect();
    let doc_events: BTreeSet<String> = blocks
        .iter()
        .filter_map(|v| v.get("event").and_then(Value::as_str).map(str::to_string))
        .collect();

    assert!(
        blocks.contains(&json!({ "version": 1 })),
        "README must document version 1"
    );
    assert_eq!(
        doc_ops,
        BTreeSet::from(
            [
                "answer",
                "archive",
                "delete",
                "commands",
                "efforts",
                "list",
                "models",
                "resume",
                "send",
                "set-default-mode",
                "set-effort",
                "set-free-models",
                "set-mode",
                "set-model",
                "status",
                "stop",
                "watch",
            ]
            .map(str::to_string)
        )
    );
    assert_eq!(
        doc_replies,
        BTreeSet::from(
            [
                "activity",
                "commands",
                "default-mode",
                "archived",
                "deleted",
                "effort-set",
                "efforts",
                "error",
                "free-models",
                "list",
                "mode-set",
                "model-set",
                "models",
                "ok",
                "resumed",
                "sent",
                "status",
                "steered",
            ]
            .map(str::to_string)
        )
    );
    assert_eq!(
        doc_events,
        BTreeSet::from(
            [
                "archived",
                "deleted",
                "threads",
                "act",
                "activity",
                "commands",
                "done",
                "failed",
                "free-models",
                "interrupted",
                "message",
                "models",
                "note",
                "request",
                "steered",
                "stopped",
                "subagent",
                "subagent-act",
                "subagent-text",
                "text",
                "thread-updated"
            ]
            .map(str::to_string)
        )
    );

    // Exercise Lever against the stub harness and verify it produces every
    // documented reply and event.
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "models": [
                { "id": "stub/stub-1", "name": "Stub One", "efforts": ["low", "high"] },
                { "id": "stub/stub-2", "name": "Stub Two" }
            ],
            "default_model": "stub/stub-1",
            "modes": true,
            "steering": true,
            "commands": [{ "name": "review", "description": "review changes", "hint": "commit, branch, or pr" }],
            "turns": [
                {
                    "match": "Ask",
                    "steps": [
                        { "kind": "ask", "id": "c9", "title": "echo hi", "raw_input": { "command": "echo hi" } },
                        { "kind": "text", "text": "Asked." }
                    ]
                },
                {
                    "steps": [
                        { "kind": "act", "id": "c1", "tool_kind": "execute", "title": "Run ls", "status": "completed", "content": "Cargo.toml" },
                        { "kind": "subagent_start", "id": "ses_readme_sub", "title": "Check workspace", "prompt": "Inspect files" },
                        { "kind": "subagent_act", "subagent": "ses_readme_sub", "id": "c2", "tool_kind": "read", "title": "Read README.md", "status": "completed" },
                        { "kind": "subagent_text", "subagent": "ses_readme_sub", "text": "Found two files." },
                        { "kind": "subagent_end", "subagent": "ses_readme_sub", "status": "completed" },
                        { "kind": "unknown", "update": { "sessionUpdate": "extra", "summary": "Cache warm" } },
                        { "kind": "title", "title": "Inspect workspace" },
                        { "kind": "text", "text": "Here are the files." }
                    ]
                },
                { "steps": [{ "kind": "text", "text": "Wait" }], "stall": true },
                { "error": { "code": -32000, "message": "boom" } },
                { "steps": [{ "kind": "text", "text": "Crash" }], "crash": true }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let mut seen_replies: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    let mut seen_events: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    let record = |v: Value,
                  replies: &mut std::collections::BTreeMap<String, Value>,
                  events: &mut std::collections::BTreeMap<String, Value>|
     -> Value {
        // The first of each is kept. Of `thread-updated` and `commands`, it
        // is the first that says what the README's says: the title, and a
        // thread's commands.
        if let Some(r) = v.get("reply").and_then(Value::as_str) {
            replies.entry(r.to_string()).or_insert_with(|| v.clone());
        }
        if let Some(e) = v.get("event").and_then(Value::as_str) {
            let as_documented = match e {
                "thread-updated" => v.get("title").is_some(),
                "commands" => v.get("thread").is_some(),
                _ => true,
            };
            if as_documented {
                events.entry(e.to_string()).or_insert_with(|| v.clone());
            }
        }
        v
    };

    // status + error + set-free-models + models
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(&mut peer, &json!({ "op": "unsupported-op" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(&mut peer, &json!({ "op": "set-free-models", "enabled": true })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(&mut peer, &json!({ "op": "models" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    // efforts + commands, asked until the harness has named its commands, so
    // none comes as an event of its own once this peer watches.
    write_frame(&mut peer, &json!({ "op": "efforts", "model": "stub/stub-1" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    wait_until("the harness to name its commands", || {
        write_frame(&mut peer, &json!({ "op": "commands" })).unwrap();
        read_frame(&mut peer).unwrap()["known"] == true
    });
    write_frame(&mut peer, &json!({ "op": "commands" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(&mut peer, &json!({ "op": "watch" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(&mut peer, &json!({ "op": "set-free-models", "enabled": false })).unwrap();
    while !seen_events.contains_key("free-models") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    write_frame(&mut peer, &json!({ "op": "models", "refresh": true })).unwrap();
    while !seen_events.contains_key("models") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }

    // Turn 1: send -> message, commands, act, note, thread-updated, text, done
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "List the files", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent = loop {
        let frame = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
        if frame["reply"] == "sent" {
            break frame;
        }
    };
    let thread_id = sent["thread"].as_str().unwrap().to_string();
    loop {
        let ev = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
        if ev["event"] == "done" {
            break;
        }
    }

    // A turn that asks: request, answer -> ok, done
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Ask first" }),
    )
    .unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    let request_id = loop {
        let ev = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
        if ev["event"] == "request" {
            break ev["request"].as_str().unwrap().to_string();
        }
    };
    write_frame(
        &mut peer,
        &json!({ "op": "answer", "thread": thread_id, "request": request_id, "answer": "once" }),
    )
    .unwrap();
    loop {
        let ev = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
        if ev["event"] == "done" {
            break;
        }
    }

    // set-effort + set-model + set-mode + set-default-mode + list + resume
    write_frame(
        &mut peer,
        &json!({ "op": "set-effort", "thread": thread_id, "effort": "high" }),
    )
    .unwrap();
    while !seen_replies.contains_key("effort-set") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    write_frame(
        &mut peer,
        &json!({ "op": "set-model", "thread": thread_id, "model": "stub/stub-2" }),
    )
    .unwrap();
    while !seen_replies.contains_key("model-set") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    write_frame(
        &mut peer,
        &json!({ "op": "set-mode", "thread": thread_id, "mode": "full-access" }),
    )
    .unwrap();
    while !seen_replies.contains_key("mode-set") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    write_frame(&mut peer, &json!({ "op": "set-default-mode", "mode": "ask-first" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    write_frame(&mut peer, &json!({ "op": "list" })).unwrap();
    while !seen_replies.contains_key("list") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    while !seen_replies.contains_key("resumed") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }

    // Turn 3: stop -> ok + stopped
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Stall" }),
    )
    .unwrap();
    loop {
        let ev = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
        if ev["event"] == "text" {
            break;
        }
    }
    // A message sent while it works is steered into it: steered + steered.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Also check the docs" }),
    )
    .unwrap();
    while !seen_replies.contains_key("steered") || !seen_events.contains_key("steered") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread_id, "turn": 3 })).unwrap();
    while !seen_replies.contains_key("ok") || !seen_events.contains_key("stopped") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }

    // Turn 3: failed
    write_frame(&mut peer, &json!({ "op": "send", "thread": thread_id, "text": "Fail" })).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    while !seen_events.contains_key("failed") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }

    // Turn 4: interrupted
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Crash" }),
    )
    .unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    while !seen_events.contains_key("interrupted") {
        record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    }

    write_frame(&mut peer, &json!({"op":"list","watch":true})).unwrap();
    record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
    for (op, reply) in [("archive", "archived"), ("delete", "deleted")] {
        write_frame(&mut peer, &json!({"op":op,"thread":thread_id})).unwrap();
        loop {
            let v = record(read_frame(&mut peer).unwrap(), &mut seen_replies, &mut seen_events);
            if v["event"] == "threads" && seen_replies.contains_key(reply) && seen_events.contains_key(reply) {
                break;
            }
        }
    }

    let seen_reply_names: BTreeSet<String> = seen_replies.keys().cloned().collect();
    let seen_event_names: BTreeSet<String> = seen_events.keys().cloned().collect();
    assert_eq!(seen_reply_names, doc_replies);
    assert_eq!(seen_event_names, doc_events);

    // Every documented reply and event block's top-level keys must match what Lever emits.
    for block in &blocks {
        if let Some(name) = block.get("reply").and_then(Value::as_str) {
            let actual = &seen_replies[name];
            let doc_keys: BTreeSet<&str> = block.as_object().unwrap().keys().map(String::as_str).collect();
            let actual_keys: BTreeSet<&str> = actual
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .filter(|k| !(matches!(*k, "model" | "effort") && name == "resumed"))
                .collect();
            assert_eq!(actual_keys, doc_keys, "reply {name:?} keys drifted from README");
        }
        if let Some(name) = block.get("event").and_then(Value::as_str) {
            let actual = &seen_events[name];
            let doc_keys: BTreeSet<&str> = block.as_object().unwrap().keys().map(String::as_str).collect();
            let actual_keys: BTreeSet<&str> = actual.as_object().unwrap().keys().map(String::as_str).collect();
            assert_eq!(actual_keys, doc_keys, "event {name:?} keys drifted from README");
        }
    }

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn reused_tool_call_id_short_id_in_answer_and_error_on_cancel_are_handled_correctly() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "turns": [
                {
                    "steps": [
                        { "kind": "act", "id": "c1", "tool_kind": "execute", "title": "First act", "status": "completed", "content": "one" },
                        { "kind": "text", "text": "Use vec1 here." }
                    ]
                },
                {
                    "steps": [
                        { "kind": "act", "id": "c1", "tool_kind": "read", "title": "Second act", "status": "in_progress" },
                        { "kind": "act_update", "id": "c1", "status": "completed", "content": "two" },
                        { "kind": "text", "text": "Turn two." }
                    ]
                },
                {
                    "steps": [{ "kind": "text", "text": "Cancel me" }],
                    "stall": true,
                    "cancel_with_error": true
                }
            ]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);

    // Turn 1: short tool ID "c1" must not corrupt "vec1" in the answer.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "One", "cwd": dir.0.to_str().unwrap() }),
    )
    .unwrap();
    let sent1 = read_frame(&mut peer).unwrap();
    let thread_id = sent1["thread"].as_str().unwrap().to_string();
    let mut t1_text = String::new();
    let mut t1_act_id = String::new();
    loop {
        let ev = read_frame(&mut peer).unwrap();
        if ev["event"] == "text" {
            t1_text.push_str(ev["text"].as_str().unwrap());
        } else if ev["event"] == "act" {
            t1_act_id = ev["act"].as_str().unwrap().to_string();
        } else if ev["event"] == "done" {
            break;
        }
    }
    assert_eq!(t1_text, "Use vec1 here.");

    // Turn 2: reusing harness tool call ID "c1" creates a new act on turn 2 without overwriting turn 1's act.
    write_frame(&mut peer, &json!({ "op": "send", "thread": thread_id, "text": "Two" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["turn"], 2);
    let mut t2_act_id = String::new();
    loop {
        let ev = read_frame(&mut peer).unwrap();
        if ev["event"] == "act" {
            assert_eq!(ev["turn"], 2, "turn 2 act must broadcast under turn 2: {ev}");
            t2_act_id = ev["act"].as_str().unwrap().to_string();
        } else if ev["event"] == "done" {
            break;
        }
    }
    assert_ne!(t1_act_id, t2_act_id, "turn 2 must get its own Lever act ID");

    // Turn 3: harness answering `session/cancel` with a JSON-RPC error still finishes as `stopped`.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread_id, "text": "Three" }),
    )
    .unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["turn"], 3);
    loop {
        if read_frame(&mut peer).unwrap()["event"] == "text" {
            break;
        }
    }
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread_id, "turn": 3 })).unwrap();
    let mut saw_stopped = false;
    while !saw_stopped {
        let frame = read_frame(&mut peer).unwrap();
        assert_ne!(
            frame["event"], "failed",
            "cancelled turn must not end as failed: {frame}"
        );
        if frame["event"] == "stopped" && frame["turn"] == 3 {
            saw_stopped = true;
        }
    }

    // Verify all three turns and both acts are intact in the resumed record.
    write_frame(&mut peer, &json!({ "op": "resume", "thread": thread_id })).unwrap();
    let resumed = read_frame(&mut peer).unwrap();
    assert_eq!(resumed["turns"][0]["acts"][0]["title"], "First act");
    assert_eq!(resumed["turns"][0]["acts"][0]["detail"], "one");
    assert_eq!(resumed["turns"][1]["acts"][0]["title"], "Second act");
    assert_eq!(resumed["turns"][1]["acts"][0]["detail"], "two");
    assert_eq!(resumed["turns"][2]["state"], "stopped");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

fn write_script(dir: &Path, script: &Value) -> PathBuf {
    let path = dir.join("script.json");
    std::fs::write(&path, serde_json::to_string(script).unwrap()).unwrap();
    path
}

/// Reads frames until one is what is looked for, and returns it.
fn read_until(peer: &mut UnixStream, mut found: impl FnMut(&Value) -> bool) -> Value {
    loop {
        let frame = read_frame(peer).unwrap();
        if found(&frame) {
            return frame;
        }
    }
}

/// Sends a message, to a thread or to begin one, and returns the thread and the turn.
fn send(peer: &mut UnixStream, thread: Option<&str>, text: &str) -> (String, u64) {
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
fn turn_events(peer: &mut UnixStream, thread: &str, turn: u64) -> Vec<Value> {
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

fn text_of(events: &[Value]) -> String {
    events
        .iter()
        .filter(|event| event["event"] == "text")
        .filter_map(|event| event["text"].as_str())
        .collect()
}

fn resume(peer: &mut UnixStream, thread: &str) -> Value {
    write_frame(peer, &json!({ "op": "resume", "thread": thread })).unwrap();
    read_until(peer, |frame| frame.get("reply").is_some())
}

fn wait_until(what: &str, mut met: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !met() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Has the harness end at once, on a thread of its own, so the next turn
/// starts a new harness process.
fn end_the_harness(peer: &mut UnixStream) {
    let (thread, turn) = send(peer, None, "restart the harness");
    let events = turn_events(peer, &thread, turn);
    assert_eq!(events.last().unwrap()["event"], "interrupted");
}

#[test]
fn a_session_id_the_harness_gives_twice_is_one_threads_alone() {
    let dir = TempDir::new();
    // A prefix of the script's own has every stub process count its sessions
    // from one, so a harness started again gives out IDs it gave before.
    let script = write_script(
        &dir.0,
        &json!({
            "session_prefix": "acp-twice-",
            "turns": [
                { "match": "restart the harness", "crash": true },
                { "match": "A one", "steps": [{ "kind": "text", "text": "A1." }] },
                { "match": "A two", "steps": [{ "kind": "text", "text": "A2." }] },
                { "match": "A three", "steps": [{ "kind": "text", "text": "A3." }] },
                { "match": "B one", "steps": [{ "kind": "text", "text": "B1." }] },
                { "match": "B two", "steps": [{ "kind": "text", "text": "B2." }] },
                { "match": "C one", "steps": [{ "kind": "text", "text": "C1." }] },
                { "match": "D one", "steps": [{ "kind": "text", "text": "D1." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);

    // The first harness gives thread A session 1.
    let (a, turn) = send(&mut peer, None, "A one");
    assert_eq!(text_of(&turn_events(&mut peer, &a, turn)), "A1.");
    end_the_harness(&mut peer);

    // The second gives session 1 to thread B. Thread A cannot load it now:
    // it is B's. A gets a session of its own, and a note that says so.
    let (b, turn) = send(&mut peer, None, "B one");
    assert_eq!(text_of(&turn_events(&mut peer, &b, turn)), "B1.");
    let (_, turn) = send(&mut peer, Some(&a), "A two");
    let events = turn_events(&mut peer, &a, turn);
    assert_eq!(text_of(&events), "A2.", "{events:?}");
    let note = events.iter().find(|event| event["event"] == "note").expect("a note");
    assert!(
        note["text"].as_str().unwrap().contains("began a new one"),
        "the thread must say its harness began afresh: {note}"
    );

    // Each thread's next answer is its own.
    let (_, turn) = send(&mut peer, Some(&b), "B two");
    assert_eq!(text_of(&turn_events(&mut peer, &b, turn)), "B2.");
    let record_a = resume(&mut peer, &a);
    let record_b = resume(&mut peer, &b);
    assert_eq!(record_a["turns"].as_array().unwrap().len(), 2, "{record_a}");
    assert_eq!(record_a["turns"][1]["answer"], "A2.", "{record_a}");
    assert_eq!(record_a["turns"][1]["notes"].as_array().unwrap().len(), 1, "{record_a}");
    assert_eq!(record_b["turns"].as_array().unwrap().len(), 2, "{record_b}");
    assert_eq!(record_b["turns"][1]["answer"], "B2.", "{record_b}");
    assert_eq!(record_b["turns"][1]["notes"], json!([]), "{record_b}");
    end_the_harness(&mut peer);

    // The third loads A's own session, 2, and A goes on with no note. Then a
    // new thread is given 1, which is free, and the next is given 2, which is
    // A's: that thread's turn fails, and A keeps its session.
    let (_, turn) = send(&mut peer, Some(&a), "A three");
    let events = turn_events(&mut peer, &a, turn);
    assert_eq!(text_of(&events), "A3.", "{events:?}");
    assert!(events.iter().all(|event| event["event"] != "note"), "{events:?}");
    let (c, turn) = send(&mut peer, None, "C one");
    assert_eq!(text_of(&turn_events(&mut peer, &c, turn)), "C1.");
    let (d, turn) = send(&mut peer, None, "D one");
    let events = turn_events(&mut peer, &d, turn);
    let end = events.last().unwrap();
    assert_eq!(end["event"], "failed", "{events:?}");
    assert!(
        end["message"].as_str().unwrap().contains("another thread is using"),
        "{end}"
    );
    let record_a = resume(&mut peer, &a);
    assert_eq!(record_a["turns"].as_array().unwrap().len(), 3, "{record_a}");
    assert_eq!(record_a["turns"][2]["answer"], "A3.", "{record_a}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_thread_whose_session_cannot_be_loaded_says_its_harness_began_afresh() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({
            "load_session": false,
            "turns": [
                { "match": "restart the harness", "crash": true },
                { "match": "One", "steps": [{ "kind": "text", "text": "First." }] },
                { "match": "Two", "steps": [{ "kind": "text", "text": "Second." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);

    let (thread, turn) = send(&mut peer, None, "One");
    let events = turn_events(&mut peer, &thread, turn);
    assert!(events.iter().all(|event| event["event"] != "note"), "{events:?}");
    end_the_harness(&mut peer);

    let (_, turn) = send(&mut peer, Some(&thread), "Two");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(text_of(&events), "Second.");
    let said = "The harness could not pick this thread's session up again. \
                It began a new one, which has none of the turns before.";
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "note")
            .map(|event| event["text"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [said],
        "{events:?}"
    );
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["notes"], json!([]), "{record}");
    assert_eq!(record["turns"][1]["notes"][0]["text"], said, "{record}");
    assert!(!std::fs::read_to_string(&rpc_log).unwrap().contains("session/load"));

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_that_fails_after_its_turn_was_stopped_fails_the_turn_that_waits_and_no_other() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({
            "new_session_delay_ms": 400,
            "new_session_error": { "code": -32000, "message": "no sessions today" }
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);
    let session_requests = || {
        std::fs::read_to_string(&rpc_log)
            .unwrap_or_default()
            .matches("\"method\":\"session/new\"")
            .count()
    };

    // A turn is stopped while the harness still has its request for a
    // session, and nothing more is sent. When the harness says there is no
    // session, no turn waits for one: the turn stays stopped, and nobody is
    // told it failed.
    let (alone, _) = send(&mut peer, None, "Alone");
    wait_until("the harness to be asked for a session", || session_requests() == 1);
    write_frame(&mut peer, &json!({ "op": "stop", "thread": alone, "turn": 1 })).unwrap();
    read_until(&mut peer, |frame| frame["event"] == "stopped" && frame["turn"] == 1);
    std::thread::sleep(Duration::from_millis(700));
    write_frame(&mut peer, &json!({ "op": "resume", "thread": alone })).unwrap();
    let record = read_until(&mut peer, |frame| {
        assert_ne!(frame["event"], "failed", "a stopped turn was failed: {frame}");
        frame.get("reply").is_some()
    });
    assert_eq!(record["turns"][0]["state"], "stopped", "{record}");

    // Turn 1 of another thread is stopped in the same way.
    let (thread, _) = send(&mut peer, None, "One");
    wait_until("the harness to be asked for a session", || session_requests() == 2);
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": 1 })).unwrap();
    read_until(&mut peer, |frame| frame["event"] == "stopped" && frame["turn"] == 1);

    // Turn 2 waits for that same session. When the harness says there is
    // none, turn 2 fails, and turn 1 stays as the person left it.
    let (_, turn) = send(&mut peer, Some(&thread), "Two");
    assert_eq!(turn, 2);
    let failed = read_until(&mut peer, |frame| frame["event"] == "failed");
    assert_eq!(failed["turn"], 2, "{failed}");
    assert_eq!(failed["message"], "no sessions today");
    assert_eq!(session_requests(), 2, "one thread asks for one session at a time");
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["state"], "stopped", "{record}");
    assert_eq!(record["turns"][1]["state"], "failed", "{record}");
    assert_eq!(record["working"], false, "{record}");

    // The thread takes another message: no turn of it was left working.
    let (_, turn) = send(&mut peer, Some(&thread), "Three");
    assert_eq!(turn_events(&mut peer, &thread, turn).last().unwrap()["event"], "failed");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_peer_that_reads_none_of_a_threads_events_is_closed_and_the_thread_goes_on() {
    const PIECE: usize = 64 * 1024;
    const PIECES: usize = 96;
    let dir = TempDir::new();
    let mut steps = vec![json!({ "kind": "pause_ms", "ms": 300 })];
    steps.extend((0..PIECES).map(|_| json!({ "kind": "text", "text": "x".repeat(PIECE) })));
    let script = write_script(&dir.0, &json!({ "turns": [{ "steps": steps }] }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);

    // One peer begins the turn and reads nothing more. Another watches the
    // same thread and reads all of it.
    let mut stalled = handshake(&lever.person);
    let (thread, turn) = send(&mut stalled, None, "Say a great deal");
    let mut reader = handshake(&lever.person);
    assert_eq!(resume(&mut reader, &thread)["reply"], "resumed");
    let events = turn_events(&mut reader, &thread, turn);
    assert_eq!(events.last().unwrap()["event"], "done");
    assert_eq!(text_of(&events).len(), PIECE * PIECES);

    // Lever did not keep the whole answer in memory for the peer that read
    // nothing: it closed it, and said why.
    wait_until("the stalled peer to be closed", || {
        lever
            .log_text()
            .contains("connection: closed, the person left more than 4 MiB of events unread")
    });
    let mut got = 0;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match stalled.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => got += n,
            Err(err) if err.kind() == io::ErrorKind::ConnectionReset => break,
            Err(err) => panic!("Lever kept a peer that read nothing, after {got} bytes: {err}"),
        }
    }
    assert!(
        got < PIECE * PIECES,
        "the stalled peer was sent everything: {got} bytes"
    );

    // The record has what that peer missed, for when it comes back.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    let (state, saved): (String, i64) = db
        .query_row("SELECT state, length(answer) FROM turns", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!((state.as_str(), saved as usize), ("done", PIECE * PIECES));
    drop(db);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_the_database_did_not_take_fails_and_its_end_is_saved_when_the_database_takes_it() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                { "steps": [
                    { "kind": "text", "text": "Kept. " },
                    { "kind": "pause_ms", "ms": 600 },
                    { "kind": "text", "text": "Lost." }
                ] },
                { "steps": [{ "kind": "text", "text": "Again." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Say two things");
    read_until(&mut peer, |frame| frame["event"] == "text");

    // The database stops taking writes to its turns partway through the answer.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch("ALTER TABLE turns RENAME TO turns_away").unwrap();

    // The rest of the answer still reaches who is watching, and the turn
    // ends as failed, because the record does not have all of it.
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(text_of(&events), "Lost.", "{events:?}");
    let end = events.last().unwrap();
    assert_eq!(end["event"], "failed", "{events:?}");
    assert_eq!(end["message"], "Lever could not save all of this turn.");
    wait_until("the storage failure to be logged", || {
        lever.log_text().contains(" error: storage: ")
    });

    // When the database takes writes again, Lever saves how the turn ended,
    // and the thread is not left working.
    db.execute_batch("ALTER TABLE turns_away RENAME TO turns").unwrap();
    wait_until("the turn's end to be saved", || {
        resume(&mut peer, &thread)["turns"][0]["state"] == "failed"
    });
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["answer"], "Kept. ", "{record}");
    assert_eq!(record["turns"][0]["error"], "Lever could not save all of this turn.");
    assert_eq!(record["working"], false, "{record}");
    let (_, turn) = send(&mut peer, Some(&thread), "Say it again");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(events.last().unwrap()["event"], "done", "{events:?}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    let log = lever.log_text();
    assert!(
        !log.contains("Kept.") && !log.contains("Lost."),
        "the answer is in the log: {log}"
    );
}

#[test]
fn a_turn_working_when_lever_is_told_to_stop_is_interrupted_in_the_record_it_leaves() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({ "turns": [{ "steps": [{ "kind": "text", "text": "Waiting" }], "stall": true }] }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    send(&mut peer, None, "Wait for me");
    read_until(&mut peer, |frame| frame["event"] == "text");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    // Read with no Lever running: the database it closed says how the turn ended.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    let (state, ended): (String, Option<i64>) = db
        .query_row("SELECT state, ended_at FROM turns", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(state, "interrupted");
    assert!(ended.is_some());
}

#[test]
fn a_harness_failure_is_a_failure_though_the_person_had_asked_to_stop() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [{ "kind": "text", "text": "Working" }],
                "stall": true,
                "error": { "code": -32000, "message": "The provider went away." }
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Work");
    read_until(&mut peer, |frame| frame["event"] == "text");

    // The harness answers the stop with an error that is not a cancellation.
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": turn })).unwrap();
    let end = turn_events(&mut peer, &thread, turn).pop().unwrap();
    assert_eq!(end["event"], "failed", "{end}");
    assert_eq!(end["message"], "The provider went away.");
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["state"], "failed", "{record}");
    assert_eq!(record["turns"][0]["error"], "The provider went away.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_result_with_a_null_error_is_a_result_and_an_update_with_a_null_id_is_an_update() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({ "explicit_nulls": true, "turns": [{ "steps": [{ "kind": "text", "text": "Fine." }] }] }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Hello");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(events.last().unwrap()["event"], "done", "{events:?}");
    assert_eq!(text_of(&events), "Fine.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_id_the_record_has_for_two_threads_is_loaded_for_neither() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({
            "session_prefix": "acp-twice-",
            "turns": [
                { "match": "restart the harness", "crash": true },
                { "match": "A one", "steps": [{ "kind": "text", "text": "A1." }] },
                { "match": "A two", "steps": [{ "kind": "text", "text": "A2." }] },
                { "match": "B one", "steps": [{ "kind": "text", "text": "B1." }] },
                { "match": "B two", "steps": [{ "kind": "text", "text": "B2." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);

    // One harness gives thread A session 1, and the next gives thread B the
    // same ID. A sends nothing while that harness runs, so nothing in memory
    // says the ID was A's once.
    let (a, turn) = send(&mut peer, None, "A one");
    assert_eq!(text_of(&turn_events(&mut peer, &a, turn)), "A1.");
    end_the_harness(&mut peer);
    let (b, turn) = send(&mut peer, None, "B one");
    assert_eq!(text_of(&turn_events(&mut peer, &b, turn)), "B1.");
    end_the_harness(&mut peer);

    // What a harness holds under that ID now is B's, if it is anyone's. A
    // does not load it: A is given a new session, and a note that says so.
    // That new session has the ID again, so B has lost it in turn.
    for (thread, message, answer) in [(&a, "A two", "A2."), (&b, "B two", "B2.")] {
        let (_, turn) = send(&mut peer, Some(thread), message);
        let events = turn_events(&mut peer, thread, turn);
        assert_eq!(text_of(&events), answer, "{events:?}");
        assert_eq!(
            events.iter().filter(|event| event["event"] == "note").count(),
            1,
            "the thread must say its harness began afresh: {events:?}"
        );
    }
    let rpc_text = std::fs::read_to_string(&rpc_log).unwrap();
    assert!(
        !rpc_text.contains("session/load"),
        "a shared session was loaded: {rpc_text}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_session_id_one_thread_is_loading_is_not_given_to_another() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({
            "session_prefix": "acp-twice-",
            "load_delay_ms": 500,
            "turns": [
                { "match": "restart the harness", "crash": true },
                { "match": "A one", "steps": [{ "kind": "text", "text": "A1." }] },
                { "match": "A two", "steps": [{ "kind": "text", "text": "A2." }] },
                { "match": "B one", "steps": [{ "kind": "text", "text": "B1." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);
    let (a, turn) = send(&mut peer, None, "A one");
    assert_eq!(text_of(&turn_events(&mut peer, &a, turn)), "A1.");
    end_the_harness(&mut peer);

    // The new harness is slow to load A's session, 1, and meanwhile gives
    // the same ID to a new thread. That thread's turn fails: the session is
    // A's, and while it loads Lever takes none of its updates.
    let (_, turn_a) = send(&mut peer, Some(&a), "A two");
    wait_until("the harness to be asked to load A's session", || {
        std::fs::read_to_string(&rpc_log)
            .unwrap_or_default()
            .contains("session/load")
    });
    let (b, turn_b) = send(&mut peer, None, "B one");
    let mut ends = std::collections::HashMap::new();
    let mut a_text = String::new();
    while ends.len() < 2 {
        let frame = read_frame(&mut peer).unwrap();
        let thread = frame["thread"].as_str().unwrap_or_default().to_string();
        if frame["event"] == "text" && thread == a {
            a_text.push_str(frame["text"].as_str().unwrap());
        }
        if ["done", "failed", "stopped", "interrupted"]
            .iter()
            .any(|end| frame["event"] == *end)
        {
            ends.insert(thread, frame);
        }
    }
    assert_eq!(ends[&b]["event"], "failed", "{ends:?}");
    assert_eq!(ends[&b]["turn"], turn_b);
    assert!(
        ends[&b]["message"]
            .as_str()
            .unwrap()
            .contains("another thread is using"),
        "{ends:?}"
    );
    assert_eq!(ends[&a]["event"], "done", "{ends:?}");
    assert_eq!(ends[&a]["turn"], turn_a);
    assert_eq!(a_text, "A2.");
    let record = resume(&mut peer, &a);
    assert_eq!(
        record["turns"][1]["notes"],
        json!([]),
        "A loaded its own session: {record}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

/// Has the database refuse a kind of write until the trigger is dropped.
fn refuse_writes(db: &rusqlite::Connection, what: &str) {
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch(&format!(
        "CREATE TRIGGER refused BEFORE {what} BEGIN SELECT RAISE(ABORT, 'refused by the test'); END"
    ))
    .unwrap();
}

#[test]
fn a_harness_that_ends_while_the_database_takes_no_writes_still_interrupts_its_turns() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "crash",
                    "steps": [{ "kind": "text", "text": "Half" }, { "kind": "pause_ms", "ms": 600 }],
                    "crash": true
                },
                { "steps": [{ "kind": "text", "text": "Back." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Now crash");
    read_until(&mut peer, |frame| frame["event"] == "text");

    // No turn's state can be written when the harness ends.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF state ON turns");
    let end = turn_events(&mut peer, &thread, turn).pop().unwrap();
    assert_eq!(end["event"], "interrupted", "who is watching is told all the same");

    // When the database takes writes again, the record gets the turn's end,
    // and the thread takes another message.
    db.execute_batch("DROP TRIGGER refused").unwrap();
    wait_until("the turn's end to be saved", || {
        resume(&mut peer, &thread)["turns"][0]["state"] == "interrupted"
    });
    let (_, turn) = send(&mut peer, Some(&thread), "Go on");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "Back.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_the_database_takes_only_part_of_begins_no_turn() {
    let dir = TempDir::new();
    let script = write_script(&dir.0, &json!({ "turns": [] }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "First");
    turn_events(&mut peer, &thread, turn);

    // The database takes a new turn and a new thread, and refuses the
    // timestamp that is written after them.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF updated_at ON threads");
    for request in [
        json!({ "op": "send", "thread": thread, "text": "Second" }),
        json!({ "op": "send", "text": "Another thread", "cwd": "/tmp" }),
    ] {
        write_frame(&mut peer, &request).unwrap();
        let reply = read_until(&mut peer, |frame| frame.get("reply").is_some());
        assert_eq!(reply["code"], "storage-error", "{reply}");
    }
    db.execute_batch("DROP TRIGGER refused").unwrap();

    // Neither left anything behind: no second thread, and no turn that is
    // working in the record with nothing working on it.
    write_frame(&mut peer, &json!({ "op": "list" })).unwrap();
    let listed = read_until(&mut peer, |frame| frame.get("reply").is_some());
    assert_eq!(listed["threads"].as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed["threads"][0]["turns"], 1, "{listed}");
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");
    let (_, turn) = send(&mut peer, Some(&thread), "Second, again");
    assert_eq!(turn, 2);
    assert_eq!(turn_events(&mut peer, &thread, turn).last().unwrap()["event"], "done");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    assert!(lever.log_text().contains(" error: storage: "), "{}", lever.log_text());
}

/// The memory a process holds, in KiB, as the kernel counts it.
fn resident_kib(pid: u32) -> usize {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kib| kib.parse().ok())
        .expect("VmRSS")
}

#[test]
fn many_requests_for_a_long_record_from_a_peer_that_reads_nothing_are_answered_as_it_reads() {
    const PIECE: usize = 64 * 1024;
    const PIECES: usize = 24;
    const REQUESTS: usize = 64;
    let dir = TempDir::new();
    let steps: Vec<Value> = (0..PIECES)
        .map(|_| json!({ "kind": "text", "text": "x".repeat(PIECE) }))
        .collect();
    let script = write_script(&dir.0, &json!({ "turns": [{ "steps": steps }] }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut first = handshake(&lever.person);
    let (thread, turn) = send(&mut first, None, "Say a great deal");
    assert_eq!(turn_events(&mut first, &thread, turn).last().unwrap()["event"], "done");
    drop(first);

    // A peer asks for the 1.5 MiB record many times over in one write, says
    // it will send no more, and reads nothing. The replies come to 96 MiB.
    let mut peer = handshake(&lever.person);
    let request = encode_frame(&json!({ "op": "resume", "thread": thread }));
    peer.write_all(&request.repeat(REQUESTS)).unwrap();
    peer.shutdown(std::net::Shutdown::Write).unwrap();
    std::thread::sleep(Duration::from_millis(500));

    // Lever has answered only as far as the mark, and holds the rest of the
    // requests, not their replies.
    let held = resident_kib(lever.pid());
    assert!(held < 40 * 1024, "Lever holds {held} KiB for a peer that reads nothing");

    // Every request is answered as the peer reads, and then Lever closes.
    for n in 0..REQUESTS {
        let reply = read_frame(&mut peer).unwrap_or_else(|err| panic!("reply {n} was lost: {err}"));
        assert_eq!(reply["reply"], "resumed", "reply {n}");
        assert_eq!(reply["turns"][0]["answer"].as_str().unwrap().len(), PIECE * PIECES);
    }
    assert_closed_without_reply(peer);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_that_writes_a_line_with_no_end_is_stopped_and_its_turn_interrupted() {
    const MAX_LINE: usize = 16 * 1024 * 1024;
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "Flood",
                    "steps": [
                        { "kind": "text", "text": "Before." },
                        { "kind": "flood", "bytes": MAX_LINE + 1024 * 1024 }
                    ],
                    "stall": true
                },
                {
                    // As long a line as Lever takes with no end, and then,
                    // once Lever has read that, an update that ends it.
                    "match": "To the brim",
                    "steps": [
                        { "kind": "flood", "bytes": MAX_LINE },
                        { "kind": "pause_ms", "ms": 400 },
                        { "kind": "text", "text": "After." }
                    ],
                    "stall": true
                }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let stopped_for_it = |lever: &RunningLever| {
        lever
            .log_text()
            .matches("was stopped: it wrote a line longer than 16 MiB")
            .count()
    };

    let (thread, turn) = send(&mut peer, None, "Flood");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(text_of(&events), "Before.");
    assert_eq!(events.last().unwrap()["event"], "interrupted", "{events:?}");
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], false);
    assert!(resident_kib(lever.pid()) < 60 * 1024, "Lever kept the line");
    assert_eq!(stopped_for_it(&lever), 1, "{}", lever.log_text());

    // A line that is too long is too long wherever its end falls in what
    // Lever reads.
    let (thread, turn) = send(&mut peer, None, "To the brim");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(events.last().unwrap()["event"], "interrupted", "{events:?}");
    assert_eq!(stopped_for_it(&lever), 2, "{}", lever.log_text());

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn one_event_longer_than_the_backlog_reaches_a_peer_that_reads() {
    const LONG: usize = 5 * 1024 * 1024;
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({ "turns": [{ "steps": [{ "kind": "text", "text": "x".repeat(LONG) }] }] }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Say it all at once");

    // The peer reads as fast as it is sent to. It is behind on nothing.
    let mut got = 0;
    loop {
        let frame = read_frame_up_to(&mut peer, 2 * LONG)
            .unwrap_or_else(|err| panic!("Lever closed a peer that was reading: {err}"));
        if frame["event"] == "text" {
            got += frame["text"].as_str().unwrap().len();
        }
        if frame["event"] == "done" {
            assert_eq!(
                (frame["thread"].as_str(), frame["turn"].as_u64()),
                (Some(&*thread), Some(turn))
            );
            break;
        }
    }
    assert_eq!(got, LONG);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    assert!(!lever.log_text().contains("events unread"), "{}", lever.log_text());
}

#[test]
fn a_session_the_record_did_not_take_is_not_used() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({
            "session_prefix": "acp-twice-",
            "turns": [
                { "match": "restart the harness", "crash": true },
                { "match": "A one", "steps": [{ "kind": "text", "text": "A1." }] },
                { "match": "A two", "steps": [{ "kind": "text", "text": "A2." }] },
                { "match": "B unsaved", "steps": [{ "kind": "text", "text": "Never." }] },
                { "match": "B saved", "steps": [{ "kind": "text", "text": "B1." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);
    let (a, turn) = send(&mut peer, None, "A one");
    assert_eq!(text_of(&turn_events(&mut peer, &a, turn)), "A1.");
    end_the_harness(&mut peer);

    // The next harness gives thread B the ID the record has for A, and the
    // database does not take the write that makes it B's. B's turn fails,
    // and its message never reaches the harness: with the ID still A's in
    // the record, A would load at a later start what B had said.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF harness_session_id ON threads");
    let (b, turn) = send(&mut peer, None, "B unsaved");
    let end = turn_events(&mut peer, &b, turn).pop().unwrap();
    assert_eq!(end["event"], "failed", "{end}");
    assert_eq!(end["message"], "Lever could not save this thread's session.");
    db.execute_batch("DROP TRIGGER refused").unwrap();
    assert!(!std::fs::read_to_string(&rpc_log).unwrap().contains("B unsaved"));

    // B's next turn is given a session the record takes, and A loads its own.
    let (_, turn) = send(&mut peer, Some(&b), "B saved");
    assert_eq!(text_of(&turn_events(&mut peer, &b, turn)), "B1.");
    end_the_harness(&mut peer);
    let (_, turn) = send(&mut peer, Some(&a), "A two");
    let events = turn_events(&mut peer, &a, turn);
    assert_eq!(text_of(&events), "A2.", "{events:?}");
    assert!(events.iter().all(|event| event["event"] != "note"), "{events:?}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_whose_end_waits_to_be_saved_has_ended_for_whoever_asks() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({ "turns": [{ "steps": [{ "kind": "text", "text": "Waiting" }], "stall": true }] }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Wait");
    read_until(&mut peer, |frame| frame["event"] == "text");

    // The turn is stopped while the database takes no turn's state.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF state ON turns");
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": turn })).unwrap();
    assert_eq!(turn_events(&mut peer, &thread, turn).pop().unwrap()["event"], "stopped");

    // A peer that comes now was not there for that event, and none will say
    // it again: the record it is given, and the list, say the turn stopped.
    let mut late = handshake(&lever.person);
    let record = resume(&mut late, &thread);
    assert_eq!(record["turns"][0]["state"], "stopped", "{record}");
    assert_eq!(record["working"], false, "{record}");
    write_frame(&mut late, &json!({ "op": "list" })).unwrap();
    let listed = read_frame(&mut late).unwrap();
    assert_eq!(listed["threads"][0]["state"], "stopped", "{listed}");
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");

    // The thread is not working, and its next message is not taken either
    // until the record has how the turn ended. Lever says which it is.
    write_frame(&mut late, &json!({ "op": "send", "thread": thread, "text": "Go on" })).unwrap();
    let refused = read_frame(&mut late).unwrap();
    assert_eq!(refused["code"], "storage-error", "{refused}");
    assert_eq!(
        refused["message"],
        "Lever is still saving how this thread's last turn ended."
    );

    // Once the database takes writes, the next message is taken at once,
    // with no wait for Lever to try the save again on its own.
    db.execute_batch("DROP TRIGGER refused").unwrap();
    let (_, turn) = send(&mut late, Some(&thread), "Go on");
    assert_eq!(turn, 2);
    assert_eq!(turn_events(&mut late, &thread, turn).pop().unwrap()["event"], "done");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_peer_that_sends_and_resumes_at_once_is_told_of_the_new_turn_once() {
    let dir = TempDir::new();
    let script = write_script(&dir.0, &json!({ "turns": [] }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "One");
    turn_events(&mut peer, &thread, turn);

    // A message and a request for the record, in one write.
    let mut both = encode_frame(&json!({ "op": "send", "thread": thread, "text": "Two" }));
    both.extend(encode_frame(&json!({ "op": "resume", "thread": thread })));
    peer.write_all(&both).unwrap();
    let mut frames = Vec::new();
    while !(frames.iter().any(|frame: &Value| frame["event"] == "done")
        && frames.iter().any(|frame| frame["reply"] == "resumed"))
    {
        frames.push(read_frame(&mut peer).unwrap());
    }

    // The new turn's message comes as an event before the record that has
    // it, and not again after.
    let at = |found: &dyn Fn(&Value) -> bool| frames.iter().position(found);
    let messages = frames.iter().filter(|frame| frame["event"] == "message").count();
    let message = at(&|frame| frame["event"] == "message").expect("the message");
    let record = at(&|frame| frame["reply"] == "resumed").expect("the record");
    assert_eq!(messages, 1, "{frames:?}");
    assert!(message < record, "the record came before the event it has: {frames:?}");
    assert_eq!(frames[record]["turns"][1]["message"], "Two", "{frames:?}");

    // So with the answer, however much of it the harness had given by then:
    // what the record has and what comes after it are the answer, once.
    let after: String = text_of(&frames[record..]);
    let in_record = frames[record]["turns"][1]["answer"].as_str().unwrap();
    assert_eq!(format!("{in_record}{after}"), "stub echo: Two", "{frames:?}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_a_harness_says_when_it_cannot_start_is_the_persons_and_not_the_logs() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({ "initialize_error": { "code": -32000, "message": "HARNESS_DIAGNOSTIC\n2026 fake: line" } }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Hello");
    let end = turn_events(&mut peer, &thread, turn).pop().unwrap();
    assert_eq!(end["event"], "failed", "{end}");
    assert_eq!(end["message"], "HARNESS_DIAGNOSTIC\n2026 fake: line");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    let log = lever.log_text();
    assert!(log.contains(" error: the harness could not be initialized"), "{log}");
    assert!(
        !log.contains("HARNESS_DIAGNOSTIC") && !log.contains("fake: line"),
        "{log}"
    );
}

#[test]
fn a_session_with_no_id_is_no_session() {
    let dir = TempDir::new();
    let script = write_script(&dir.0, &json!({ "session_id": "" }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Hello");
    let end = turn_events(&mut peer, &thread, turn).pop().unwrap();
    assert_eq!(end["event"], "failed", "{end}");
    assert_eq!(end["message"], "The harness could not start a thread.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_option_given_twice_is_refused() {
    let given_twice = Command::new(BIN)
        .args([
            "--state",
            "/nonexistent",
            "--surface-socket",
            "/nonexistent/s",
            "--shell-uid",
            "1",
        ])
        .args(["--person-socket", "/nonexistent/p", "--person-uid", "1"])
        .args(["--idle-timeout-ms", "1000", "--idle-timeout-ms", "2000"])
        .output()
        .unwrap();
    assert_eq!(given_twice.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&given_twice.stderr).starts_with("usage: crowbar-lever"));
}

#[test]
fn a_long_event_a_peer_has_read_gives_it_no_more_room_to_fall_behind() {
    const MIB: usize = 1024 * 1024;
    let dir = TempDir::new();
    let short = || json!({ "kind": "text", "text": "y".repeat(MIB) });
    let mut steps = vec![json!({ "kind": "text", "text": "x".repeat(6 * MIB) })];
    steps.extend((0..3).map(|_| short()));
    steps.push(json!({ "kind": "pause_ms", "ms": 1500 }));
    steps.extend((0..4).map(|_| short()));
    let script = write_script(&dir.0, &json!({ "turns": [{ "steps": steps }] }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let closed = |lever: &RunningLever| {
        lever
            .log_text()
            .contains("connection: closed, the person left more than 4 MiB of events unread")
    };

    // A peer lets a 6 MiB event and three of 1 MiB wait for it. That is
    // within 4 MiB over the longest of them, so it is kept.
    let mut peer = handshake(&lever.person);
    send(&mut peer, None, "Say a great deal");
    std::thread::sleep(Duration::from_millis(700));
    assert!(!closed(&lever), "{}", lever.log_text());

    // It reads the long event, and then nothing. Events were waiting behind
    // that one all along, so it never had none waiting.
    loop {
        let frame = read_frame_up_to(&mut peer, 8 * MIB).unwrap();
        if frame["event"] == "text" {
            assert_eq!(frame["text"].as_str().unwrap().len(), 6 * MIB);
            break;
        }
    }

    // Four more events of 1 MiB come. With the three it left, that is more
    // than 4 MiB over the longest still waiting, which is 1 MiB: the event
    // it has taken no longer counts for it.
    wait_until("the peer that stopped reading to be closed", || closed(&lever));

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_a_peer_sent_before_it_went_is_carried_out() {
    let dir = TempDir::new();
    let script = write_script(&dir.0, &json!({ "turns": [] }));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);

    // A peer states its version, sends two messages, and is gone, all
    // before Lever has read a byte of it.
    // The second is longer than Lever takes in one read, so part of it is
    // still to be read when Lever finds the peer gone.
    let second = format!("second {}", "x".repeat(12 * 1024));
    let mut all = encode_frame(&json!({ "version": 1 }));
    all.extend(encode_frame(&json!({ "op": "send", "text": "first", "cwd": "/tmp" })));
    all.extend(encode_frame(&json!({ "op": "send", "text": second, "cwd": "/tmp" })));
    let mut gone = connect(&lever.person);
    gone.write_all(&all).unwrap();
    drop(gone);

    // Both messages began their threads, and both were answered.
    let mut peer = handshake(&lever.person);
    wait_until("both threads to be done", || {
        write_frame(&mut peer, &json!({ "op": "list" })).unwrap();
        let listed = read_frame(&mut peer).unwrap();
        let threads = listed["threads"].as_array().unwrap();
        threads.len() == 2 && threads.iter().all(|thread| thread["state"] == "done")
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_started_again_has_the_ids_of_earlier_acts_taken_out_of_what_it_says() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                { "match": "restart the harness", "crash": true },
                {
                    "match": "Act",
                    "steps": [
                        { "kind": "act", "id": "acp-earlier-call-7", "title": "Look", "status": "completed" },
                        { "kind": "text", "text": "Looked." }
                    ]
                },
                { "match": "Fail", "error": { "code": -32000, "message": "Could not go on from acp-earlier-call-7." } }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Act");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "Looked.");
    end_the_harness(&mut peer);

    // The new harness has shown Lever no act, and names the old one.
    let (_, turn) = send(&mut peer, Some(&thread), "Fail");
    let end = turn_events(&mut peer, &thread, turn).pop().unwrap();
    assert_eq!(end["event"], "failed", "{end}");
    assert_eq!(end["message"], "Could not go on from [redacted].");
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][1]["error"], "Could not go on from [redacted].");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn harness_env_disables_autoupdate_and_free_models_until_chosen_and_selects_models() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "no_models_when_free_disabled": true,
            "models": [
                { "id": "stub/stub-1", "name": "Stub One" },
                { "id": "stub/stub-2", "name": "Stub Two" }
            ],
            "default_model": "stub/stub-1",
            "turns": [
                { "steps": [{ "kind": "text", "text": "first on {model}" }] },
                { "steps": [{ "kind": "text", "text": "second on {model}" }] },
                { "steps": [{ "kind": "text", "text": "restarted on {model}" }] }
            ]
        }),
    );
    let env_out = dir.0.join("env.json");
    let stub_bin = env!("CARGO_BIN_EXE_crowbar-stub-harness");
    let extra = vec![
        "--harness".into(),
        stub_bin.into(),
        "--harness-name".into(),
        "OpenCode".into(),
        "--harness-arg".into(),
        "--script".into(),
        "--harness-arg".into(),
        script.to_str().unwrap().into(),
        "--harness-arg".into(),
        "--env-out".into(),
        "--harness-arg".into(),
        env_out.to_str().unwrap().into(),
        "--harness-env".into(),
        "OPENCODE_DISABLE_MODELS_FETCH=1".into(),
    ];

    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);

    // Idle Lever starts no harness and has free_hosted_models = false.
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut peer).unwrap();
    assert_eq!(status["harness_started"], false);
    assert_eq!(status["free_hosted_models"], false);
    assert!(!env_out.exists(), "idle Lever must not start the harness");

    // Before the person chooses, models is empty and OPENCODE_DISABLE_FREE_MODELS=1 + update:disable are passed.
    write_frame(&mut peer, &json!({ "op": "models" })).unwrap();
    let models = read_frame(&mut peer).unwrap();
    assert_eq!(models["reply"], "models");
    assert_eq!(models["supported"], true);
    assert_eq!(models["models"], json!([]));
    assert_eq!(models["free_hosted_models"], false);

    let env1: Value = serde_json::from_str(&std::fs::read_to_string(&env_out).unwrap()).unwrap();
    assert_eq!(env1["OPENCODE_DISABLE_FREE_MODELS"], "1");
    assert_eq!(env1["OPENCODE_DISABLE_MODELS_FETCH"], "1");
    let cfg1: Value = serde_json::from_str(env1["OPENCODE_CONFIG_CONTENT"].as_str().unwrap()).unwrap();
    assert_eq!(cfg1["update"], "disable");

    // Sending a message with no Models available is refused with `no-model` and the reason.
    write_frame(&mut peer, &json!({ "op": "send", "text": "Hello" })).unwrap();
    let refused = read_frame(&mut peer).unwrap();
    assert_eq!(refused["reply"], "error");
    assert_eq!(refused["code"], "no-model");
    assert!(refused["message"].as_str().unwrap().contains("No Model is available"));

    // Enabling free hosted Models stops the idle harness and restarts without OPENCODE_DISABLE_FREE_MODELS.
    write_frame(&mut peer, &json!({ "op": "set-free-models", "enabled": true })).unwrap();
    let toggled = read_frame(&mut peer).unwrap();
    assert_eq!(
        toggled,
        json!({ "reply": "free-models", "enabled": true, "models_chosen": true })
    );

    write_frame(&mut peer, &json!({ "op": "models" })).unwrap();
    let models = read_frame(&mut peer).unwrap();
    assert_eq!(models["free_hosted_models"], true);
    assert_eq!(models["default"], "stub/stub-1");
    assert_eq!(models["models"].as_array().unwrap().len(), 2);

    let env2: Value = serde_json::from_str(&std::fs::read_to_string(&env_out).unwrap()).unwrap();
    assert!(env2["OPENCODE_DISABLE_FREE_MODELS"].is_null());
    let cfg2: Value = serde_json::from_str(env2["OPENCODE_CONFIG_CONTENT"].as_str().unwrap()).unwrap();
    assert_eq!(cfg2["update"], "disable");

    // Turn 1 uses default model stub/stub-1.
    let (tid, turn1) = send(&mut peer, None, "one");
    let ev1 = turn_events(&mut peer, &tid, turn1);
    assert_eq!(text_of(&ev1), "first on stub/stub-1");

    // Invalid Model is rejected cleanly.
    write_frame(
        &mut peer,
        &json!({ "op": "set-model", "thread": tid, "model": "stub/missing" }),
    )
    .unwrap();
    let bad = read_frame(&mut peer).unwrap();
    assert_eq!(bad["reply"], "error");
    assert_eq!(bad["code"], "invalid-model");

    // Switch thread to stub/stub-2 and run turn 2.
    write_frame(
        &mut peer,
        &json!({ "op": "set-model", "thread": tid, "model": "stub/stub-2" }),
    )
    .unwrap();
    let set_ok = read_frame(&mut peer).unwrap();
    assert_eq!(
        set_ok,
        json!({ "reply": "model-set", "thread": tid, "model": "stub/stub-2" })
    );
    let (_, turn2) = send(&mut peer, Some(&tid), "two");
    let ev2 = turn_events(&mut peer, &tid, turn2);
    assert_eq!(text_of(&ev2), "second on stub/stub-2");

    // Restart Lever: free_hosted_models and the thread's chosen Model persist and apply on turn 3.
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    drop(peer);

    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let rec = resume(&mut peer, &tid);
    assert_eq!(rec["model"], "stub/stub-2");
    let (_, turn3) = send(&mut peer, Some(&tid), "three");
    let ev3 = turn_events(&mut peer, &tid, turn3);
    assert_eq!(text_of(&ev3), "first on stub/stub-2");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn known_acp_updates_like_thoughts_and_available_commands_do_not_become_notes() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "emit_available_commands": true,
            "turns": [
                {
                    "steps": [
                        { "kind": "thought", "text": "thinking " },
                        { "kind": "thought", "text": "hard" },
                        { "kind": "available_commands" },
                        { "kind": "unknown", "update": { "sessionUpdate": "vendor_real_note", "summary": "Keep me" } },
                        { "kind": "text", "text": "Clean answer." }
                    ]
                }
            ]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (tid, turn) = send(&mut peer, None, "go");
    let evs = turn_events(&mut peer, &tid, turn);
    assert_eq!(text_of(&evs), "Clean answer.");

    let notes: Vec<&str> = evs
        .iter()
        .filter(|e| e["event"] == "note")
        .filter_map(|e| e["text"].as_str())
        .collect();
    assert_eq!(notes, vec!["Keep me"]);

    let rec = resume(&mut peer, &tid);
    let saved_notes: Vec<&str> = rec["turns"][0]["notes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["text"].as_str())
        .collect();
    assert_eq!(saved_notes, vec!["Keep me"]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn surface_socket_can_toggle_free_models_and_model_switch_preserves_default() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "models": [
                { "id": "stub/stub-1", "name": "Stub One" },
                { "id": "stub/stub-2", "name": "Stub Two" }
            ],
            "default_model": "stub/stub-1",
            "turns": [
                { "steps": [{ "kind": "text", "text": "on {model}" }] },
                { "steps": [{ "kind": "text", "text": "after {model}" }] }
            ]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);

    // Surface socket can change free hosted Models and broadcasts to watchers.
    let mut surface = handshake(&lever.surface);
    write_frame(&mut surface, &json!({ "op": "watch" })).unwrap();
    assert_eq!(read_frame(&mut surface).unwrap()["reply"], "activity");
    write_frame(&mut surface, &json!({ "op": "set-free-models", "enabled": true })).unwrap();
    assert_eq!(
        read_frame(&mut surface).unwrap(),
        json!({ "reply": "free-models", "enabled": true, "models_chosen": true })
    );
    assert_eq!(
        read_frame(&mut surface).unwrap(),
        json!({ "event": "free-models", "enabled": true, "models_chosen": true })
    );
    write_frame(&mut surface, &json!({ "op": "set-free-models", "enabled": false })).unwrap();
    assert_eq!(
        read_frame(&mut surface).unwrap(),
        json!({ "reply": "free-models", "enabled": false, "models_chosen": false })
    );
    assert_eq!(
        read_frame(&mut surface).unwrap(),
        json!({ "event": "free-models", "enabled": false, "models_chosen": false })
    );

    // Person socket can read models, switch one thread's Model, and `models` still reports stub/stub-1 as default.
    let mut person = handshake(&lever.person);
    write_frame(&mut person, &json!({ "op": "models" })).unwrap();
    let m1 = read_frame(&mut person).unwrap();
    assert_eq!(m1["default"], "stub/stub-1");

    let (tid, t1) = send(&mut person, None, "hello");
    let ev1 = turn_events(&mut person, &tid, t1);
    assert_eq!(text_of(&ev1), "on stub/stub-1");

    write_frame(
        &mut person,
        &json!({ "op": "set-model", "thread": tid, "model": "stub/stub-2" }),
    )
    .unwrap();
    let set_ok = read_frame(&mut person).unwrap();
    assert_eq!(set_ok["reply"], "model-set");
    // Who watches the thread is told its Model.
    assert_eq!(
        read_frame(&mut person).unwrap(),
        json!({ "event": "thread-updated", "thread": tid, "model": "stub/stub-2" })
    );

    write_frame(&mut person, &json!({ "op": "models" })).unwrap();
    let m2 = read_frame(&mut person).unwrap();
    assert_eq!(m2["default"], "stub/stub-1");

    // A rejected `--model` on a new thread is not persisted in SQLite.
    write_frame(
        &mut person,
        &json!({ "op": "send", "thread": null, "text": "bad model", "model": "stub/nonexistent" }),
    )
    .unwrap();
    let sent = read_frame(&mut person).unwrap();
    let bad_tid = sent["thread"].as_str().unwrap().to_string();
    let bad_evs = turn_events(&mut person, &bad_tid, 1);
    assert_eq!(bad_evs.last().unwrap()["event"], "failed");
    let rec = resume(&mut person, &bad_tid);
    assert!(rec.get("model").is_none(), "rejected model must not persist: {rec}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn free_models_off_while_working_refuses_new_sends_until_harness_restarts() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "no_models_when_free_disabled": true,
            "models": [{ "id": "stub/stub-1", "name": "Stub One" }],
            "default_model": "stub/stub-1",
            "turns": [
                { "stall": true, "steps": [{ "kind": "text", "text": "working..." }] }
            ]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut p1 = handshake(&lever.person);
    write_frame(&mut p1, &json!({ "op": "set-free-models", "enabled": true })).unwrap();
    assert_eq!(read_frame(&mut p1).unwrap()["reply"], "free-models");

    // Start a stalled turn on p1.
    let (tid, t1) = send(&mut p1, None, "hold");
    let ev = read_frame(&mut p1).unwrap();
    assert_eq!(ev["event"], "message");
    read_until(&mut p1, |ev| ev["event"] == "text");

    // Turn free hosted Models off from a second client while turn 1 is still working.
    let mut p2 = handshake(&lever.person);
    write_frame(&mut p2, &json!({ "op": "set-free-models", "enabled": false })).unwrap();
    assert_eq!(
        read_frame(&mut p2).unwrap(),
        json!({ "reply": "free-models", "enabled": false, "models_chosen": false })
    );

    // A new send and models query must be refused while the stale harness is still winding down.
    write_frame(
        &mut p2,
        &json!({ "op": "send", "thread": null, "text": "sneak through" }),
    )
    .unwrap();
    let refused = read_frame(&mut p2).unwrap();
    assert_eq!(refused["reply"], "error");
    assert_eq!(refused["code"], "working");

    write_frame(&mut p2, &json!({ "op": "models" })).unwrap();
    let refused_models = read_frame(&mut p2).unwrap();
    assert_eq!(refused_models["reply"], "error");
    assert_eq!(refused_models["code"], "working");

    // Stop turn 1; Lever stops the stale harness so the next check sees free hosted Models off.
    write_frame(&mut p1, &json!({ "op": "stop", "thread": tid, "turn": t1 })).unwrap();
    let _ = turn_events(&mut p1, &tid, t1);

    write_frame(&mut p2, &json!({ "op": "models" })).unwrap();
    let models = read_frame(&mut p2).unwrap();
    assert_eq!(models["free_hosted_models"], false);
    assert_eq!(models["models"], json!([]));

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn disconnecting_while_waiting_for_models_reply_does_not_stall_lever() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "new_session_delay_ms": 2000,
            "models": [{ "id": "stub/stub-1", "name": "Stub One" }],
            "default_model": "stub/stub-1",
            "turns": []
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);

    // Send `models` and close the socket well before the 2s probe finishes.
    {
        let mut departing = handshake(&lever.person);
        write_frame(&mut departing, &json!({ "op": "models" })).unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }

    // Lever must notice the hang-up and log it BEFORE the 2s probe finishes
    // (without the POLLHUP fix, Lever busy-spins and never logs disconnect until the probe completes).
    let log_path = lever.state.join("lever.log");
    let deadline = Instant::now() + Duration::from_millis(800);
    while Instant::now() < deadline {
        if std::fs::read_to_string(&log_path)
            .unwrap_or_default()
            .contains("the person left")
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let log_before_probe = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        log_before_probe.contains("the person left"),
        "expected disconnect to be logged before probe completed: {log_before_probe}"
    );

    // Another peer stays responsive.
    let mut p2 = handshake(&lever.person);
    write_frame(&mut p2, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut p2).unwrap();
    assert_eq!(status["reply"], "status");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn stopping_turn_during_model_switch_locks_thread_and_preserves_saved_model() {
    let dir = TempDir::new();
    let uid = my_uid();
    let rpc_log = dir.0.join("rpc.log");
    let script = write_script(
        &dir.0,
        &json!({
            "set_config_delay_ms": 400,
            "models": [
                { "id": "stub/stub-1", "name": "Stub One" },
                { "id": "stub/stub-2", "name": "Stub Two" }
            ],
            "default_model": "stub/stub-1",
            "turns": [
                { "steps": [{ "kind": "text", "text": "reply on {model}" }] }
            ]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);

    // Start a new thread requesting stub/stub-2 (which triggers a delayed set_config_option from default stub/stub-1).
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "switch and stop", "model": "stub/stub-2" }),
    )
    .unwrap();
    let sent = read_frame(&mut peer).unwrap();
    assert_eq!(sent["reply"], "sent");
    let tid = sent["thread"].as_str().unwrap().to_string();
    let ev_msg = read_frame(&mut peer).unwrap();
    assert_eq!(ev_msg["event"], "message");

    // Wait until the stub actually receives session/set_config_option, then stop turn 1.
    wait_until("the harness to receive session/set_config_option", || {
        std::fs::read_to_string(&rpc_log)
            .unwrap_or_default()
            .contains("session/set_config_option")
    });
    write_frame(&mut peer, &json!({ "op": "stop", "thread": tid, "turn": 1 })).unwrap();
    let stop_reply = read_frame(&mut peer).unwrap();
    assert_eq!(stop_reply["reply"], "ok");
    let ev_stopped = read_frame(&mut peer).unwrap();
    assert_eq!(ev_stopped["event"], "stopped");

    // While the stopped turn's set_config_option is still in flight, a new send on the same thread is refused with `working`.
    write_frame(&mut peer, &json!({ "op": "send", "thread": tid, "text": "too soon" })).unwrap();
    let busy = read_frame(&mut peer).unwrap();
    assert_eq!(busy["reply"], "error");
    assert_eq!(busy["code"], "working");

    // Once the delayed set_config_option settles, the stopped turn's Model was NOT saved to SQLite,
    // and the next turn without `--model` runs on the default Model (stub/stub-1), not stub/stub-2.
    std::thread::sleep(Duration::from_millis(550));
    let rec = resume(&mut peer, &tid);
    assert!(rec.get("model").is_none(), "stopped switch must not persist: {rec}");

    let (_, turn2) = send(&mut peer, Some(&tid), "after settle");
    let evs = turn_events(&mut peer, &tid, turn2);
    assert_eq!(text_of(&evs), "reply on stub/stub-1");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn surface_send_attaches_fulcrum_desktop_context_and_falls_back_when_fulcrum_is_away() {
    use std::os::unix::net::UnixListener;

    let dir = TempDir::new();
    let uid = my_uid();
    let rpc_log = dir.0.join("rpc.log");
    let fulcrum_sock = dir.0.join("fulcrum.sock");
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                { "match": "", "steps": [{ "kind": "text", "text": "seen" }] }
            ]
        }),
    );

    // Fake Fulcrum agent socket: accepts one connection, answers handshake + snapshot.
    let listener = UnixListener::bind(&fulcrum_sock).unwrap();
    let fulcrum_thread = std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().unwrap();
        let v = read_frame(&mut peer).unwrap();
        assert_eq!(v, json!({ "version": 1 }));
        write_frame(&mut peer, &json!({ "version": 1 })).unwrap();
        let req = read_frame(&mut peer).unwrap();
        assert_eq!(req, json!({ "op": "snapshot" }));
        write_frame(
            &mut peer,
            &json!({
                "reply": "snapshot",
                "workspace": 2,
                "active": "0x2",
                "locked": false,
                "windows": [
                    { "id": "0x1", "app_id": "com.crowbaros.Files", "title": "Home", "workspace": 1 },
                    { "id": "0x2", "app_id": "foot", "title": "terminal", "workspace": 2 }
                ],
                "displays": [
                    { "name": "Virtual-1", "width": 1280, "height": 960, "x": 0, "y": 0, "scale": 120 }
                ]
            }),
        )
        .unwrap();
    });

    let extra: Vec<String> = vec![
        "--fulcrum-socket".into(),
        fulcrum_sock.to_str().unwrap().into(),
        "--harness".into(),
        STUB_BIN.into(),
        "--harness-name".into(),
        "Stub".into(),
        "--harness-arg".into(),
        "--script".into(),
        "--harness-arg".into(),
        script.to_str().unwrap().into(),
        "--harness-arg".into(),
        "--log".into(),
        "--harness-arg".into(),
        rpc_log.to_str().unwrap().into(),
    ];
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);

    // 1. Surface send with Fulcrum running: carries desktop context in message event, resume, and ACP prompt.
    let mut surface = handshake(&lever.surface);
    write_frame(
        &mut surface,
        &json!({ "op": "send", "thread": null, "text": "what is open?" }),
    )
    .unwrap();
    let sent = read_frame(&mut surface).unwrap();
    assert_eq!(sent["reply"], "sent");
    let tid = sent["thread"].as_str().unwrap().to_string();
    let msg_ev = read_frame(&mut surface).unwrap();
    assert_eq!(msg_ev["event"], "message");
    assert_eq!(msg_ev["text"], "what is open?");
    assert_eq!(msg_ev["context"]["workspace"], 2);
    assert_eq!(msg_ev["context"]["active"]["app_id"], "foot");
    assert_eq!(msg_ev["context"]["active"]["title"], "terminal");
    assert_eq!(msg_ev["context"]["displays"][0]["name"], "Virtual-1");
    let evs = turn_events(&mut surface, &tid, 1);
    assert_eq!(text_of(&evs), "seen");
    fulcrum_thread.join().unwrap();

    // Persisted in SQLite and returned by resume.
    let rec = resume(&mut surface, &tid);
    assert_eq!(rec["turns"][0]["context"]["active"]["app_id"], "foot");
    assert_eq!(rec["turns"][0]["message"], "what is open?");

    // Harness received fenced desktop context as an assistant-audience block
    // separate from the person's text, so leading slash commands remain visible.
    let log_after_1 = std::fs::read_to_string(&rpc_log).unwrap();
    assert!(log_after_1.contains("<desktop-context>"), "{log_after_1}");
    assert!(log_after_1.contains("[Desktop context"), "{log_after_1}");
    assert!(log_after_1.contains("terminal (app_id: foot)"), "{log_after_1}");
    assert!(log_after_1.contains("Virtual-1: 1280x960"), "{log_after_1}");
    let prompt_req = log_after_1
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|v| v["method"] == "session/prompt")
        .expect("session/prompt logged");
    let blocks = prompt_req["params"]["prompt"].as_array().expect("prompt blocks");
    assert_eq!(blocks.len(), 2, "expected context + user text blocks: {prompt_req}");
    assert_eq!(blocks[0]["annotations"]["audience"], json!(["assistant"]), "{blocks:?}");
    assert!(
        blocks[0]["text"].as_str().unwrap_or("").contains("[Desktop context"),
        "{blocks:?}"
    );
    assert!(
        blocks[1].get("annotations").is_none(),
        "user prompt block must not carry assistant audience: {blocks:?}"
    );
    assert_eq!(blocks[1]["text"], "what is open?");

    // 2. Remove Fulcrum socket: next surface send succeeds with NO desktop context.
    let _ = std::fs::remove_file(&fulcrum_sock);
    let log_offset = log_after_1.len();
    write_frame(
        &mut surface,
        &json!({ "op": "send", "thread": tid, "text": "without fulcrum" }),
    )
    .unwrap();
    let sent2 = read_frame(&mut surface).unwrap();
    assert_eq!(sent2["reply"], "sent");
    let msg_ev2 = read_frame(&mut surface).unwrap();
    assert_eq!(msg_ev2["event"], "message");
    assert!(
        msg_ev2.get("context").is_none(),
        "expected no context when Fulcrum is away: {msg_ev2}"
    );
    let evs2 = turn_events(&mut surface, &tid, 2);
    assert_eq!(text_of(&evs2), "seen");

    let log_after_2 = &std::fs::read_to_string(&rpc_log).unwrap()[log_offset..];
    assert!(log_after_2.contains("without fulcrum"), "{log_after_2}");
    assert!(!log_after_2.contains("<desktop-context>"), "{log_after_2}");

    // 3. Person-socket send carries no desktop context even if Fulcrum were listening.
    let listener2 = UnixListener::bind(&fulcrum_sock).unwrap();
    listener2.set_nonblocking(true).unwrap();
    let mut person = handshake(&lever.person);
    let (ptid, pturn) = send(&mut person, None, "from terminal");
    let pevs = turn_events(&mut person, &ptid, pturn);
    assert_eq!(text_of(&pevs), "seen");
    assert!(listener2.accept().is_err(), "person send should not query Fulcrum");
    let prec = resume(&mut person, &ptid);
    assert!(prec["turns"][0].get("context").is_none());

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

/// Lever's arguments for the stub harness with `script`, its log, and more.
fn stub_args(script: &Path, log: &Path, more: &[&str]) -> Vec<String> {
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
fn ask_lever(peer: &mut UnixStream, op: Value) -> Value {
    write_frame(peer, &op).unwrap();
    read_until(peer, |f| f.get("reply").is_some())
}

/// The answers the stub harness gave Lever's requests, in its log: the
/// option each chose, or `cancelled`.
fn stub_heard(log: &Path) -> Vec<String> {
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
fn stub_modes_set(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v["method"] == "session/set_config_option" && v["params"]["configId"] == "mode")
        .map(|v| v["params"]["value"].as_str().unwrap_or("").to_string())
        .collect()
}

fn ask_script(dir: &Path) -> PathBuf {
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

#[test]
fn a_request_waits_for_the_person_with_no_time_limit_and_is_answered_once_always_or_no() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--idle-timeout-ms", "200"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let mut shell = handshake(&lever.surface);

    // The shell watches what works and what waits.
    assert_eq!(
        ask_lever(&mut shell, json!({ "op": "watch" })),
        json!({ "reply": "activity", "working": [], "waiting": [] })
    );

    let (thread, _) = send(&mut peer, None, "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(request["state"], "waiting");
    assert_eq!(request["kind"], "command");
    assert_eq!(request["detail"], "echo hi && id -un");
    assert_eq!(
        request["answers"],
        json!([
            { "answer": "once", "name": "Allow once" },
            { "answer": "always", "name": "Always allow" },
            { "answer": "no", "name": "Reject" }
        ])
    );
    assert!(request["act"].as_str().unwrap().starts_with("act-"), "{request}");
    let request_id = request["request"].as_str().unwrap().to_string();
    assert!(request_id.starts_with("r-"), "{request}");
    let activity = read_until(&mut shell, |f| f["event"] == "activity" && f["waiting"] != json!([]));
    assert_eq!(activity["waiting"], json!([thread.clone()]));
    assert_eq!(activity["working"], json!([thread.clone()]));

    // It waits with no time limit: past the idle timeout, the harness still runs.
    std::thread::sleep(Duration::from_millis(700));
    let status = ask_lever(&mut peer, json!({ "op": "status" }));
    assert_eq!(status["harness_started"], true);
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(list["threads"][0]["waiting"], true, "{list}");
    assert_eq!(list["threads"][0]["mode"], "ask-first", "{list}");

    // Only an answer it offers, for its own thread, is taken.
    let wrong = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": request_id, "answer": "maybe" }),
    );
    assert_eq!(wrong["code"], "invalid-answer");
    let missing = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": "r-000000000000", "answer": "once" }),
    );
    assert_eq!(missing["code"], "no-request");

    // Once: it runs, and the next time it asks again.
    let ok = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": request_id, "answer": "once" }),
    );
    assert_eq!(ok, json!({ "reply": "ok" }));
    let answered = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(
        (answered["state"].as_str(), answered["answer"].as_str()),
        (Some("answered"), Some("once"))
    );
    let act = read_until(&mut peer, |f| f["event"] == "act" && f["state"] == "done");
    assert_eq!(act["detail"], "hi");
    read_until(&mut peer, |f| f["event"] == "done");
    let again = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": request_id, "answer": "once" }),
    );
    assert_eq!(again["code"], "no-request", "an answered request is answered once");

    send(&mut peer, Some(&thread), "please run it again");
    let second = read_until(&mut peer, |f| f["event"] == "request");
    // Always: it runs, and is not asked again.
    ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": second["request"], "answer": "always" }),
    );
    read_until(&mut peer, |f| f["event"] == "done");
    send(&mut peer, Some(&thread), "please run it a third time");
    let third = read_until(&mut peer, |f| f["event"] == "request" || f["event"] == "done");
    assert_eq!(third["event"], "done", "always is the harness's to keep: {third}");

    // No: the harness is told, and ends the turn as it has it.
    send(&mut peer, Some(&thread), "please edit it");
    let edit = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(edit["kind"], "edit");
    assert_eq!(edit["path"], "/w/seed.txt");
    assert_eq!(edit["detail"], "-alpha\n+beta");
    ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": edit["request"], "answer": "no" }),
    );
    let failed = read_until(&mut peer, |f| f["event"] == "act" && f["state"] == "failed");
    assert_eq!(failed["detail"], "The user declined this tool call");
    read_until(&mut peer, |f| f["event"] == "stopped" && f["turn"] == 4);
    assert_eq!(stub_heard(&stub_log), ["once", "always", "reject"]);

    // The record has each request, and how it was answered.
    let resumed = ask_lever(&mut peer, json!({ "op": "resume", "thread": thread }));
    let answers: Vec<_> = resumed["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            t["requests"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["answer"].clone())
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(
        answers,
        vec![vec![json!("once")], vec![json!("always")], vec![], vec![json!("no")]]
    );
    assert_eq!(resumed["turns"][0]["mode"], "ask-first");
    read_until(&mut shell, |f| {
        f["event"] == "activity" && f["waiting"] == json!([]) && f["working"] == json!([])
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn stop_and_the_ends_of_lever_and_its_harness_withdraw_a_waiting_request() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);

    // Stop withdraws it, and the harness is told so.
    let (thread, _) = send(&mut peer, None, "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread })),
        json!({ "reply": "ok" })
    );
    let withdrawn = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(
        (withdrawn["request"].clone(), withdrawn["state"].as_str()),
        (request["request"].clone(), Some("withdrawn"))
    );
    read_until(&mut peer, |f| f["event"] == "stopped");
    assert_eq!(stub_heard(&stub_log), ["cancelled"]);
    let gone = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": request["request"], "answer": "once" }),
    );
    assert_eq!(gone["code"], "no-request");

    // Lever killed while a request waits: after it starts again, the turn
    // was interrupted and the request withdrawn.
    send(&mut peer, Some(&thread), "please run it again");
    read_until(&mut peer, |f| f["event"] == "request");
    drop(peer);
    lever.kill_and_wait();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let resumed = ask_lever(&mut peer, json!({ "op": "resume", "thread": thread }));
    assert_eq!(resumed["turns"][1]["state"], "interrupted");
    assert_eq!(resumed["turns"][1]["requests"][0]["state"], "withdrawn");
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(list["threads"][0]["waiting"], false);

    // The harness ends while a request waits: withdrawn, and interrupted.
    send(&mut peer, Some(&thread), "please run it once more");
    read_until(&mut peer, |f| f["event"] == "request");
    let stub_pid: Vec<u32> = std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            std::fs::read_to_string(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|c| c.starts_with(&format!("{STUB_BIN}\0")) && c.contains(script.to_str().unwrap()))
        })
        .collect();
    assert_eq!(stub_pid.len(), 1, "one stub harness runs");
    unsafe { libc::kill(stub_pid[0] as libc::pid_t, libc::SIGKILL) };
    let withdrawn = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(withdrawn["state"], "withdrawn");
    read_until(&mut peer, |f| f["event"] == "interrupted");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_thread_keeps_its_mode_and_new_threads_start_in_the_mode_the_person_chose() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let mut shell = handshake(&lever.surface);

    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "status" }))["default_mode"],
        "ask-first"
    );
    let models = ask_lever(&mut peer, json!({ "op": "models" }));
    assert_eq!(
        models["modes"],
        json!([
            { "id": "ask-first", "name": "Ask first" },
            { "id": "full-access", "name": "Full access" },
            { "id": "plan", "name": "Plan" }
        ])
    );

    // A new thread asks first; the session starts there, and Lever sets nothing.
    let (first, _) = send(&mut peer, None, "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");
    ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": first, "request": request["request"], "answer": "once" }),
    );
    read_until(&mut peer, |f| f["event"] == "done");
    assert!(stub_modes_set(&stub_log).is_empty());

    // Full access from the drawer: the next turn runs in OpenCode's build, and asks nothing.
    let set = ask_lever(
        &mut shell,
        json!({ "op": "set-mode", "thread": first, "mode": "full-access" }),
    );
    assert_eq!(
        set,
        json!({ "reply": "mode-set", "thread": first, "mode": "full-access" })
    );
    send(&mut peer, Some(&first), "please run it again");
    let next = read_until(&mut peer, |f| f["event"] == "request" || f["event"] == "done");
    assert_eq!(next["event"], "done", "{next}");
    assert_eq!(stub_modes_set(&stub_log), ["build"]);

    // Only Lever's modes, and only starting modes for new threads.
    let bad = ask_lever(&mut peer, json!({ "op": "set-mode", "thread": first, "mode": "yolo" }));
    assert_eq!(bad["code"], "invalid-mode");
    let bad = ask_lever(&mut peer, json!({ "op": "set-default-mode", "mode": "plan" }));
    assert_eq!(bad["code"], "invalid-mode");
    let bad = ask_lever(&mut peer, json!({ "op": "send", "text": "x", "mode": "yolo" }));
    assert_eq!(bad["code"], "invalid-mode");

    // Full access as the starting mode, from the drawer.
    let default = ask_lever(&mut shell, json!({ "op": "set-default-mode", "mode": "full-access" }));
    assert_eq!(default, json!({ "reply": "default-mode", "mode": "full-access" }));
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "status" }))["default_mode"],
        "full-access"
    );
    let (second, _) = send(&mut peer, None, "please run it");
    let next = read_until(&mut peer, |f| f["event"] == "request" || f["event"] == "done");
    assert_eq!(next["event"], "done", "{next}");
    // A thread can still start in another mode.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "please run it", "cwd": "/tmp", "mode": "ask-first" }),
    )
    .unwrap();
    let third = read_until(&mut peer, |f| f["reply"] == "sent")["thread"]
        .as_str()
        .unwrap()
        .to_string();
    let request = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(request["thread"], third.as_str());

    // A thread working cannot change its mode.
    let busy = ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": third, "mode": "full-access" }),
    );
    assert_eq!(busy["code"], "working");
    ask_lever(&mut peer, json!({ "op": "stop", "thread": third }));
    read_until(&mut peer, |f| f["event"] == "stopped" && f["thread"] == third.as_str());

    // Plan: an edit is refused by the harness, and nothing is asked.
    ask_lever(&mut peer, json!({ "op": "set-mode", "thread": first, "mode": "plan" }));
    send(&mut peer, Some(&first), "please edit it");
    let refused = read_until(&mut peer, |f| f["event"] == "act" && f["state"] == "failed");
    assert_eq!(refused["detail"], "Not allowed in this mode");
    read_until(&mut peer, |f| f["event"] == "done");

    // The harness changes the mode itself: the thread takes it.
    ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": second, "mode": "ask-first" }),
    );
    send(&mut peer, Some(&second), "please switch");
    let updated = read_until(&mut peer, |f| f["event"] == "thread-updated");
    assert_eq!(
        updated,
        json!({ "event": "thread-updated", "thread": second, "mode": "full-access" })
    );
    read_until(&mut peer, |f| f["event"] == "done");

    // Each thread keeps its mode across a restart of Lever, and its next turn
    // sets it on the harness started again.
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    let modes: std::collections::BTreeMap<String, String> = list["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["thread"].as_str().unwrap().to_string(),
                t["mode"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(modes[&first], "plan");
    assert_eq!(modes[&second], "full-access");
    assert_eq!(modes[&third], "ask-first");
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "resume", "thread": first }))["mode"],
        "plan"
    );
    let before = stub_modes_set(&stub_log).len();
    send(&mut peer, Some(&first), "hello");
    read_until(&mut peer, |f| f["event"] == "done");
    assert_eq!(stub_modes_set(&stub_log)[before..], ["plan"]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn lever_adds_ask_first_for_its_own_harness_and_leaves_the_persons_opencode_files_alone() {
    let dir = TempDir::new();
    let uid = my_uid();
    let home = dir.0.join("home");
    let config = home.join(".config/opencode");
    std::fs::create_dir_all(&config).unwrap();
    let mine = b"{\n  // the person's own\n  \"permission\": {\"bash\": {\"*\": \"allow\", \"rm *\": \"deny\"}},\n  \"agent\": {\"mine\": {\"mode\": \"primary\"}},\n}\n";
    std::fs::write(config.join("opencode.jsonc"), mine).unwrap();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let env_out = dir.0.join("env.json");
    let extra = stub_args(
        &script,
        &stub_log,
        &["--harness-arg", "--env-out", "--harness-arg", env_out.to_str().unwrap()],
    );
    let xdg = home.join(".config");
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("XDG_CONFIG_HOME", xdg.to_str().unwrap()),
        (
            "OPENCODE_CONFIG_CONTENT",
            r#"{"agent":{"from-env":{"mode":"primary"}}}"#,
        ),
    ];
    let mut lever = RunningLever::start_with_env(&dir.0, uid, uid, &extra, &env);
    let mut peer = handshake(&lever.person);
    send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");

    let seen: Value = serde_json::from_str(&std::fs::read_to_string(&env_out).unwrap()).unwrap();
    let content: Value = serde_json::from_str(seen["OPENCODE_CONFIG_CONTENT"].as_str().unwrap()).unwrap();
    assert_eq!(content["default_agent"], "crowbar-ask-first");
    // Ask first asks before every command and edit, except where the
    // person's own rules say otherwise: what they refuse stays refused.
    let permission = &content["agent"]["crowbar-ask-first"]["permission"];
    assert_eq!(
        permission,
        &json!({ "bash": { "*": "ask", "rm *": "deny" }, "edit": { "*": "ask" } })
    );
    let order: Vec<&String> = permission["bash"].as_object().unwrap().keys().collect();
    assert_eq!(order, ["*", "rm *"], "the person's rule comes after Ask first's");
    assert_eq!(content["agent"]["from-env"], json!({ "mode": "primary" }), "{content}");
    assert_eq!(content["update"], "disable");

    // The person's own file is as they made it, and Lever made no other.
    assert_eq!(std::fs::read(config.join("opencode.jsonc")).unwrap(), mine);
    let files: Vec<_> = std::fs::read_dir(&config)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(files, vec![std::ffi::OsString::from("opencode.jsonc")]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

/// Whether a process runs, and is not a zombie.
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| stat.rfind(')').map(|at| stat[at + 1..].trim_start().starts_with('Z')))
        .is_some_and(|zombie| !zombie)
}

#[test]
fn a_stop_the_harness_ignores_ends_the_turn_and_every_command_it_started() {
    let dir = TempDir::new();
    let uid = my_uid();
    let pid_file = dir.0.join("sleep.pid");
    let detached_file = dir.0.join("detached.pid");
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "ignore",
                    "steps": [
                        { "kind": "spawn", "pid_file": pid_file.to_str().unwrap() },
                        { "kind": "spawn", "pid_file": detached_file.to_str().unwrap(), "detach": true },
                        { "kind": "text", "text": "Working" }
                    ],
                    "stall": true,
                    "ignore_cancel": true
                },
                { "match": "other", "steps": [{ "kind": "text", "text": "Other" }], "stall": true },
                { "match": "hello", "steps": [{ "kind": "text", "text": "Hello again" }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--stop-timeout-ms", "500"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);

    let (other, _) = send(&mut peer, None, "other");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == other.as_str());
    let (stuck, _) = send(&mut peer, None, "ignore");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == stuck.as_str());
    let pid: u32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    assert!(alive(pid), "the turn's command runs");
    // A command run in the background, whose shell has ended.
    let detached: u32 = std::fs::read_to_string(&detached_file).unwrap().trim().parse().unwrap();
    assert!(alive(detached), "the command it left behind runs");

    let asked = Instant::now();
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": stuck })),
        json!({ "reply": "ok" })
    );
    let mut ended = std::collections::BTreeMap::new();
    while ended.len() < 2 {
        let frame = read_until(&mut peer, |f| {
            ["stopped", "interrupted"].contains(&f["event"].as_str().unwrap_or(""))
        });
        ended.insert(
            frame["thread"].as_str().unwrap().to_string(),
            frame["event"].as_str().unwrap().to_string(),
        );
    }
    let waited = asked.elapsed();
    assert!(
        waited >= Duration::from_millis(450),
        "Lever waited for the harness first: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(2),
        "Lever did not wait for ever: {waited:?}"
    );
    assert_eq!(ended[&stuck], "stopped");
    assert_eq!(ended[&other], "interrupted", "a turn of the same harness cannot go on");
    let deadline = Instant::now() + Duration::from_secs(2);
    while alive(pid) || alive(detached) {
        assert!(Instant::now() < deadline, "the turn's commands still run");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"],
        false
    );
    assert!(
        lever.log_text().contains("did not stop a turn when asked"),
        "{}",
        lever.log_text()
    );

    // The record says so, and the next message starts the harness again.
    let resumed = ask_lever(&mut peer, json!({ "op": "resume", "thread": stuck }));
    assert_eq!(resumed["turns"][0]["state"], "stopped");
    send(&mut peer, Some(&other), "hello");
    let text = read_until(&mut peer, |f| f["event"] == "text");
    assert_eq!(text["text"], "Hello again");
    read_until(&mut peer, |f| f["event"] == "done");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_the_harness_carries_out_leaves_the_other_threads_working() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                { "match": "one", "steps": [{ "kind": "text", "text": "One" }], "stall": true },
                { "match": "two", "steps": [{ "kind": "text", "text": "Two" }], "stall": true }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--stop-timeout-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (one, _) = send(&mut peer, None, "one");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == one.as_str());
    let (two, _) = send(&mut peer, None, "two");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == two.as_str());

    ask_lever(&mut peer, json!({ "op": "stop", "thread": two }));
    read_until(&mut peer, |f| f["event"] == "stopped" && f["thread"] == two.as_str());
    std::thread::sleep(Duration::from_millis(600));
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    let one_now = list["threads"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["thread"] == one.as_str())
        .unwrap();
    assert_eq!(one_now["state"], "working", "{list}");
    assert!(!lever.log_text().contains("did not stop a turn when asked"));

    ask_lever(&mut peer, json!({ "op": "stop", "thread": one }));
    read_until(&mut peer, |f| f["event"] == "stopped" && f["thread"] == one.as_str());
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_after_its_turn_keeps_the_harness_until_stop_withdraws_it() {
    let dir = TempDir::new();
    let uid = my_uid();
    // The turn ends, and a request comes after it, as a subagent's would.
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [{ "kind": "text", "text": "Done here." }],
                "after": [{ "kind": "ask", "id": "c7", "title": "make", "raw_input": { "command": "make" } }]
            }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--idle-timeout-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut watcher = handshake(&lever.surface);
    assert_eq!(
        ask_lever(&mut watcher, json!({"op":"list","watch":true}))["reply"],
        "list"
    );
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");
    let request = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(
        (request["turn"].as_u64(), request["detail"].as_str()),
        (Some(1), Some("make"))
    );

    // Past the idle timeout, the harness still runs: a request waits on it.
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"], true);
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(
        (
            list["threads"][0]["working"].clone(),
            list["threads"][0]["waiting"].clone()
        ),
        (json!(false), json!(true))
    );

    // A post-turn request still keeps deletion unsafe and the list current.
    let snapshot = read_until(&mut watcher, |f| {
        f["event"] == "threads" && f["threads"][0]["waiting"] == true
    });
    assert_eq!(snapshot["threads"][0]["working"], false);
    assert_eq!(snapshot["threads"][0]["archived"], false);
    assert_eq!(snapshot["threads"][0]["mode"], "ask-first");
    assert_eq!(
        ask_lever(&mut peer, json!({"op":"delete","thread":thread}))["code"],
        "working"
    );
    assert_eq!(
        ask_lever(&mut peer, json!({"op":"resume","thread":thread}))["reply"],
        "resumed"
    );

    // Stop withdraws it though no turn works, and the harness is then idle.
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread })),
        json!({ "reply": "ok" })
    );
    let withdrawn = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(withdrawn["state"], "withdrawn");
    let deadline = Instant::now() + Duration::from_secs(3);
    while ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == true {
        assert!(Instant::now() < deadline, "an idle harness is stopped");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(stub_heard(&stub_log), ["cancelled"]);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_for_a_turn_that_ended_withdraws_nothing_of_the_turn_after_it() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");
    send(&mut peer, Some(&thread), "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");

    // A Stop that comes late, for the turn before, leaves this one's request.
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": first })),
        json!({ "reply": "ok" })
    );
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(list["threads"][0]["waiting"], true, "{list}");
    assert_eq!(
        ask_lever(
            &mut peer,
            json!({ "op": "answer", "thread": thread, "request": request["request"], "answer": "once" })
        ),
        json!({ "reply": "ok" })
    );
    read_until(&mut peer, |f| f["event"] == "done");
    assert_eq!(stub_heard(&stub_log), ["once"]);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_the_person_asked_to_stop_asks_them_nothing_more() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [
                    { "kind": "text", "text": "Working" },
                    { "kind": "pause_ms", "ms": 400 },
                    { "kind": "ask", "id": "c1", "title": "make", "raw_input": { "command": "make" } }
                ],
                "stall": true,
                "ignore_cancel": true
            }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--stop-timeout-ms", "5000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "text");
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));

    // What the harness asks after the Stop is answered for the person: no.
    let end = read_until(&mut peer, |f| f["event"] == "request" || f["event"] == "stopped");
    assert_eq!(end["event"], "stopped", "{end}");
    assert_eq!(stub_heard(&stub_log), ["cancelled"]);
    let resumed = ask_lever(&mut peer, json!({ "op": "resume", "thread": thread }));
    assert_eq!(resumed["turns"][0]["requests"], json!([]));
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_stopped_while_its_mode_is_set_holds_the_thread_until_the_harness_answers() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "set_config_delay_ms": 600,
            "turns": [
                {
                    "match": "please run",
                    "steps": [{ "kind": "ask", "id": "c1", "title": "make", "raw_input": { "command": "make" } }]
                },
                { "steps": [{ "kind": "text", "text": "Hello." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");

    // The message asks for Full access. The harness takes it at once, and
    // answers late; the turn is stopped before it answers.
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "hello again", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    wait_until("Lever asks for Full access", || stub_modes_set(&stub_log) == ["build"]);
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    read_until(&mut peer, |f| f["event"] == "stopped");

    // Until it answers, the thread takes no other mode and no message.
    let busy = ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": thread, "mode": "ask-first" }),
    );
    assert_eq!(busy["code"], "working", "{busy}");
    let busy = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "please run it" }),
    );
    assert_eq!(busy["code"], "working", "{busy}");

    // Then Ask first is set again, and the next turn asks.
    wait_until("the thread is free", || {
        ask_lever(
            &mut peer,
            json!({ "op": "set-mode", "thread": thread, "mode": "ask-first" }),
        )["reply"]
            == "mode-set"
    });
    send(&mut peer, Some(&thread), "please run it");
    let asked = read_until(&mut peer, |f| f["event"] == "request" || f["event"] == "done");
    assert_eq!(asked["event"], "request", "{asked}");
    assert_eq!(stub_modes_set(&stub_log), ["build", "crowbar-ask-first"]);
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    read_until(&mut peer, |f| f["event"] == "stopped");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_that_says_only_what_changed_still_offers_its_modes() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "partial_config_replies": true,
            "models": [{ "id": "m1", "name": "One" }, { "id": "m2", "name": "Two" }],
            "turns": [{ "steps": [{ "kind": "text", "text": "Hello." }] }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "set-model", "thread": thread, "model": "m2" }))["reply"],
        "model-set"
    );
    let models = ask_lever(&mut peer, json!({ "op": "models" }));
    assert_eq!(models["modes"].as_array().map(Vec::len), Some(3), "{models}");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_whose_modes_are_not_levers_runs_turns_and_new_threads_start_only_in_one_it_offers() {
    let dir = TempDir::new();
    let uid = my_uid();
    // Modes of its own: Lever sets none, and the turn runs.
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "mode_values": ["default", "yolo"],
            "turns": [{ "steps": [{ "kind": "text", "text": "Hello." }] }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    send(&mut peer, None, "hello");
    let end = read_until(&mut peer, |f| {
        ["done", "failed"].contains(&f["event"].as_str().unwrap_or(""))
    });
    assert_eq!(end["event"], "done", "{end}");
    assert!(stub_modes_set(&stub_log).is_empty());
    assert_eq!(ask_lever(&mut peer, json!({ "op": "models" }))["modes"], json!([]));
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    // No Full access: new threads cannot start in it.
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "mode_values": ["crowbar-ask-first", "plan"],
            "turns": [{ "steps": [{ "kind": "text", "text": "Hello." }] }]
        }),
    );
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    ask_lever(&mut peer, json!({ "op": "models" }));
    let refused = ask_lever(&mut peer, json!({ "op": "set-default-mode", "mode": "full-access" }));
    assert_eq!(refused["code"], "invalid-mode", "{refused}");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_answer_the_database_does_not_take_is_kept_and_saved_when_it_does() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");

    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE ON requests");
    assert_eq!(
        ask_lever(
            &mut peer,
            json!({ "op": "answer", "thread": thread, "request": request["request"], "answer": "once" })
        ),
        json!({ "reply": "ok" })
    );
    // Who watches is told, and whoever asks hears it answered.
    let answered = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(
        (answered["state"].as_str(), answered["answer"].as_str()),
        (Some("answered"), Some("once"))
    );
    read_until(&mut peer, |f| f["event"] == "done");
    let list = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(list["threads"][0]["waiting"], false, "{list}");
    let resumed = ask_lever(&mut peer, json!({ "op": "resume", "thread": thread }));
    assert_eq!(resumed["turns"][0]["requests"][0]["state"], "answered");

    // The record gets it when the database takes writes again.
    db.execute_batch("DROP TRIGGER refused").unwrap();
    wait_until("the answer is saved", || {
        db.query_row("SELECT state FROM requests", [], |r| r.get::<_, String>(0))
            .is_ok_and(|state| state == "answered")
    });
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_the_harness_withdraws_is_answered_cancelled_and_withdrawn_for_the_person() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [
                    { "kind": "ask", "id": "c1", "title": "make", "raw_input": { "command": "make" },
                      "withdraw_after_ms": 300 },
                    { "kind": "text", "text": "Done." }
                ]
            }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "request" && f["state"] == "waiting");
    let withdrawn = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(withdrawn["state"], "withdrawn");
    read_until(&mut peer, |f| f["event"] == "stopped");
    wait_until("the harness has its answer", || stub_heard(&stub_log) == ["cancelled"]);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_in_another_mode_tells_who_watches_the_threads_mode() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");
    let mut shell = handshake(&lever.surface);
    ask_lever(&mut shell, json!({ "op": "resume", "thread": thread }));

    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "hello", "mode": "full-access" }),
    )
    .unwrap();
    let updated = read_until(&mut shell, |f| {
        f["event"] == "thread-updated" || f["event"] == "message"
    });
    assert_eq!(
        updated,
        json!({ "event": "thread-updated", "thread": thread, "mode": "full-access" })
    );
    read_until(&mut shell, |f| f["event"] == "done");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_that_does_not_say_its_kind_leaves_its_acts_kind() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [{ "kind": "ask", "id": "c1", "title": "make", "raw_input": { "command": "make" },
                            "request_without_kind": true }]
            }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    let act = read_until(&mut peer, |f| f["event"] == "act");
    let request = read_until(&mut peer, |f| f["event"] == "request");
    let resumed = ask_lever(&mut peer, json!({ "op": "resume", "thread": thread }));
    assert_eq!(resumed["turns"][0]["acts"][0]["kind"], act["kind"], "{resumed}");
    assert_eq!(request["kind"], "other");
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    read_until(&mut peer, |f| f["event"] == "stopped");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn turns_the_person_asked_to_stop_end_as_stopped_when_the_harness_is_ended() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                { "match": "one", "steps": [{ "kind": "text", "text": "One" }], "stall": true, "ignore_cancel": true },
                { "match": "two", "steps": [{ "kind": "text", "text": "Two" }], "stall": true, "ignore_cancel": true }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--stop-timeout-ms", "600"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (one, _) = send(&mut peer, None, "one");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == one.as_str());
    let (two, _) = send(&mut peer, None, "two");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == two.as_str());
    ask_lever(&mut peer, json!({ "op": "stop", "thread": one }));
    std::thread::sleep(Duration::from_millis(200));
    ask_lever(&mut peer, json!({ "op": "stop", "thread": two }));
    let mut ended = std::collections::BTreeMap::new();
    while ended.len() < 2 {
        let frame = read_until(&mut peer, |f| {
            ["stopped", "interrupted"].contains(&f["event"].as_str().unwrap_or(""))
        });
        ended.insert(
            frame["thread"].as_str().unwrap().to_string(),
            frame["event"].as_str().unwrap().to_string(),
        );
    }
    assert_eq!(ended[&one], "stopped");
    assert_eq!(ended[&two], "stopped", "the person asked to stop it too");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_that_ends_when_told_to_stop_leaves_nothing_of_the_turn_running() {
    let dir = TempDir::new();
    let uid = my_uid();
    let detached_file = dir.0.join("detached.pid");
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "spawn", "pid_file": detached_file.to_str().unwrap(), "detach": true },
                    { "kind": "text", "text": "Working" }
                ],
                "stall": true,
                "exit_on_cancel": true
            }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "text");
    let detached: u32 = std::fs::read_to_string(&detached_file).unwrap().trim().parse().unwrap();
    assert!(alive(detached), "the command it left behind runs");

    // The harness ends on the Stop, before it ends the turn.
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    let end = read_until(&mut peer, |f| {
        ["stopped", "interrupted"].contains(&f["event"].as_str().unwrap_or(""))
    });
    assert_eq!(end["event"], "stopped");
    wait_until("the turn's command ends", || !alive(detached));
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_that_never_answers_a_stopped_turns_mode_is_ended_and_the_thread_is_free() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "set_config_delay_ms": 60000,
            "turns": [{ "steps": [{ "kind": "text", "text": "Hello." }] }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--stop-timeout-ms", "500"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");
    ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": thread, "mode": "full-access" }),
    );
    send(&mut peer, Some(&thread), "hello again");
    wait_until("Lever asks for Full access", || stub_modes_set(&stub_log) == ["build"]);
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    read_until(&mut peer, |f| f["event"] == "stopped");

    // The harness never answers: after the Stop's deadline it is ended, and
    // the thread takes a mode again.
    wait_until("the thread is free", || {
        ask_lever(
            &mut peer,
            json!({ "op": "set-mode", "thread": thread, "mode": "full-access" }),
        )["reply"]
            == "mode-set"
    });
    assert!(
        lever.log_text().contains("did not answer a change of mode"),
        "{}",
        lever.log_text()
    );
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_the_harness_ignores_ends_what_it_started_and_not_what_an_earlier_harness_left() {
    let dir = TempDir::new();
    let uid = my_uid();
    let earlier_file = dir.0.join("earlier.pid");
    let stuck_file = dir.0.join("stuck.pid");
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "leave",
                    "steps": [
                        { "kind": "spawn", "pid_file": earlier_file.to_str().unwrap(), "detach": true },
                        { "kind": "text", "text": "Left a server running." }
                    ]
                },
                {
                    "match": "ignore",
                    "steps": [
                        { "kind": "spawn", "pid_file": stuck_file.to_str().unwrap(), "detach": true },
                        { "kind": "text", "text": "Working" }
                    ],
                    "stall": true,
                    "ignore_cancel": true
                }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(
        &script,
        &stub_log,
        &["--idle-timeout-ms", "300", "--stop-timeout-ms", "500"],
    );
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);

    // One harness leaves a command running in the background, and is
    // stopped when idle.
    send(&mut peer, None, "leave");
    read_until(&mut peer, |f| f["event"] == "done");
    let earlier: u32 = std::fs::read_to_string(&earlier_file).unwrap().trim().parse().unwrap();
    wait_until("the idle harness is stopped", || {
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == false
    });

    // The next one ignores a Stop: what it started ends, and what the one
    // before left goes on.
    let (stuck, _) = send(&mut peer, None, "ignore");
    read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == stuck.as_str());
    let mine: u32 = std::fs::read_to_string(&stuck_file).unwrap().trim().parse().unwrap();
    ask_lever(&mut peer, json!({ "op": "stop", "thread": stuck }));
    read_until(&mut peer, |f| f["event"] == "stopped");
    wait_until("the stopped turn's command ends", || !alive(mine));
    let still = alive(earlier);
    unsafe { libc::kill(earlier as libc::pid_t, libc::SIGKILL) };
    assert!(still, "what an earlier harness left is not this one's to end");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn how_a_request_ends_is_told_though_the_database_can_be_neither_read_nor_written() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");

    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch("ALTER TABLE requests RENAME TO requests_away")
        .unwrap();
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    let withdrawn = read_until(&mut peer, |f| f["event"] == "request");
    assert_eq!(
        (
            withdrawn["request"].clone(),
            withdrawn["state"].as_str(),
            withdrawn["detail"].as_str()
        ),
        (request["request"].clone(), Some("withdrawn"), Some("echo hi && id -un"))
    );
    db.execute_batch("ALTER TABLE requests_away RENAME TO requests")
        .unwrap();
    wait_until("the end is saved", || {
        db.query_row("SELECT state FROM requests", [], |r| r.get::<_, String>(0))
            .is_ok_and(|state| state == "withdrawn")
    });
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_lever_holds_is_answered_and_a_working_thread_stopped_though_its_record_cannot_be_read() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = ask_script(&dir.0);
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "please run it");
    let request = read_until(&mut peer, |f| f["event"] == "request");

    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch("ALTER TABLE threads RENAME TO threads_away").unwrap();
    assert_eq!(
        ask_lever(
            &mut peer,
            json!({ "op": "answer", "thread": thread, "request": request["request"], "answer": "once" })
        ),
        json!({ "reply": "ok" })
    );
    read_until(&mut peer, |f| f["event"] == "request" && f["state"] == "answered");
    db.execute_batch("ALTER TABLE threads_away RENAME TO threads").unwrap();
    read_until(&mut peer, |f| f["event"] == "done" || f["event"] == "failed");

    send(&mut peer, Some(&thread), "please stall");
    read_until(&mut peer, |f| f["event"] == "text");
    db.execute_batch("ALTER TABLE threads RENAME TO threads_away").unwrap();
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread })),
        json!({ "reply": "ok" })
    );
    read_until(&mut peer, |f| f["event"] == "stopped");
    db.execute_batch("ALTER TABLE threads_away RENAME TO threads").unwrap();
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn list_watch_tracks_terminal_work_and_archive_preserves_history_without_a_harness() {
    let dir = TempDir::new();
    let script = dir.0.join("script.json");
    std::fs::write(
        &script,
        r#"{"turns":[{"match":"slow","stall":true,"steps":[{"kind":"text","text":"working"}]}]}"#,
    )
    .unwrap();
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut watcher = handshake(&lever.surface);
    write_frame(&mut watcher, &json!({"op":"list","watch":true})).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["threads"], json!([]));
    let mut terminal = handshake(&lever.person);
    write_frame(&mut terminal, &json!({"op":"send","text":"slow","cwd":"/tmp"})).unwrap();
    let sent = read_until(&mut terminal, |v| v["reply"] == "sent");
    let tid = &sent["thread"];
    let list = read_until(&mut watcher, |v| v["event"] == "threads");
    assert_eq!(list["threads"][0]["thread"], *tid);
    assert_eq!(list["threads"][0]["cwd"], "/tmp");
    assert_eq!(list["threads"][0]["state"], "working");
    let mut admin = handshake(&lever.person);
    write_frame(&mut admin, &json!({"op":"delete","thread":tid})).unwrap();
    assert_eq!(read_frame(&mut admin).unwrap()["code"], "working");
    write_frame(&mut admin, &json!({"op":"archive","thread":tid})).unwrap();
    assert_eq!(read_frame(&mut admin).unwrap()["reply"], "archived");
    let hidden = read_until(&mut watcher, |v| v["event"] == "threads");
    assert_eq!(hidden["threads"][0]["archived"], true);
    assert_eq!(hidden["threads"][0]["working"], true);
    write_frame(&mut admin, &json!({"op":"list"})).unwrap();
    assert_eq!(read_frame(&mut admin).unwrap()["threads"], json!([]));
    write_frame(&mut admin, &json!({"op":"stop","thread":tid})).unwrap();
    assert_eq!(read_frame(&mut admin).unwrap()["reply"], "ok");
    let ended = read_until(&mut watcher, |v| v["event"] == "threads" && v["finished"] == true);
    assert_eq!(ended["threads"][0]["state"], "stopped");
    // Stop/restart with no harness configured: archive persists, resume reads SQLite alone.
    lever.signal(libc::SIGTERM);
    assert!(lever.wait().success());
    let mut lever = RunningLever::start(&dir.0, my_uid(), my_uid());
    let mut peer = handshake(&lever.surface);
    write_frame(&mut peer, &json!({"op":"list"})).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["threads"], json!([]));
    write_frame(&mut peer, &json!({"op":"resume","thread":tid})).unwrap();
    let resumed = read_frame(&mut peer).unwrap();
    assert_eq!(resumed["turns"][0]["message"], "slow");
    write_frame(&mut peer, &json!({"op":"status"})).unwrap();
    assert_eq!(read_frame(&mut peer).unwrap()["harness_started"], false);
    lever.signal(libc::SIGTERM);
    assert!(lever.wait().success());
}

#[test]
fn delete_removes_the_record_and_asks_the_harness_but_keeps_history_on_failure() {
    for fails in [false, true] {
        let dir = TempDir::new();
        let script = dir.0.join("script.json");
        let rpc = dir.0.join("rpc.jsonl");
        let mut spec = json!({"delete_session":true,"turns":[{"steps":[{"kind":"text","text":"answer"}]}]});
        if fails {
            spec["delete_session_error"] = json!({"code":-32603,"message":"Could not delete session"});
        }
        std::fs::write(&script, spec.to_string()).unwrap();
        let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc), None);
        let mut peer = handshake(&lever.person);
        write_frame(&mut peer, &json!({"op":"send","text":"hello","cwd":"/tmp"})).unwrap();
        let sent = read_until(&mut peer, |v| v["reply"] == "sent");
        let tid = sent["thread"].clone();
        read_until(&mut peer, |v| v["event"] == "done");
        // Restart so deletion must initialize the harness again, without loading history.
        lever.signal(libc::SIGTERM);
        assert!(lever.wait().success());
        let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc), None);
        let mut peer = handshake(&lever.person);
        write_frame(&mut peer, &json!({"op":"delete","thread":tid})).unwrap();
        let reply = read_until(&mut peer, |v| v.get("reply").is_some());
        assert_eq!(reply["reply"], if fails { "error" } else { "deleted" });
        write_frame(&mut peer, &json!({"op":"resume","thread":tid})).unwrap();
        let record = read_until(&mut peer, |v| v.get("reply").is_some());
        assert_eq!(record["reply"], if fails { "resumed" } else { "error" });
        assert!(std::fs::read_to_string(&rpc).unwrap().contains("session/delete"));
        let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
        let turns: i64 = db.query_row("SELECT count(*) FROM turns", [], |r| r.get(0)).unwrap();
        assert_eq!(turns, if fails { 1 } else { 0 });
        lever.signal(libc::SIGTERM);
        assert!(lever.wait().success());
    }
}

/// What the stub harness was asked to set of `config` on its sessions, in its
/// log, in order.
fn stub_config_set(log: &Path, config: &str) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|v| v["method"] == "session/set_config_option" && v["params"]["configId"] == config)
        .filter_map(|v| v["params"]["value"].as_str().map(str::to_string))
        .collect()
}

/// The prompts the stub harness was sent, in its log, each as one text.
fn stub_prompts(log: &Path) -> Vec<String> {
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

/// A script with three Models from two providers: one with effort levels,
/// one with fewer, and one with none.
fn effort_script(dir: &Path, more: Value) -> PathBuf {
    let mut script = json!({
        "models": [
            { "id": "stub/stub-1", "name": "stub/Stub One", "efforts": ["low", "high"] },
            { "id": "stub/stub-2", "name": "stub/Stub Two" },
            { "id": "other/stub-3", "name": "other/Stub Three", "efforts": ["low"] }
        ],
        "default_model": "stub/stub-1",
        // Every turn says the Model and the effort its session is at.
        "turns": [{ "match": "", "steps": [{ "kind": "text", "text": "{model} at {effort}" }] }]
    });
    for (key, value) in more.as_object().unwrap() {
        script[key] = value.clone();
    }
    write_script(dir, &script)
}

#[test]
fn models_say_each_models_provider_apart_from_its_name() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({}));
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let models = ask_lever(&mut peer, json!({ "op": "models" }));
    assert_eq!(
        models["models"],
        json!([
            { "id": "stub/stub-1", "name": "Stub One", "provider": "stub", "provider_name": "stub" },
            { "id": "stub/stub-2", "name": "Stub Two", "provider": "stub", "provider_name": "stub" },
            { "id": "other/stub-3", "name": "Stub Three", "provider": "other", "provider_name": "other" }
        ])
    );
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_models_effort_levels_are_read_from_the_harness_and_kept() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({}));
    let stub_log = dir.0.join("stub.log");
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&stub_log), None);
    let mut peer = handshake(&lever.person);

    // The harness is started for the asking, and a session opened, set to
    // the Model, read, and closed.
    let levels = ask_lever(&mut peer, json!({ "op": "efforts", "model": "other/stub-3" }));
    assert_eq!(
        levels,
        json!({ "reply": "efforts", "model": "other/stub-3", "efforts": [
            { "id": "low", "name": "Low" },
            { "id": "default", "name": "Default" }
        ]})
    );
    assert_eq!(stub_config_set(&stub_log, "model"), ["other/stub-3"]);
    let methods = |log: &Path| -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|v| v["method"].as_str().map(str::to_string))
            .collect()
    };
    wait_until("the session to be closed", || {
        methods(&stub_log)
            == [
                "initialize",
                "session/new",
                "session/set_config_option",
                "session/close",
            ]
    });

    // What Lever has read, it does not read again.
    let again = ask_lever(&mut peer, json!({ "op": "efforts", "model": "other/stub-3" }));
    assert_eq!(again, levels);
    assert_eq!(methods(&stub_log).len(), 4);

    // The Model a new session is at needs no setting, and one with no
    // levels has an empty list.
    let default = ask_lever(&mut peer, json!({ "op": "efforts", "model": "stub/stub-1" }));
    assert_eq!(
        default["efforts"],
        json!([
            { "id": "low", "name": "Low" },
            { "id": "high", "name": "High" },
            { "id": "default", "name": "Default" }
        ])
    );
    let none = ask_lever(&mut peer, json!({ "op": "efforts", "model": "stub/stub-2" }));
    assert_eq!(none["efforts"], json!([]));
    assert_eq!(stub_config_set(&stub_log, "model"), ["other/stub-3", "stub/stub-2"]);

    // A Model the harness is known not to offer has no levels to read.
    ask_lever(&mut peer, json!({ "op": "models" }));
    let unknown = ask_lever(&mut peer, json!({ "op": "efforts", "model": "stub/none" }));
    assert_eq!(unknown["code"], "invalid-model", "{unknown}");
    let bad = ask_lever(&mut peer, json!({ "op": "efforts" }));
    assert_eq!(bad["code"], "invalid-request", "{bad}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_thread_keeps_its_effort_and_each_turn_runs_at_it() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({}));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);

    // A new thread's first message says its effort, and who watches is told
    // the thread's Model and effort as Lever knows them.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "hello", "cwd": "/tmp", "effort": "high" }),
    )
    .unwrap();
    let sent = read_until(&mut peer, |f| f.get("reply").is_some());
    let thread = sent["thread"].as_str().unwrap().to_string();
    let events = turn_events(&mut peer, &thread, 1);
    assert_eq!(text_of(&events), "stub/stub-1 at high");
    assert_eq!(stub_config_set(&stub_log, "effort"), ["high"]);
    let record = resume(&mut peer, &thread);
    assert_eq!(
        (&record["model"], &record["effort"]),
        (&json!("stub/stub-1"), &json!("high"))
    );
    let listed = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(listed["threads"][0]["effort"], "high");

    // The next turn asks the harness for nothing: the session is at it.
    let (_, turn) = send(&mut peer, Some(&thread), "again");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "stub/stub-1 at high");
    assert_eq!(stub_config_set(&stub_log, "effort"), ["high"]);

    // `set-effort` applies from the next turn, and is refused for a level
    // the thread's Model is known not to have.
    let set = ask_lever(
        &mut peer,
        json!({ "op": "set-effort", "thread": thread, "effort": "low" }),
    );
    assert_eq!(set, json!({ "reply": "effort-set", "thread": thread, "effort": "low" }));
    assert_eq!(
        read_frame(&mut peer).unwrap(),
        json!({ "event": "thread-updated", "thread": thread, "effort": "low" })
    );
    let refused = ask_lever(
        &mut peer,
        json!({ "op": "set-effort", "thread": thread, "effort": "max" }),
    );
    assert_eq!(refused["code"], "invalid-effort", "{refused}");
    let refused = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "no", "effort": "max" }),
    );
    assert_eq!(refused["code"], "invalid-effort", "{refused}");
    let (_, turn) = send(&mut peer, Some(&thread), "lower");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "stub/stub-1 at low");

    // The effort outlasts Lever and the harness: a harness started again has
    // the session at its own effort, and Lever sets the thread's.
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    assert_eq!(resume(&mut peer, &thread)["effort"], "low");
    let (_, turn) = send(&mut peer, Some(&thread), "after a restart");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "stub/stub-1 at low");
    assert_eq!(stub_config_set(&stub_log, "effort"), ["high", "low", "low"]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_effort_is_one_models_and_a_thread_given_another_model_takes_what_the_harness_has() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({}));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);

    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "hello", "cwd": "/tmp", "effort": "high" }),
    )
    .unwrap();
    let thread = read_until(&mut peer, |f| f.get("reply").is_some())["thread"]
        .as_str()
        .unwrap()
        .to_string();
    turn_events(&mut peer, &thread, 1);

    // A Model that has no `high`: it is not asked for, and the thread is at
    // the effort the harness has the session at.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "on three", "model": "other/stub-3" }),
    )
    .unwrap();
    let mut told = Vec::new();
    let events = loop {
        let frame = read_frame(&mut peer).unwrap();
        if frame["event"] == "thread-updated" {
            told.push(frame);
        } else if frame["event"] == "done" {
            break told;
        } else if frame["event"] == "text" {
            assert_eq!(frame["text"], "other/stub-3 at default");
        }
    };
    assert_eq!(
        events,
        [
            json!({ "event": "thread-updated", "thread": thread, "model": "other/stub-3" }),
            json!({ "event": "thread-updated", "thread": thread, "effort": "default" }),
        ]
    );
    assert_eq!(stub_config_set(&stub_log, "effort"), ["high"]);

    // `set-model` on the thread's session: a Model with no levels leaves the
    // thread with no effort.
    let set = ask_lever(
        &mut peer,
        json!({ "op": "set-model", "thread": thread, "model": "stub/stub-2" }),
    );
    assert_eq!(set["reply"], "model-set", "{set}");
    assert_eq!(
        [read_frame(&mut peer).unwrap(), read_frame(&mut peer).unwrap()],
        [
            json!({ "event": "thread-updated", "thread": thread, "model": "stub/stub-2" }),
            json!({ "event": "thread-updated", "thread": thread, "effort": null }),
        ]
    );
    let record = resume(&mut peer, &thread);
    assert_eq!(record["model"], "stub/stub-2");
    assert!(record.get("effort").is_none(), "{record}");
    // Lever has that Model's levels from the session, with no session of
    // its own opened to read them.
    let news = |log: &Path| {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .matches("\"session/new\"")
            .count()
    };
    let before = news(&stub_log);
    let none = ask_lever(&mut peer, json!({ "op": "efforts", "model": "stub/stub-2" }));
    assert_eq!(none["efforts"], json!([]));
    assert_eq!(news(&stub_log), before);
    let (_, turn) = send(&mut peer, Some(&thread), "on two");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "stub/stub-2 at none");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_thread_with_no_session_given_another_model_has_no_effort_until_its_next_turn() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({}));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "hello", "cwd": "/tmp", "effort": "high" }),
    )
    .unwrap();
    let thread = read_until(&mut peer, |f| f.get("reply").is_some())["thread"]
        .as_str()
        .unwrap()
        .to_string();
    turn_events(&mut peer, &thread, 1);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    // Lever started again holds no session of the thread's.
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    resume(&mut peer, &thread);
    let set = ask_lever(
        &mut peer,
        json!({ "op": "set-model", "thread": thread, "model": "other/stub-3" }),
    );
    assert_eq!(set["reply"], "model-set", "{set}");
    assert_eq!(
        [read_frame(&mut peer).unwrap(), read_frame(&mut peer).unwrap()],
        [
            json!({ "event": "thread-updated", "thread": thread, "model": "other/stub-3" }),
            json!({ "event": "thread-updated", "thread": thread, "effort": null }),
        ]
    );
    let (_, turn) = send(&mut peer, Some(&thread), "on three");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(text_of(&events), "other/stub-3 at default");
    assert_eq!(resume(&mut peer, &thread)["effort"], "default");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_whose_effort_the_harness_refuses_fails_and_the_thread_goes_back_to_the_sessions() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({ "refuse_effort": "high" }));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "hello", "cwd": "/tmp", "effort": "high" }),
    )
    .unwrap();
    let thread = read_until(&mut peer, |f| f.get("reply").is_some())["thread"]
        .as_str()
        .unwrap()
        .to_string();
    let events = turn_events(&mut peer, &thread, 1);
    assert_eq!(events.last().unwrap()["event"], "failed", "{events:?}");
    assert!(stub_prompts(&stub_log).is_empty(), "the prompt was sent");
    assert_eq!(resume(&mut peer, &thread)["effort"], "default");

    // The next turn asks for nothing the harness refused.
    let (_, turn) = send(&mut peer, Some(&thread), "again");
    assert_eq!(
        text_of(&turn_events(&mut peer, &thread, turn)),
        "stub/stub-1 at default"
    );
    assert_eq!(stub_config_set(&stub_log, "effort"), ["high"]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_stopped_while_its_effort_is_set_holds_the_thread_until_the_harness_answers() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({ "set_config_delay_ms": 600 }));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    let (thread, _) = send(&mut peer, None, "hello");
    read_until(&mut peer, |f| f["event"] == "done");

    // The harness takes the effort at once, and answers late; the turn is
    // stopped before it answers.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "harder", "effort": "high" }),
    )
    .unwrap();
    wait_until("Lever asks for the effort", || {
        stub_config_set(&stub_log, "effort") == ["high"]
    });
    ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }));
    read_until(&mut peer, |f| f["event"] == "stopped");
    assert_eq!(stub_prompts(&stub_log).len(), 1, "the stopped turn's prompt was sent");

    // Until it answers, the thread takes no message and no other effort.
    let busy = ask_lever(&mut peer, json!({ "op": "send", "thread": thread, "text": "now" }));
    assert_eq!(busy["code"], "working", "{busy}");
    let busy = ask_lever(
        &mut peer,
        json!({ "op": "set-effort", "thread": thread, "effort": "low" }),
    );
    assert_eq!(busy["code"], "working", "{busy}");

    // Then the thread is free, and its next turn runs at the effort it has.
    wait_until("the thread is free", || {
        ask_lever(
            &mut peer,
            json!({ "op": "set-effort", "thread": thread, "effort": "high" }),
        )["reply"]
            == "effort-set"
    });
    let (_, turn) = send(&mut peer, Some(&thread), "now");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "stub/stub-1 at high");
    assert_eq!(stub_config_set(&stub_log, "effort"), ["high"]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn the_harness_s_commands_are_kept_for_each_thread_and_a_command_is_sent_as_a_message() {
    let dir = TempDir::new();
    let commands = json!([
        { "name": "review", "description": "review changes", "hint": "commit, branch, or pr" },
        { "name": "compact", "description": "Compact the session" }
    ]);
    let script = write_script(
        &dir.0,
        &json!({
            "commands": commands,
            "turns": [{ "steps": [{ "kind": "text", "text": "Reviewed." }] }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"],
        false
    );

    // Lever answers at once that it has none yet, starts the harness, and
    // tells who watches what the harness names.
    ask_lever(&mut peer, json!({ "op": "watch" }));
    let unknown = ask_lever(&mut peer, json!({ "op": "commands" }));
    assert_eq!(unknown, json!({ "reply": "commands", "known": false, "commands": [] }));
    let told = read_until(&mut peer, |f| f["event"] == "commands");
    assert_eq!(told, json!({ "event": "commands", "commands": commands }));
    let known = ask_lever(&mut peer, json!({ "op": "commands" }));
    assert_eq!(
        known,
        json!({ "reply": "commands", "known": true, "commands": commands })
    );

    // A command is a message that begins with `/` and its name, sent as it
    // is. The thread's session names its commands, and the thread has them.
    let (thread, turn) = send(&mut peer, None, "/review the last commit");
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(text_of(&events), "Reviewed.");
    assert_eq!(stub_prompts(&stub_log), ["/review the last commit"]);
    let of_thread = ask_lever(&mut peer, json!({ "op": "commands", "thread": thread }));
    assert_eq!(
        of_thread,
        json!({ "reply": "commands", "known": true, "thread": thread, "commands": commands })
    );
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["message"], "/review the last commit");

    let missing = ask_lever(&mut peer, json!({ "op": "commands", "thread": "t-none" }));
    assert_eq!(missing["code"], "no-thread", "{missing}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_threads_commands_come_as_an_event_to_who_watches_it() {
    let dir = TempDir::new();
    let commands = json!([{ "name": "review", "description": "review changes" }]);
    let script = write_script(
        &dir.0,
        &json!({
            "commands": commands,
            "turns": [{ "steps": [{ "kind": "pause_ms", "ms": 50 }, { "kind": "text", "text": "Hello." }] }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "hello");
    let told = read_until(&mut peer, |f| f["event"] == "commands");
    assert_eq!(
        told,
        json!({ "event": "commands", "thread": thread, "commands": commands })
    );
    // They are no part of the thread's record: nothing of them is a note.
    let events = turn_events(&mut peer, &thread, turn);
    assert!(events.iter().all(|e| e["event"] != "note"), "{events:?}");
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["notes"], json!([]));

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_record_says_when_each_turn_began_and_ended() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({ "turns": [
            { "steps": [{ "kind": "text", "text": "Hello." }] },
            { "steps": [{ "kind": "text", "text": "Wait" }], "stall": true }
        ]}),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let before = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let (thread, turn) = send(&mut peer, None, "hello");
    turn_events(&mut peer, &thread, turn);
    send(&mut peer, Some(&thread), "wait");
    read_until(&mut peer, |f| f["event"] == "text");
    let record = resume(&mut peer, &thread);
    let (first, second) = (&record["turns"][0], &record["turns"][1]);
    let began = first["started"].as_u64().unwrap();
    assert!(began >= before && began <= before + 60, "{first}");
    assert!(first["ended"].as_u64().unwrap() >= began, "{first}");
    assert!(second["started"].as_u64().unwrap() >= began, "{second}");
    assert!(second.get("ended").is_none(), "a working turn has not ended: {second}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_effort_answered_with_the_one_before_is_taken_and_the_update_after_says_so() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({ "effort_answer_lags": true }));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);

    // As OpenCode answers while another of its sessions works: with the
    // effort the session was at, and the new one in an update that follows.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": null, "text": "hello", "cwd": "/tmp", "model": "other/stub-3", "effort": "low" }),
    )
    .unwrap();
    let thread = read_until(&mut peer, |f| f.get("reply").is_some())["thread"]
        .as_str()
        .unwrap()
        .to_string();
    let events = turn_events(&mut peer, &thread, 1);
    assert_eq!(events.last().unwrap()["event"], "done", "{events:?}");
    assert_eq!(text_of(&events), "other/stub-3 at low");
    assert_eq!(
        stub_config_set(&stub_log, "effort"),
        ["low"],
        "the effort is asked for once"
    );
    let record = resume(&mut peer, &thread);
    assert_eq!(
        (&record["model"], &record["effort"]),
        (&json!("other/stub-3"), &json!("low"))
    );

    // The next turn has nothing to set.
    let (_, turn) = send(&mut peer, Some(&thread), "again");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn)), "other/stub-3 at low");
    assert_eq!(stub_config_set(&stub_log, "effort"), ["low"]);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_catalog_update_between_set_effort_and_send_keeps_the_picked_effort() {
    let dir = TempDir::new();
    let script = effort_script(&dir.0, json!({ "emit_catalog_update_on_new_session": true }));
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);

    // Turn 1 starts a live session at the default effort ("default").
    let (thread, turn) = send(&mut peer, None, "first");
    assert_eq!(
        text_of(&turn_events(&mut peer, &thread, turn)),
        "stub/stub-1 at default"
    );

    // Pick "high" while idle (saved for the next turn; live session is still "default").
    write_frame(
        &mut peer,
        &json!({ "op": "set-effort", "thread": thread, "effort": "high" }),
    )
    .unwrap();
    let reply = read_until(&mut peer, |f| f.get("reply").is_some());
    assert_eq!(reply["reply"], "effort-set", "{reply}");
    assert_eq!(reply["effort"], "high", "{reply}");

    // Trigger a catalog update on the idle session (via an `efforts` probe for
    // another Model, which opens a new probe session and emits a
    // `config_option_update` naming the live session's previous "default" effort).
    write_frame(&mut peer, &json!({ "op": "efforts", "model": "other/stub-3" })).unwrap();
    let efforts_reply = read_until(&mut peer, |f| f.get("reply").is_some());
    assert_eq!(efforts_reply["reply"], "efforts", "{efforts_reply}");

    // The saved effort must still be "high", and the next turn must apply it.
    let record = resume(&mut peer, &thread);
    assert_eq!(record["effort"], "high", "{record}");
    let (_, turn2) = send(&mut peer, Some(&thread), "second");
    assert_eq!(text_of(&turn_events(&mut peer, &thread, turn2)), "stub/stub-1 at high");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn commands_refuses_with_no_harness_and_does_not_reprobe_when_harness_names_none() {
    // Without a harness configured, `commands` fails immediately with `no-harness`.
    let dir_none = TempDir::new();
    let mut lever_none = RunningLever::start(&dir_none.0, my_uid(), my_uid());
    let mut peer_none = handshake(&lever_none.person);
    write_frame(&mut peer_none, &json!({ "op": "commands" })).unwrap();
    let reply = read_frame(&mut peer_none).unwrap();
    assert_eq!(reply["reply"], "error", "{reply}");
    assert_eq!(reply["code"], "no-harness", "{reply}");
    lever_none.signal(libc::SIGTERM);
    assert_eq!(lever_none.wait().code(), Some(0));

    // With a harness that never emits `available_commands_update`, repeated
    // `commands` polls must not spawn repeated probe sessions.
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{ "steps": [{ "kind": "text", "text": "ok" }] }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &[]);
    let mut lever = RunningLever::start_with_extra(&dir.0, my_uid(), my_uid(), &extra);
    let mut peer = handshake(&lever.person);

    // First `commands` triggers one probe; wait for models so the probe finishes.
    write_frame(&mut peer, &json!({ "op": "commands" })).unwrap();
    let first = read_frame(&mut peer).unwrap();
    assert_eq!(first["reply"], "commands", "{first}");
    write_frame(&mut peer, &json!({ "op": "models" })).unwrap();
    let models = read_frame(&mut peer).unwrap();
    assert_eq!(models["reply"], "models", "{models}");

    // Polling `commands` repeatedly afterwards opens no further probe sessions.
    for _ in 0..5 {
        write_frame(&mut peer, &json!({ "op": "commands" })).unwrap();
        let polled = read_frame(&mut peer).unwrap();
        assert_eq!(polled["reply"], "commands", "{polled}");
    }
    let log = std::fs::read_to_string(&stub_log).unwrap();
    let new_sessions = log
        .lines()
        .filter(|line| line.contains("\"method\":\"session/new\""))
        .count();
    assert_eq!(new_sessions, 1, "expected exactly one probe session, log:\n{log}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn restart_during_writes_preserves_every_thread_and_loads_sessions_without_replaying_work() {
    for kill_lever in [false, true] {
        let dir = TempDir::new();
        let uid = my_uid();
        // Keep producing database writes after the first text is acknowledged.
        let chunk = "recorded ".repeat(64);
        let steps: Vec<_> = (0..1000)
            .flat_map(|_| [json!({"kind":"text","text":chunk}), json!({"kind":"pause_ms","ms":2})])
            .collect();
        let script = write_script(
            &dir.0,
            &json!({
                "modes": true,
                "turns": [
                    {"match":"write continuously", "steps":steps},
                    {"match":"wait for permission", "steps":[
                        {"kind":"ask","id":"waiting","title":"echo allowed","raw_input":{"command":"echo allowed"}},
                        {"kind":"text","text":"Should never run"}
                    ]},
                    {"match":"finish", "steps":[{"kind":"text","text":"Kept answer"}]}
                ]
            }),
        );
        let rpc_log = dir.0.join("rpc.jsonl");
        let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log), None);
        let mut peer = handshake(&lever.person);
        let (finished, _) = send(&mut peer, None, "finish earlier work");
        read_until(&mut peer, |f| f["event"] == "done");
        let earlier = resume(&mut peer, &finished);
        assert_eq!(
            ask_lever(&mut peer, json!({"op":"archive", "thread":finished}))["reply"],
            "archived"
        );
        let (waiting, _) = send(&mut peer, None, "wait for permission");
        let request = read_until(&mut peer, |f| f["event"] == "request");
        let (writing, _) = send(&mut peer, None, "write continuously");
        read_until(&mut peer, |f| f["event"] == "text" && f["thread"] == writing);
        let children = std::fs::read_to_string(format!("/proc/{}/task/{}/children", lever.pid(), lever.pid())).unwrap();
        let children: Vec<u32> = children.split_whitespace().map(|p| p.parse().unwrap()).collect();
        assert_eq!(children.len(), 1, "one harness serves the threads");
        let harness_pid = children[0];
        let original_sessions: Vec<Value> = std::fs::read_to_string(&rpc_log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|rpc| rpc["method"] == "session/prompt")
            .map(|rpc| rpc["params"]["sessionId"].clone())
            .collect();
        assert_eq!(original_sessions.len(), 3);

        if kill_lever {
            lever.kill_and_wait();
        } else {
            // SAFETY: the harness is the direct child of this test's Lever.
            assert_eq!(unsafe { libc::kill(harness_pid as libc::pid_t, libc::SIGKILL) }, 0);
            read_until(&mut peer, |f| f["event"] == "interrupted" && f["thread"] == writing);
        }
        wait_until("the old harness to die", || !alive(harness_pid));
        if kill_lever {
            lever = RunningLever::start_with_stub(&dir.0, uid, &script, Some(&rpc_log), None);
        }
        let mut peer = handshake(&lever.person);
        assert_eq!(ask_lever(&mut peer, json!({"op":"status"}))["harness_started"], false);
        assert_eq!(resume(&mut peer, &finished)["turns"], earlier["turns"]);
        let record = resume(&mut peer, &waiting);
        assert_eq!(record["turns"].as_array().unwrap().len(), 1);
        assert_eq!(record["turns"][0]["state"], "interrupted");
        assert_eq!(record["turns"][0]["requests"][0]["state"], "withdrawn");
        assert_eq!(record["turns"][0]["answer"], "");
        let rejected = ask_lever(
            &mut peer,
            json!({"op":"answer","thread":waiting,"request":request["request"],"answer":"once"}),
        );
        assert_eq!(rejected["code"], "no-request");
        let record = resume(&mut peer, &writing);
        assert_eq!(record["turns"].as_array().unwrap().len(), 1);
        assert_eq!(record["turns"][0]["state"], "interrupted");
        assert!(record["turns"][0]["answer"].as_str().unwrap().starts_with(&chunk));
        let list = ask_lever(&mut peer, json!({"op":"list", "archived":true}));
        assert_eq!(list["threads"].as_array().unwrap().len(), 3);
        assert!(
            list["threads"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["working"] == false && t["waiting"] == false)
        );
        assert_eq!(
            ask_lever(&mut peer, json!({"op":"status"}))["harness_started"],
            false,
            "reading recovered threads must not restart or repeat their work"
        );

        for thread in [&waiting, &writing] {
            assert_eq!(
                send(&mut peer, Some(thread), "finish after restart"),
                (thread.clone(), 2)
            );
            read_until(&mut peer, |f| f["event"] == "done" && f["thread"] == *thread);
            let record = resume(&mut peer, thread);
            assert_eq!(record["turns"].as_array().unwrap().len(), 2);
            assert_eq!(record["turns"][1]["answer"], "Kept answer");
            assert_eq!(record["turns"][1]["notes"], json!([]), "no fresh-session fallback");
        }
        let loaded: Vec<Value> = std::fs::read_to_string(&rpc_log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|rpc| rpc["method"] == "session/load")
            .map(|rpc| rpc["params"]["sessionId"].clone())
            .collect();
        assert_eq!(
            loaded,
            original_sessions[1..],
            "each next message loads its original session"
        );
        lever.signal(libc::SIGTERM);
        assert_eq!(lever.wait().code(), Some(0));
    }
}

#[test]
fn models_refresh_broadcasts_catalog_live_and_persists_signed_in_choice() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");

    // Start with no Models available when free hosted Models are off.
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "no_models_when_free_disabled": true,
            "models": [{ "id": "stub/stub-1", "name": "Stub One" }],
            "default_model": "stub/stub-1",
            "modes": true,
            "turns": []
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut surface = handshake(&lever.surface);
    write_frame(&mut surface, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut surface).unwrap();
    assert_eq!(status["harness_started"], false);
    assert_eq!(status["free_hosted_models"], false);
    assert_eq!(status["models_chosen"], false);

    // Subscribe surface to watch events (as the shell does).
    write_frame(&mut surface, &json!({ "op": "watch" })).unwrap();
    assert_eq!(read_frame(&mut surface).unwrap()["reply"], "activity");

    // Person signs in to a first provider; `crowbar-agent login` sends `models` with `refresh: true`.
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "models": [{ "id": "acme/small", "name": "Small" }],
            "default_model": "acme/small",
            "modes": true,
            "turns": []
        }))
        .unwrap(),
    )
    .unwrap();

    let mut person = handshake(&lever.person);
    write_frame(&mut person, &json!({ "op": "models", "refresh": true })).unwrap();
    let reply1 = read_frame(&mut person).unwrap();
    assert_eq!(reply1["reply"], "models");
    assert_eq!(reply1["models"].as_array().unwrap().len(), 1);

    let event1 = read_until(&mut surface, |v| v["event"] == "models");
    assert_eq!(event1["models"][0]["id"], "acme/small");
    assert_eq!(event1["free_hosted_models"], false);

    // Person signs in to a second provider while the catalog is already non-empty.
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "models": [
                { "id": "acme/small", "name": "Small" },
                { "id": "other/large", "name": "Large" }
            ],
            "default_model": "acme/small",
            "modes": true,
            "turns": []
        }))
        .unwrap(),
    )
    .unwrap();
    write_frame(&mut person, &json!({ "op": "models", "refresh": true })).unwrap();
    let reply2 = read_frame(&mut person).unwrap();
    assert_eq!(reply2["models"].as_array().unwrap().len(), 2);

    let event2 = read_until(&mut surface, |v| v["event"] == "models");
    let ids: Vec<&str> = event2["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["acme/small", "other/large"]);

    // Restart Lever: idle status remembers `models_chosen: true` without starting the harness.
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    let mut restarted = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut surface2 = handshake(&restarted.surface);
    write_frame(&mut surface2, &json!({ "op": "status" })).unwrap();
    let status2 = read_frame(&mut surface2).unwrap();
    assert_eq!(status2["harness_started"], false);
    assert_eq!(status2["free_hosted_models"], false);
    assert_eq!(status2["models_chosen"], true);

    restarted.signal(libc::SIGTERM);
    assert_eq!(restarted.wait().code(), Some(0));
}

#[test]
fn unreachable_model_transport_retry_loop_fails_the_turn_with_reason() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    {
                        "kind": "unknown",
                        "update": {
                            "sessionUpdate": "session_info_update",
                            "_meta": {
                                "opencode/retry": {
                                    "attempt": 2,
                                    "error": {
                                        "type": "provider.transport",
                                        "message": "ECONNREFUSED: Unable to connect."
                                    }
                                }
                            }
                        }
                    },
                    {
                        "kind": "unknown",
                        "update": {
                            "sessionUpdate": "session_info_update",
                            "_meta": {
                                "opencode/retry": {
                                    "attempt": 3,
                                    "error": {
                                        "type": "provider.transport",
                                        "message": "ECONNREFUSED: Unable to connect."
                                    }
                                }
                            }
                        }
                    }
                ],
                "stall": true
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "hello unreachable");
    let failed = read_until(&mut peer, |v| v["event"] == "failed");
    assert_eq!(failed["thread"], thread);
    assert_eq!(failed["turn"], turn);
    assert!(
        failed["message"]
            .as_str()
            .unwrap_or("")
            .contains("ECONNREFUSED: Unable to connect."),
        "{failed}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn models_refresh_while_busy_defers_harness_restart_until_turn_finishes() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = dir.0.join("script.json");

    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "models": [{ "id": "acme/small", "name": "Small" }],
            "default_model": "acme/small",
            "modes": true,
            "turns": [{
                "steps": [{ "kind": "text", "text": "working..." }],
                "stall": true
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut surface = handshake(&lever.surface);
    write_frame(&mut surface, &json!({ "op": "watch" })).unwrap();
    assert_eq!(read_frame(&mut surface).unwrap()["reply"], "activity");

    let mut person = handshake(&lever.person);
    let (thread, _) = send(&mut person, None, "keep busy");
    read_until(&mut person, |v| v["event"] == "text");

    // Sign-in finishes while the thread is still working; credentials update on
    // disk and `crowbar-agent login` sends `models` with `refresh: true`.
    std::fs::write(
        &script,
        serde_json::to_string(&json!({
            "models": [
                { "id": "acme/small", "name": "Small" },
                { "id": "other/new", "name": "New" }
            ],
            "default_model": "acme/small",
            "modes": true,
            "turns": []
        }))
        .unwrap(),
    )
    .unwrap();
    write_frame(&mut person, &json!({ "op": "models", "refresh": true })).unwrap();
    let busy_reply = read_frame(&mut person).unwrap();
    assert_eq!(busy_reply["reply"], "models");

    // Stop the active turn: Lever restarts the harness and broadcasts the new catalog.
    write_frame(&mut person, &json!({ "op": "stop", "thread": thread })).unwrap();
    read_until(&mut person, |v| v["event"] == "stopped");

    let refreshed = read_until(&mut surface, |v| {
        v["event"] == "models"
            && v["models"]
                .as_array()
                .is_some_and(|arr| arr.iter().any(|m| m["id"] == "other/new"))
    });
    assert_eq!(refreshed["models"].as_array().unwrap().len(), 2);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn subagent_outlives_turn_raises_request_keeps_harness_alive_and_hides_harness_names() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [
                    {
                        "kind": "subagent_start",
                        "id": "ses_child_secret_99",
                        "title": "Check secrets",
                        "prompt": "Read .env and report"
                    },
                    { "kind": "text", "text": "Handed off to a subagent." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 550 },
                    {
                        "kind": "subagent_act",
                        "subagent": "ses_child_secret_99",
                        "id": "call_ls_1",
                        "tool_kind": "read",
                        "title": "List folder",
                        "status": "completed",
                        "content": ".env"
                    },
                    {
                        "kind": "subagent_ask",
                        "subagent": "ses_child_secret_99",
                        "id": "call_env_1",
                        "tool_kind": "read",
                        "title": "Read /tmp/.env",
                        "raw_input": { "path": "/tmp/.env" },
                        "output": "API_MODE=ok"
                    },
                    {
                        "kind": "subagent_text",
                        "subagent": "ses_child_secret_99",
                        "text": "Found API_MODE=ok."
                    },
                    {
                        "kind": "subagent_end",
                        "subagent": "ses_child_secret_99",
                        "status": "completed"
                    }
                ]
            }]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, Some(350));
    let mut watcher = handshake(&lever.surface);
    write_frame(&mut watcher, &json!({ "op": "watch" })).unwrap();
    assert_eq!(read_frame(&mut watcher).unwrap()["reply"], "activity");

    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Check the workspace");
    let mut seen_frames: Vec<Value> = Vec::new();
    let mut subagent_id: Option<String> = None;

    // Wait for the parent turn to end (`done`), recording every event.
    loop {
        let frame = read_frame(&mut peer).unwrap();
        seen_frames.push(frame.clone());
        if frame["event"] == "subagent" {
            subagent_id = frame["subagent"].as_str().map(str::to_string);
        }
        if frame["event"] == "done" && frame["thread"] == thread && frame["turn"] == turn {
            break;
        }
    }
    let sub_id = subagent_id.expect("subagent event before turn ended");
    assert!(sub_id.starts_with("sub-"), "expected Lever sub-... id, got {sub_id}");

    // Wait past the 350ms idle timeout while the subagent is still working.
    std::thread::sleep(Duration::from_millis(450));

    let mut inspector = handshake(&lever.person);
    write_frame(&mut inspector, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut inspector).unwrap();
    assert_eq!(
        status["harness_started"], true,
        "harness must stay running while subagent works past idle timeout"
    );

    write_frame(&mut inspector, &json!({ "op": "list" })).unwrap();
    let listed = read_frame(&mut inspector).unwrap();
    seen_frames.push(listed.clone());
    let threads = listed["threads"].as_array().unwrap();
    assert_eq!(
        threads.len(),
        1,
        "subagent session must never be listed as a thread: {listed}"
    );
    assert_eq!(threads[0]["thread"], thread);
    assert_eq!(threads[0]["working"], true);
    assert_eq!(threads[0]["state"], "working");
    assert_eq!(threads[0]["subagents"], 1);

    // Subagent raises a request after its parent turn has ended.
    let req = loop {
        let frame = read_frame(&mut peer).unwrap();
        seen_frames.push(frame.clone());
        if frame["event"] == "request" && frame["state"] == "waiting" {
            break frame;
        }
    };
    assert_eq!(req["thread"], thread);
    assert_eq!(req["turn"], turn);
    assert_eq!(req["subagent"], sub_id);
    assert_eq!(
        req["title"], "Read /tmp/.env",
        "child title prefix must be stripped: {req}"
    );
    let req_id = req["request"].as_str().unwrap().to_string();

    // Activity reports the thread as both working and waiting.
    let waiting_activity = read_until(&mut watcher, |v| {
        v["event"] == "activity" && v["waiting"].as_array().is_some_and(|w| w.iter().any(|t| t == &thread))
    });
    assert!(
        waiting_activity["working"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == &thread),
        "{waiting_activity}"
    );

    // Answer the subagent's request and wait for the subagent to finish.
    write_frame(
        &mut peer,
        &json!({ "op": "answer", "thread": thread, "request": req_id, "answer": "once" }),
    )
    .unwrap();
    let ans_reply = read_until(&mut peer, |v| v.get("reply").is_some());
    assert_eq!(ans_reply["reply"], "ok", "{ans_reply}");

    let sub_done = loop {
        let frame = read_frame(&mut peer).unwrap();
        seen_frames.push(frame.clone());
        if frame["event"] == "subagent" && frame["state"] == "done" {
            break frame;
        }
    };
    assert_eq!(sub_done["subagent"], sub_id);

    // Activity clears working and waiting once the subagent ends.
    let clear = read_until(&mut watcher, |v| {
        v["event"] == "activity"
            && v["working"].as_array().is_some_and(Vec::is_empty)
            && v["waiting"].as_array().is_some_and(Vec::is_empty)
    });
    assert_eq!(clear["working"], json!([]));

    // Resumed record keeps the subagent's title, messages, acts, state, and request link.
    let rec = resume(&mut inspector, &thread);
    seen_frames.push(rec.clone());
    assert_eq!(rec["working"], false);
    assert_eq!(
        rec["turns"][0]["acts"].as_array().unwrap().len(),
        0,
        "OpenCode subagent wrapper must not appear as parent act: {rec}"
    );
    let subs = rec["turns"][0]["subagents"].as_array().unwrap();
    assert_eq!(subs.len(), 1, "{rec}");
    let sub = &subs[0];
    assert_eq!(sub["subagent"], sub_id);
    assert_eq!(sub["title"], "Check secrets");
    assert_eq!(sub["state"], "done");
    assert_eq!(
        sub["messages"],
        json!([
            { "role": "prompt", "text": "Read .env and report" },
            { "role": "answer", "text": "Found API_MODE=ok." }
        ])
    );
    let sub_acts = sub["acts"].as_array().unwrap();
    assert_eq!(sub_acts.len(), 2, "{sub}");
    assert_eq!(sub_acts[0]["title"], "List folder");
    assert_eq!(sub_acts[1]["title"], "Read /tmp/.env");
    assert_eq!(sub_acts[1]["state"], "done");
    assert_eq!(rec["turns"][0]["requests"][0]["subagent"], sub_id);
    assert_eq!(rec["turns"][0]["requests"][0]["state"], "answered");
    assert_eq!(rec["turns"][0]["requests"][0]["answer"], "once");

    // No harness/OpenCode session or tool call identifier crosses Lever's sockets.
    for frame in &seen_frames {
        let raw = frame.to_string();
        for forbidden in [
            "ses_child_secret_99",
            "opencode",
            "childSessionId",
            "sessionID",
            "call_env_1",
            "call_ls_1",
        ] {
            assert!(
                !raw.contains(forbidden),
                "harness identifier {forbidden:?} leaked onto Lever socket: {raw}"
            );
        }
    }

    // Once the subagent ends, the idle timeout stops the harness as usual.
    wait_until("idle harness to stop after subagent ends", || {
        write_frame(&mut inspector, &json!({ "op": "status" })).unwrap();
        read_frame(&mut inspector).unwrap()["harness_started"] == false
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn acp_v1_subagent_works_and_stop_cancels_subagent_outliving_its_turn() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    {
                        "kind": "subagent_start",
                        "id": "acp-child-42",
                        "title": "Audit tree",
                        "prompt": "Check every file",
                        "protocol": "acp"
                    },
                    { "kind": "text", "text": "Running in background." }
                ],
                "after": [
                    {
                        "kind": "subagent_act",
                        "subagent": "acp-child-42",
                        "id": "child-act-1",
                        "tool_kind": "search",
                        "title": "Search repo",
                        "status": "completed",
                        "content": "3 matches",
                        "protocol": "acp"
                    },
                    {
                        "kind": "subagent_text",
                        "subagent": "acp-child-42",
                        "text": "Partial findings.",
                        "protocol": "acp"
                    },
                    {
                        "kind": "subagent_stall",
                        "subagent": "acp-child-42",
                        "protocol": "acp"
                    }
                ]
            }]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Audit");

    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    read_until(&mut peer, |v| v["event"] == "subagent-text");

    // Thread is still working while its ACP v1 subagent runs.
    let mut inspector = handshake(&lever.person);
    let rec = resume(&mut inspector, &thread);
    assert_eq!(rec["working"], true);
    assert_eq!(rec["turns"][0]["subagents"][0]["state"], "working");

    // Stop cancels the subagent and marks it stopped.
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread })).unwrap();
    let stopped_sub = read_until(&mut peer, |v| v["event"] == "subagent" && v["state"] == "stopped");
    assert_eq!(stopped_sub["title"], "Audit tree");

    let rec2 = resume(&mut inspector, &thread);
    assert_eq!(rec2["working"], false);
    assert_eq!(rec2["turns"][0]["subagents"][0]["state"], "stopped");
    assert_eq!(
        rec2["turns"][0]["subagents"][0]["messages"],
        json!([
            { "role": "prompt", "text": "Check every file" },
            { "role": "answer", "text": "Partial findings." }
        ])
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn lever_restart_marks_working_subagent_interrupted_and_withdraws_its_request() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    {
                        "kind": "subagent_start",
                        "id": "ses_sub_restart_1",
                        "title": "Long check"
                    },
                    { "kind": "text", "text": "Started." }
                ],
                "after": [
                    {
                        "kind": "subagent_ask",
                        "subagent": "ses_sub_restart_1",
                        "id": "call_wait_1",
                        "tool_kind": "execute",
                        "title": "uname -a",
                        "raw_input": { "command": "uname -a" }
                    },
                    {
                        "kind": "subagent_stall",
                        "subagent": "ses_sub_restart_1"
                    }
                ]
            }]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, _turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");
    drop(peer);

    // Stop Lever while the subagent is working and waiting on its request.
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    // Reopen Lever: subagent is interrupted, request is withdrawn, thread is idle.
    let mut lever2 = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer2 = handshake(&lever2.person);
    let rec = resume(&mut peer2, &thread);
    assert_eq!(rec["working"], false);
    assert_eq!(rec["turns"][0]["subagents"][0]["state"], "interrupted");
    assert_eq!(rec["turns"][0]["requests"][0]["state"], "withdrawn");

    lever2.signal(libc::SIGTERM);
    assert_eq!(lever2.wait().code(), Some(0));
}

#[test]
fn subagent_ending_withdraws_waiting_request_and_maps_cancelled_to_stopped() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    {
                        "kind": "subagent_start",
                        "id": "ses_sub_cancel_1",
                        "title": "Check tree"
                    },
                    { "kind": "text", "text": "Dispatched." }
                ],
                "after": [
                    {
                        "kind": "subagent_ask",
                        "subagent": "ses_sub_cancel_1",
                        "id": "call_cancel_1",
                        "tool_kind": "read",
                        "title": "Check tree: Read .env",
                        "raw_input": { "path": "/tmp/.env" },
                        "wait_for_answer": false
                    },
                    {
                        "kind": "pause_ms",
                        "ms": 150
                    },
                    {
                        "kind": "subagent_end",
                        "subagent": "ses_sub_cancel_1",
                        "status": "cancelled"
                    }
                ]
            }]
        }),
    );

    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, Some(300));
    let mut peer = handshake(&lever.person);
    let (thread, _turn) = send(&mut peer, None, "Go");

    let req = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");
    let req_id = req["request"].as_str().unwrap().to_string();

    // Subagent ends with status "cancelled" without cancelling the request first:
    // Lever withdraws the waiting request and maps cancelled -> stopped.
    let withdrawn = read_until(&mut peer, |v| {
        v["event"] == "request" && v["request"] == req_id && v["state"] == "withdrawn"
    });
    assert_eq!(withdrawn["state"], "withdrawn");
    let ended = read_until(&mut peer, |v| v["event"] == "subagent" && v["state"] != "working");
    assert_eq!(ended["state"], "stopped");

    let mut inspector = handshake(&lever.person);
    let rec = resume(&mut inspector, &thread);
    assert_eq!(rec["working"], false);
    assert_eq!(rec["turns"][0]["subagents"][0]["state"], "stopped");
    assert_eq!(rec["turns"][0]["requests"][0]["state"], "withdrawn");

    // Because the subagent ended and its request was withdrawn, the harness idles out.
    wait_until("idle harness stops after cancelled subagent", || {
        write_frame(&mut inspector, &json!({ "op": "status" })).unwrap();
        read_frame(&mut inspector).unwrap()["harness_started"] == false
    });

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

/// Lever's arguments for the stub harness with `script`, and `more`.
fn stub_extra(script: &Path, more: &[&str]) -> Vec<String> {
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

#[test]
fn what_the_harness_says_after_a_subagent_reports_is_a_turn_of_its_own_that_ends_when_quiet() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_wake_1", "title": "Look around", "prompt": "Look around" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 100 },
                    { "kind": "subagent_text", "subagent": "ses_wake_1", "text": "Found it." },
                    { "kind": "subagent_end", "subagent": "ses_wake_1", "status": "completed" },
                    // OpenCode wakes the parent session with the report, and
                    // its Model answers there.
                    { "kind": "text", "text": "The subagent reported." },
                    { "kind": "pause_ms", "ms": 900 },
                    { "kind": "text", "text": " One more thing." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look around");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);

    let woke = read_until(&mut peer, |v| v["event"] == "message");
    assert_eq!(woke["origin"], "harness");
    assert_eq!(woke["turn"], turn + 1, "the harness's turn is a new one: {woke}");
    assert_eq!(woke["text"], "");
    let text = read_until(&mut peer, |v| v["event"] == "text");
    assert_eq!(
        (text["turn"].clone(), text["text"].clone()),
        (json!(turn + 1), json!("The subagent reported."))
    );
    // No end comes from the harness: Lever ends the turn once it is quiet.
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn + 1);

    // What comes after reopens that turn, the thread's newest.
    let again = read_until(&mut peer, |v| v["event"] == "message");
    assert_eq!(
        (again["turn"].clone(), again["origin"].clone()),
        (json!(turn + 1), json!("harness"))
    );
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn + 1);

    let rec = resume(&mut peer, &thread);
    let turns = rec["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2, "{rec}");
    assert_eq!(
        turns[0]["answer"], "Handed off.",
        "the person's turn keeps only its own answer"
    );
    assert_eq!(turns[0].get("origin"), None);
    assert_eq!(turns[1]["origin"], "harness");
    assert_eq!(turns[1]["message"], "");
    assert_eq!(turns[1]["answer"], "The subagent reported. One more thing.");
    assert_eq!(turns[1]["state"], "done");
    assert_eq!(rec["working"], false);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_is_sent_while_a_subagent_works_and_takes_over_a_turn_the_harness_began() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "first",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_take_1", "title": "Dig", "prompt": "Dig in" },
                        { "kind": "text", "text": "Started." }
                    ],
                    "after": [
                        { "kind": "pause_ms", "ms": 800 },
                        { "kind": "subagent_end", "subagent": "ses_take_1", "status": "completed" },
                        { "kind": "text", "text": "Report in." }
                    ]
                },
                { "match": "second", "steps": [{ "kind": "text", "text": "Second answer." }] },
                { "match": "third", "steps": [{ "kind": "text", "text": "Third answer." }] }
            ]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "first");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);

    // The subagent works on, and the person's message is sent all the same.
    let (_, second) = send(&mut peer, Some(&thread), "second");
    assert_eq!(second, turn + 1);
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == second);

    // The subagent reports, the harness begins a turn, and the person's
    // next message takes it over.
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    assert_eq!(woke["turn"], second + 1);
    read_until(&mut peer, |v| v["event"] == "text" && v["text"] == "Report in.");
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "third", "cwd": "/tmp" }),
    )
    .unwrap();
    let mut ended_woken = false;
    let sent = read_until(&mut peer, |v| {
        if v["event"] == "done" && v["turn"] == second + 1 {
            ended_woken = true;
        }
        v.get("reply").is_some()
    });
    assert_eq!(sent["reply"], "sent", "{sent}");
    assert_eq!(sent["turn"], second + 2);
    if !ended_woken {
        read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == second + 1);
    }
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == second + 2);

    let rec = resume(&mut peer, &thread);
    let answers: Vec<&str> = rec["turns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["answer"].as_str().unwrap())
        .collect();
    assert_eq!(answers, ["Started.", "Second answer.", "Report in.", "Third answer."]);
    assert_eq!(rec["turns"][2]["origin"], "harness");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_first_heard_of_after_its_turn_was_stopped_is_stopped() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [{ "kind": "text", "text": "Working." }],
                "stall": true,
                // The harness says of the subagent only after it answered the Stop.
                "after": [
                    { "kind": "subagent_start", "id": "ses_late_1", "title": "Late", "prompt": "Go on" },
                    { "kind": "subagent_stall", "subagent": "ses_late_1" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "3000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Work");
    read_until(&mut peer, |v| v["event"] == "text");
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread })).unwrap();
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == turn);
    let ended = read_until(&mut peer, |v| v["event"] == "subagent" && v["state"] != "working");
    assert_eq!(ended["state"], "stopped", "{ended}");

    let mut inspector = handshake(&lever.person);
    write_frame(&mut inspector, &json!({ "op": "status" })).unwrap();
    let status = read_frame(&mut inspector).unwrap();
    assert_eq!(
        status["harness_started"], true,
        "the harness stopped the subagent when told: {status}"
    );
    let rec = resume(&mut inspector, &thread);
    assert_eq!(rec["working"], false);
    assert_eq!(rec["turns"][0]["subagents"][0]["state"], "stopped");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_that_launches_a_subagent_shows_it_and_names_no_session() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_outer_secret_7", "title": "Outer", "prompt": "Plan it" },
                    {
                        "kind": "subagent_start",
                        "id": "ses_inner_secret_8",
                        "title": "Inner",
                        "prompt": "Do the part",
                        "launched_by": "ses_outer_secret_7"
                    },
                    { "kind": "subagent_text", "subagent": "ses_inner_secret_8", "text": "Part done." },
                    { "kind": "subagent_end", "subagent": "ses_inner_secret_8", "status": "completed" },
                    { "kind": "subagent_end", "subagent": "ses_outer_secret_7", "status": "completed" },
                    { "kind": "text", "text": "All done." }
                ]
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Plan");
    let mut frames = Vec::new();
    read_until(&mut peer, |v| {
        frames.push(v.clone());
        v["event"] == "done" && v["turn"] == turn
    });
    let rec = resume(&mut peer, &thread);
    frames.push(rec.clone());
    for frame in &frames {
        let said = frame.to_string();
        assert!(
            !said.contains("secret") && !said.contains("sessionID"),
            "a harness session crossed Lever's socket: {said}"
        );
    }
    let subs = rec["turns"][0]["subagents"].as_array().unwrap();
    let titles: Vec<&str> = subs.iter().map(|s| s["title"].as_str().unwrap()).collect();
    assert_eq!(titles, ["Outer", "Inner"], "{rec}");
    assert_eq!(
        subs[0]["acts"],
        json!([]),
        "the launcher is no act of the outer subagent: {rec}"
    );
    assert_eq!(
        subs[1]["messages"],
        json!([
            { "role": "prompt", "text": "Do the part" },
            { "role": "answer", "text": "Part done." }
        ])
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_launch_the_harness_has_yet_to_report_lends_its_prompt_to_no_other_subagent() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_alpha", "title": "Alpha", "prompt": "Alpha prompt" },
                    {
                        "kind": "subagent_start",
                        "id": "ses_beta",
                        "title": "Beta",
                        "prompt": "Beta prompt",
                        "hold_launcher": true
                    },
                    // Alpha's status comes again while Beta's launch names no child.
                    { "kind": "subagent_end", "subagent": "ses_alpha", "status": "running" },
                    { "kind": "subagent_end", "subagent": "ses_alpha", "status": "completed" },
                    { "kind": "text", "text": "ok" }
                ]
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Launch two");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let rec = resume(&mut peer, &thread);
    let subs = rec["turns"][0]["subagents"].as_array().unwrap();
    assert_eq!(subs.len(), 1, "{rec}");
    assert_eq!(subs[0]["title"], "Alpha");
    assert_eq!(
        subs[0]["messages"],
        json!([{ "role": "prompt", "text": "Alpha prompt" }])
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_act_of_one_thread_is_not_taken_for_another_threads_launch_with_the_same_call_id() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "alpha",
                    "steps": [
                        {
                            "kind": "subagent_start",
                            "id": "ses_hold",
                            "title": "Held",
                            "prompt": "Wait",
                            "hold_launcher": true
                        },
                        { "kind": "text", "text": "Launching." }
                    ]
                },
                {
                    "match": "beta",
                    "steps": [
                        // A call ID is unique only within its session.
                        { "kind": "act", "id": "call_subagent_ses_hold", "tool_kind": "read", "title": "List files", "status": "pending" },
                        { "kind": "act_update", "id": "call_subagent_ses_hold", "status": "completed", "content": "a.txt" },
                        { "kind": "text", "text": "Listed." }
                    ]
                }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (_, first) = send(&mut peer, None, "alpha");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let (thread_b, turn_b) = send(&mut peer, None, "beta");
    read_until(&mut peer, |v| {
        v["event"] == "done" && v["thread"] == thread_b && v["turn"] == turn_b
    });
    let rec = resume(&mut peer, &thread_b);
    let acts = rec["turns"][0]["acts"].as_array().unwrap();
    assert_eq!(acts.len(), 1, "{rec}");
    assert_eq!(acts[0]["state"], "done", "{rec}");
    assert_eq!(acts[0]["detail"], "a.txt");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_id_from_a_harness_that_ended_routes_no_new_session_to_its_thread() {
    let dir = TempDir::new();
    let uid = my_uid();
    // Every stub gives the same IDs; the first stub's subagent has the ID the
    // second stub gives a new session.
    let script = write_script(
        &dir.0,
        &json!({
            "session_prefix": "s-",
            "load_session": false,
            "turns": [
                {
                    "match": "first",
                    "steps": [
                        { "kind": "subagent_start", "id": "s-2", "title": "Old helper", "prompt": "Help" },
                        { "kind": "subagent_end", "subagent": "s-2", "status": "completed" },
                        { "kind": "text", "text": "First." }
                    ]
                },
                { "match": "again", "steps": [{ "kind": "text", "text": "Again." }] },
                { "match": "other", "steps": [{ "kind": "text", "text": "Other thread's answer." }] }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, Some(200));
    let mut peer = handshake(&lever.person);
    let (thread_a, first) = send(&mut peer, None, "first");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let mut inspector = handshake(&lever.person);
    wait_until("the idle harness to stop", || {
        write_frame(&mut inspector, &json!({ "op": "status" })).unwrap();
        read_frame(&mut inspector).unwrap()["harness_started"] == false
    });

    // A new harness: thread A's session is s-1 again, and thread B's is s-2.
    let (_, again) = send(&mut peer, Some(&thread_a), "again");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == again);
    let (thread_b, other) = send(&mut peer, None, "other");
    read_until(&mut peer, |v| {
        v["event"] == "done" && v["thread"] == thread_b && v["turn"] == other
    });

    let rec_b = resume(&mut peer, &thread_b);
    assert_eq!(rec_b["turns"][0]["answer"], "Other thread's answer.", "{rec_b}");
    let rec_a = resume(&mut peer, &thread_a);
    let subs = rec_a["turns"][0]["subagents"].as_array().unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(
        subs[0]["messages"],
        json!([{ "role": "prompt", "text": "Help" }]),
        "{rec_a}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_launch_that_fails_ends_its_subagent_and_an_end_said_twice_is_said_once() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_fails_secret", "title": "Doomed", "prompt": "Try", "launch_fails": true },
                    { "kind": "subagent_start", "id": "ses_twice", "title": "Twice", "prompt": "Go" },
                    { "kind": "subagent_end", "subagent": "ses_twice", "status": "completed" },
                    { "kind": "subagent_end", "subagent": "ses_twice", "status": "completed" },
                    { "kind": "text", "text": "ok" }
                ]
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Launch");
    let mut frames = Vec::new();
    read_until(&mut peer, |v| {
        frames.push(v.clone());
        v["event"] == "done" && v["turn"] == turn
    });
    let rec = resume(&mut peer, &thread);
    let subs = rec["turns"][0]["subagents"].as_array().unwrap();
    assert_eq!(subs[0]["title"], "Doomed");
    assert_eq!(
        subs[0]["state"], "failed",
        "a launch that failed leaves no subagent working: {rec}"
    );
    assert!(!subs[0].to_string().contains("secret"), "{rec}");
    assert_eq!(rec["working"], false);
    let twice_done = frames
        .iter()
        .filter(|f| f["event"] == "subagent" && f["title"] == "Twice" && f["state"] == "done")
        .count();
    assert_eq!(twice_done, 1, "{frames:?}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_of_a_quiet_turn_the_harness_began_ends_it_and_no_message_takes_it_over_meanwhile() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_quiet_stop", "title": "Look", "prompt": "Look" },
                    { "kind": "subagent_end", "subagent": "ses_quiet_stop", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [{ "kind": "text", "text": "The subagent reported." }]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000", "--stop-timeout-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look");
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    assert_eq!(woke["turn"], turn + 1);
    read_until(&mut peer, |v| v["event"] == "text" && v["turn"] == turn + 1);

    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread })).unwrap();
    // A message while the Stop goes through does not take the turn over.
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "more", "cwd": "/tmp" }),
    )
    .unwrap();
    let refused = read_until(&mut peer, |v| v.get("reply").is_some() && v["reply"] != "ok");
    assert_eq!(
        (refused["reply"].clone(), refused["code"].clone()),
        (json!("error"), json!("working")),
        "{refused}"
    );
    let stopped = read_until(&mut peer, |v| {
        matches!(v["event"].as_str(), Some("stopped" | "done" | "interrupted"))
    });
    assert_eq!(
        (stopped["event"].clone(), stopped["turn"].clone()),
        (json!("stopped"), json!(turn + 1))
    );

    // The harness said nothing after the Stop: it is not stopped for it.
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    let status = read_until(&mut peer, |v| v["reply"] == "status");
    assert_eq!(status["harness_started"], true, "{status}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_a_subagent_first_asks_after_its_work_was_stopped_is_asked_of_nobody() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [{ "kind": "text", "text": "Working." }],
                "stall": true,
                "after": [{
                    "kind": "subagent_ask",
                    "subagent": "ses_asks_late",
                    "subagent_title": "Late",
                    "id": "call_late_read",
                    "tool_kind": "read",
                    "title": "Read /tmp/.env",
                    "raw_input": { "path": "/tmp/.env" },
                    "output": "SECRET=1"
                }]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "800"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Work");
    read_until(&mut peer, |v| v["event"] == "text");
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread })).unwrap();
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == turn);
    let mut asked = false;
    read_until(&mut peer, |v| {
        asked |= v["event"] == "request" && v["state"] == "waiting";
        v["event"] == "subagent" && v["state"] != "working"
    });
    assert!(!asked, "a stopped subagent's request was put to the person");
    let rec = resume(&mut peer, &thread);
    assert!(
        rec["turns"][0]["requests"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["state"] != "waiting"),
        "{rec}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_in_the_name_of_another_threads_subagent_is_put_to_nobody() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "alpha",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_owned_by_a", "title": "A's helper", "prompt": "Help A" },
                        { "kind": "text", "text": "A." }
                    ],
                    "after": [
                        { "kind": "pause_ms", "ms": 1500 },
                        { "kind": "subagent_end", "subagent": "ses_owned_by_a", "status": "completed" }
                    ]
                },
                {
                    "match": "beta",
                    "steps": [
                        {
                            "kind": "subagent_ask",
                            "subagent": "ses_owned_by_a",
                            "subagent_title": "A's helper",
                            "id": "call_cross",
                            "tool_kind": "read",
                            "title": "Read /tmp/a",
                            "raw_input": { "path": "/tmp/a" },
                            "output": "a"
                        },
                        { "kind": "text", "text": "B." }
                    ]
                }
            ]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (_, first) = send(&mut peer, None, "alpha");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let (thread_b, turn_b) = send(&mut peer, None, "beta");
    let mut asked = false;
    read_until(&mut peer, |v| {
        asked |= v["event"] == "request" && v["thread"] == thread_b;
        v["event"] == "done" && v["thread"] == thread_b && v["turn"] == turn_b
    });
    assert!(!asked, "thread B was asked in the name of thread A's subagent");
    let rec = resume(&mut peer, &thread_b);
    assert_eq!(rec["turns"][0]["requests"], json!([]), "{rec}");
    assert_eq!(rec["turns"][0]["subagents"], json!([]), "{rec}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_that_starts_and_asks_while_its_thread_is_deleted_goes_nowhere() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "delete_session": true,
            // The harness takes its time over the delete, and meanwhile a
            // subagent of the thread starts and asks.
            "delete_delay_ms": 1500,
            "turns": [{
                "steps": [{ "kind": "text", "text": "Done." }],
                "after": [
                    { "kind": "pause_ms", "ms": 300 },
                    { "kind": "subagent_start", "id": "ses_late", "title": "Late", "prompt": "Late work" },
                    {
                        "kind": "subagent_ask",
                        "subagent": "ses_late",
                        "id": "call_late",
                        "tool_kind": "read",
                        "title": "Read /tmp/late",
                        "raw_input": { "path": "/tmp/late" },
                        "output": "late"
                    },
                    { "kind": "subagent_end", "subagent": "ses_late", "status": "completed" }
                ]
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    write_frame(&mut peer, &json!({ "op": "delete", "thread": thread })).unwrap();
    let mut told = Vec::new();
    let reply = read_until(&mut peer, |v| {
        if v["event"] == "subagent" || v["event"] == "request" || v["event"] == "message" {
            told.push(v.clone());
        }
        v.get("reply").is_some()
    });
    assert_eq!(reply["reply"], "deleted", "{reply}");
    assert!(told.is_empty(), "a thread being deleted told of its harness: {told:?}");
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    let subagents: i64 = db
        .query_row("SELECT count(*) FROM subagents", [], |r| r.get(0))
        .unwrap();
    assert_eq!(subagents, 0);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_of_a_turn_the_harness_began_spares_an_older_turns_subagent_and_the_harness() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_quick", "title": "Quick", "prompt": "Quick work" },
                    { "kind": "subagent_start", "id": "ses_slow", "title": "Slow", "prompt": "Slow work" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_quick", "status": "completed" },
                    { "kind": "text", "text": "Quick reported." },
                    // The person stops the turn the harness began here. The
                    // slow subagent of the first turn asks while that Stop
                    // waits, and goes on past the stop timeout.
                    { "kind": "pause_ms", "ms": 600 },
                    {
                        "kind": "subagent_ask",
                        "subagent": "ses_slow",
                        "id": "call_slow",
                        "tool_kind": "read",
                        "title": "Read /tmp/slow",
                        "raw_input": { "path": "/tmp/slow" },
                        "output": "slow"
                    },
                    { "kind": "pause_ms", "ms": 600 },
                    { "kind": "subagent_end", "subagent": "ses_slow", "status": "completed" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "1200", "--wake-quiet-ms", "5000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let second = woke["turn"].as_u64().unwrap();
    assert_ne!(second, first);
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": second })).unwrap();
    read_until(&mut peer, |v| v["reply"] == "ok");
    let (mut asked, mut stopped) = (None, false);
    read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "subagent" && v["state"] == "interrupted"),
            "the older turn's subagent was cut off: {v}"
        );
        assert!(
            !(v["event"] == "request" && v["state"] == "withdrawn"),
            "the older turn's subagent's request was withdrawn: {v}"
        );
        if v["event"] == "request" && v["state"] == "waiting" {
            asked = Some(v.clone());
        }
        stopped |= v["event"] == "stopped" && v["turn"] == second;
        asked.is_some() && stopped
    });
    let asked = asked.unwrap();
    assert_eq!(asked["turn"], first, "{asked}");
    let request = asked["request"].as_str().unwrap();
    write_frame(
        &mut peer,
        &json!({ "op": "answer", "thread": thread, "request": request, "answer": "once" }),
    )
    .unwrap();
    let ended = read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Slow" && v["state"] != "working"
    });
    assert_eq!(ended["state"], "done", "{ended}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_whose_end_is_unsaved_stays_ended_though_it_says_more() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "delete_session": true,
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_said_more", "title": "Talker", "prompt": "Talk" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 400 },
                    { "kind": "subagent_end", "subagent": "ses_said_more", "status": "completed" },
                    // A last word of the subagent's, after the harness said
                    // it ended, while the database still refuses that end.
                    { "kind": "pause_ms", "ms": 200 },
                    { "kind": "subagent_text", "subagent": "ses_said_more", "text": "One more thing." }
                ]
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_subagent_end BEFORE UPDATE OF state ON subagents \
         WHEN NEW.state <> 'working' BEGIN SELECT RAISE(ABORT, 'refused'); END",
    )
    .unwrap();
    read_until(&mut peer, |v| v["event"] == "subagent" && v["state"] == "done");
    read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "subagent" && v["state"] == "working"),
            "the ended subagent went back to work: {v}"
        );
        v["event"] == "subagent-text"
    });
    db.execute_batch("DROP TRIGGER refuse_subagent_end").unwrap();
    // The end is saved on Lever's next try, and the thread is free.
    std::thread::sleep(Duration::from_millis(600));
    let state: String = db.query_row("SELECT state FROM subagents", [], |r| r.get(0)).unwrap();
    assert_eq!(state, "done");
    drop(db);
    write_frame(&mut peer, &json!({ "op": "list" })).unwrap();
    let listed = read_until(&mut peer, |v| v["reply"] == "list");
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");
    write_frame(&mut peer, &json!({ "op": "delete", "thread": thread })).unwrap();
    let reply = read_until(&mut peer, |v| v.get("reply").is_some());
    assert_eq!(reply["reply"], "deleted", "{reply}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_holds_against_a_turn_the_harness_begins_on_a_stopped_subagents_report() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_dig_1", "title": "Dig", "prompt": "Dig" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    // Stopped, the subagent reports, and the harness answers
                    // its report with another try.
                    { "kind": "subagent_stall", "subagent": "ses_dig_1" },
                    { "kind": "text", "text": "The subagent was cancelled; trying again." },
                    { "kind": "subagent_start", "id": "ses_dig_2", "title": "Dig again", "prompt": "Dig again" },
                    { "kind": "subagent_stall", "subagent": "ses_dig_2" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "400", "--wake-quiet-ms", "60000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    // As the drawer does, by the turn it shows.
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": first })).unwrap();
    read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Dig" && v["state"] == "stopped"
    });
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let second = woke["turn"].as_u64().unwrap();
    let again = read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Dig again" && v["state"] != "working"
    });
    assert_eq!(again["state"], "stopped", "{again}");
    let ended = read_until(&mut peer, |v| {
        v["turn"] == second && ["done", "stopped", "failed", "interrupted"].contains(&v["event"].as_str().unwrap_or(""))
    });
    assert_eq!(ended["event"], "stopped", "{ended}");
    let listed = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");
    // It said nothing once told to stop: the harness is spared.
    assert_eq!(ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"], true);

    // The person's next message lifts the Stop.
    let (_, next) = send(&mut peer, Some(&thread), "Go on");
    assert!(next > second);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_of_a_newer_turn_is_not_taken_on_by_an_older_turns_subagent_that_says_more() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_brief", "title": "Brief", "prompt": "Brief work" },
                    { "kind": "subagent_start", "id": "ses_long", "title": "Long", "prompt": "Long work" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_brief", "status": "completed" },
                    { "kind": "text", "text": "Brief reported." },
                    // While the Stop of the turn the harness began waits, the
                    // first turn's long subagent says it still runs, and it
                    // ends well past the stop timeout.
                    { "kind": "pause_ms", "ms": 300 },
                    { "kind": "subagent_end", "subagent": "ses_long", "status": "running" },
                    { "kind": "pause_ms", "ms": 1200 },
                    { "kind": "subagent_end", "subagent": "ses_long", "status": "completed" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "600", "--wake-quiet-ms", "60000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let second = woke["turn"].as_u64().unwrap();
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": second })).unwrap();
    let ended = read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Long" && v["state"] != "working"
    });
    assert_eq!(ended["state"], "done", "{ended}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_act_a_request_is_the_first_word_of_holds_a_turn_the_harness_began_while_it_runs() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_asker", "title": "Look", "prompt": "Look" },
                    { "kind": "subagent_end", "subagent": "ses_asker", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "text", "text": "Reported." },
                    {
                        "kind": "ask",
                        "id": "call_make",
                        "title": "make",
                        "raw_input": { "command": "make" },
                        "request_without_call": true,
                        "run_ms": 900
                    },
                    { "kind": "text", "text": " Done." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let second = woke["turn"].as_u64().unwrap();
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");
    write_frame(
        &mut peer,
        &json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    )
    .unwrap();
    let mut ended_early = false;
    read_until(&mut peer, |v| {
        ended_early |= v["event"] == "done" && v["turn"] == second;
        v["event"] == "text" && v["text"] == " Done."
    });
    assert!(!ended_early, "the turn ended while the act it was asked about ran");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == second);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_the_harness_began_waits_its_quiet_from_when_its_subagent_ended() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_first", "title": "First", "prompt": "One" },
                    { "kind": "subagent_end", "subagent": "ses_first", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "text", "text": "Reported." },
                    // The harness's turn starts another subagent, which runs past
                    // the quiet period, and answers its report soon after.
                    { "kind": "subagent_start", "id": "ses_second", "title": "Second", "prompt": "Two" },
                    { "kind": "pause_ms", "ms": 900 },
                    { "kind": "subagent_end", "subagent": "ses_second", "status": "completed" },
                    { "kind": "pause_ms", "ms": 150 },
                    { "kind": "text", "text": " And the second." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "500"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let mut ended_early = false;
    read_until(&mut peer, |v| {
        ended_early |= v["event"] == "done" && v["turn"] == turn + 1;
        v["event"] == "text" && v["text"] == " And the second."
    });
    assert!(
        !ended_early,
        "the turn ended before the harness answered its subagent's report"
    );
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn + 1);
    let rec = resume(&mut peer, &thread);
    assert_eq!(rec["turns"][1]["answer"], "Reported. And the second.");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

/// A script whose first turn hands off to a subagent that ends at once, so
/// the harness's report comes as a turn of its own, followed by `after`.
fn wake_script(dir: &Path, after: Value) -> PathBuf {
    write_script(
        dir,
        &json!({
            "turns": [
                {
                    "match": "Look",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_wake_helper", "title": "Look", "prompt": "Look" },
                        { "kind": "subagent_end", "subagent": "ses_wake_helper", "status": "completed" },
                        { "kind": "text", "text": "Handed off." }
                    ],
                    "after": after
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": "Next." }] }
            ]
        }),
    )
}

#[test]
fn a_subagent_stopped_with_a_quiet_harness_turn_spares_the_harness() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = wake_script(
        &dir.0,
        json!([
            { "kind": "text", "text": "Reported." },
            { "kind": "subagent_start", "id": "ses_lingers", "title": "Linger", "prompt": "Wait" },
            { "kind": "subagent_stall", "subagent": "ses_lingers" }
        ]),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000", "--stop-timeout-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look");
    read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Linger" && v["state"] == "working"
    });
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread })).unwrap();
    // The subagent stops when told, and the harness says nothing more.
    read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Linger" && v["state"] == "stopped"
    });
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == turn + 1);
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    let status = read_until(&mut peer, |v| v["reply"] == "status");
    assert_eq!(
        status["harness_started"], true,
        "the harness that stopped was stopped: {status}"
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_pending_act_of_a_harness_turn_holds_it_open() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = wake_script(
        &dir.0,
        json!([
            { "kind": "text", "text": "Reported." },
            { "kind": "act", "id": "call_slow", "tool_kind": "command", "title": "slow", "status": "pending" },
            { "kind": "pause_ms", "ms": 800 },
            { "kind": "act_update", "id": "call_slow", "status": "completed", "content": "ok" },
            { "kind": "text", "text": " Done." }
        ]),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "400"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (_, turn) = send(&mut peer, None, "Look");
    read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let mut ended_early = false;
    read_until(&mut peer, |v| {
        ended_early |= v["event"] == "done" && v["turn"] == turn + 1;
        v["event"] == "text" && v["text"] == " Done."
    });
    assert!(!ended_early, "the turn ended while its act was pending");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn + 1);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_turn_whose_end_or_answer_the_database_refuses_begins_no_turn_after_it_and_fails() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = wake_script(&dir.0, json!([{ "kind": "text", "text": "Reported." }]));
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look");
    read_until(&mut peer, |v| v["event"] == "text" && v["turn"] == turn + 1);

    // The end of the harness's turn is refused: the person's message waits.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_end BEFORE UPDATE OF state ON turns WHEN OLD.origin = 'harness' \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    )
    .unwrap();
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp" }),
    )
    .unwrap();
    let refused = read_until(&mut peer, |v| v.get("reply").is_some());
    assert_eq!(refused["code"], "storage-error", "{refused}");
    db.execute_batch("DROP TRIGGER refuse_end").unwrap();
    let turns: i64 = db
        .query_row("SELECT COUNT(*) FROM turns WHERE thread_id = ?1", [&thread], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(turns, 2, "no turn began after the one whose end is unsaved");
    drop(db);
    let (_, next) = send(&mut peer, Some(&thread), "Next");
    assert_eq!(next, turn + 2);
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == next);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_turn_whose_answer_the_database_refuses_ends_failed() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = wake_script(&dir.0, json!([{ "kind": "text", "text": "Reported." }]));
    let extra = stub_extra(&script, &["--wake-quiet-ms", "300"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_answer BEFORE UPDATE OF answer ON turns WHEN OLD.origin = 'harness' \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    )
    .unwrap();
    let mut peer = handshake(&lever.person);
    let (_, turn) = send(&mut peer, None, "Look");
    let end = read_until(&mut peer, |v| {
        v["turn"] == turn + 1 && matches!(v["event"].as_str(), Some("done" | "failed"))
    });
    assert_eq!(end["event"], "failed", "{end}");
    assert_eq!(end["message"], "Lever could not save all of this turn.");
    drop(db);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_a_new_harness_says_after_a_turn_opens_no_harness_turn_for_an_old_harnesss_subagent() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "first",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_old_helper", "title": "Old", "prompt": "Help" },
                        { "kind": "subagent_end", "subagent": "ses_old_helper", "status": "completed" },
                        { "kind": "text", "text": "First." }
                    ]
                },
                {
                    "match": "again",
                    "steps": [{ "kind": "text", "text": "Again." }],
                    "after": [{ "kind": "pause_ms", "ms": 100 }, { "kind": "text", "text": " Late." }]
                }
            ]
        }),
    );
    let extra = stub_extra(&script, &["--idle-timeout-ms", "200", "--wake-quiet-ms", "60000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "first");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let mut inspector = handshake(&lever.person);
    wait_until("the idle harness to stop", || {
        write_frame(&mut inspector, &json!({ "op": "status" })).unwrap();
        read_frame(&mut inspector).unwrap()["harness_started"] == false
    });
    let (_, again) = send(&mut peer, Some(&thread), "again");
    let mut woke = false;
    read_until(&mut peer, |v| {
        woke |= v["event"] == "message" && v["origin"] == "harness";
        v["event"] == "text" && v["text"] == " Late."
    });
    assert!(!woke, "a harness with no subagent of the thread began a turn");
    let rec = resume(&mut peer, &thread);
    assert_eq!(rec["turns"].as_array().unwrap().len(), 2, "{rec}");
    assert_eq!(rec["turns"][1]["turn"], again);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn an_act_a_stopped_harness_turn_closes_spares_the_harness() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = wake_script(
        &dir.0,
        json!([
            { "kind": "text", "text": "Reported." },
            { "kind": "act", "id": "call_cut", "tool_kind": "command", "title": "long", "status": "in_progress" },
            { "kind": "pause_ms", "ms": 400 },
            // The harness closes the act when it stops, and says nothing more.
            { "kind": "act_update", "id": "call_cut", "status": "failed", "content": "Cancelled" }
        ]),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000", "--stop-timeout-ms", "900"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look");
    read_until(&mut peer, |v| {
        v["event"] == "act" && v["turn"] == turn + 1 && v["state"] == "working"
    });
    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread })).unwrap();
    read_until(&mut peer, |v| {
        v["event"] == "act" && v["turn"] == turn + 1 && v["state"] == "failed"
    });
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == turn + 1);
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    let status = read_until(&mut peer, |v| v["reply"] == "status");
    assert_eq!(status["harness_started"], true, "{status}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_that_begins_a_turn_under_a_held_stop_asks_nothing() {
    let dir = TempDir::new();
    let script = wake_script(
        &dir.0,
        json!([
            { "kind": "pause_ms", "ms": 300 },
            { "kind": "ask", "id": "call_wake", "title": "make", "raw_input": { "command": "make" }, "request_without_call": true }
        ]),
    );
    let mut config: Value = serde_json::from_slice(&std::fs::read(&script).unwrap()).unwrap();
    config["modes"] = json!(true);
    let script = write_script(&dir.0, &config);
    let extra = stub_extra(&script, &["--stop-timeout-ms", "1000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    let got = read_until(&mut peer, |v| {
        (v["event"] == "request" && v["state"] == "waiting") || (v["event"] == "stopped" && v["turn"] == turn + 1)
    });
    assert_eq!(got["event"], "stopped", "a request slipped past Stop: {got}");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_the_harness_launches_under_a_held_stop_before_it_says_anything_is_stopped() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_first", "title": "First", "prompt": "First" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_stall", "subagent": "ses_first" },
                    { "kind": "subagent_start", "id": "ses_second", "title": "Second", "prompt": "Second" },
                    { "kind": "subagent_ask", "subagent": "ses_second", "id": "call_child", "tool_kind": "read",
                      "title": "Read /tmp/example", "raw_input": { "path": "/tmp/example" }, "output": "example" },
                    { "kind": "subagent_stall", "subagent": "ses_second" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "1000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": turn }))["reply"],
        "ok"
    );
    let got = read_until(&mut peer, |v| {
        (v["event"] == "request" && v["state"] == "waiting")
            || (v["event"] == "subagent" && v["title"] == "Second" && v["state"] == "stopped")
    });
    assert_eq!(got["event"], "subagent", "the retry slipped past Stop: {got}");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_nested_subagent_of_an_older_turns_subagent_is_not_stopped_with_a_newer_turn() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_first", "title": "First", "prompt": "First" },
                    { "kind": "subagent_start", "id": "ses_older", "title": "Older", "prompt": "Older" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_first", "status": "completed" },
                    { "kind": "text", "text": "First reported." },
                    { "kind": "pause_ms", "ms": 300 },
                    { "kind": "subagent_start", "id": "ses_nested", "title": "Nested", "prompt": "Nested", "launched_by": "ses_older" },
                    { "kind": "subagent_ask", "subagent": "ses_nested", "id": "call_child", "tool_kind": "read",
                      "title": "Read /tmp/example", "raw_input": { "path": "/tmp/example" }, "output": "example" },
                    { "kind": "subagent_stall", "subagent": "ses_nested" },
                    { "kind": "subagent_end", "subagent": "ses_older", "status": "completed" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "1000", "--wake-quiet-ms", "5000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let newer = woke["turn"].as_u64().unwrap();
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": newer }))["reply"],
        "ok"
    );
    let got = read_until(&mut peer, |v| {
        (v["event"] == "subagent" && v["title"] == "Nested" && v["state"] == "stopped")
            || (v["event"] == "request" && v["state"] == "waiting")
    });
    assert_eq!(
        got["event"], "request",
        "the newer Stop cancelled an older turn's nested subagent: {got}"
    );
    assert_eq!(got["turn"], turn);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_nested_subagent_told_of_before_its_launcher_names_it_keeps_its_launchers_turn() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_first", "title": "First", "prompt": "First" },
                    { "kind": "subagent_start", "id": "ses_older", "title": "Older", "prompt": "Older" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_first", "status": "completed" },
                    { "kind": "text", "text": "First reported." },
                    { "kind": "pause_ms", "ms": 300 },
                    // OpenCode tells of the child before the launcher's output
                    // names it.
                    {
                        "kind": "subagent_start", "id": "ses_nested", "title": "Nested", "prompt": "Nested",
                        "launched_by": "ses_older", "name_after_ms": 300
                    },
                    { "kind": "subagent_ask", "subagent": "ses_nested", "id": "call_child", "tool_kind": "read",
                      "title": "Read /tmp/example", "raw_input": { "path": "/tmp/example" }, "output": "example" },
                    { "kind": "subagent_stall", "subagent": "ses_nested" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "1500", "--wake-quiet-ms", "5000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let newer = woke["turn"].as_u64().unwrap();
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": newer }))["reply"],
        "ok"
    );
    let got = read_until(&mut peer, |v| {
        (v["event"] == "subagent" && v["title"] == "Nested" && v["state"] == "stopped")
            || (v["event"] == "request" && v["state"] == "waiting")
    });
    assert_eq!(
        got["event"], "request",
        "the newer Stop stopped an older turn's nested subagent: {got}"
    );
    assert_eq!(got["turn"], turn);
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_stop_of_each_turn_in_turn_holds_as_a_stop_of_the_thread_does() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_quick", "title": "Quick", "prompt": "Quick" },
                    { "kind": "subagent_start", "id": "ses_slow", "title": "Slow", "prompt": "Slow" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_quick", "status": "completed" },
                    { "kind": "text", "text": "Quick reported." },
                    // Stopped after the turn the harness began was, the slow
                    // subagent reports, and the harness answers well after
                    // that turn ended.
                    { "kind": "subagent_stall", "subagent": "ses_slow" },
                    { "kind": "pause_ms", "ms": 1200 },
                    { "kind": "text", "text": "Slow reported." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "600", "--wake-quiet-ms", "400"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let second = woke["turn"].as_u64().unwrap();
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": second }))["reply"],
        "ok"
    );
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": first }))["reply"],
        "ok"
    );
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == second);
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let third = woke["turn"].as_u64().unwrap();
    let ended = read_until(&mut peer, |v| {
        v["turn"] == third && ["done", "stopped", "failed", "interrupted"].contains(&v["event"].as_str().unwrap_or(""))
    });
    assert_eq!(ended["event"], "stopped", "{ended}");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_quiet_stopped_turn_ends_without_waiting_on_an_older_turns_subagent_stopped_after_it() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_quick", "title": "Quick", "prompt": "Quick" },
                    { "kind": "subagent_start", "id": "ses_slow", "title": "Slow", "prompt": "Slow" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_quick", "status": "completed" },
                    { "kind": "text", "text": "Quick reported." },
                    // Told to stop half a stop timeout after the turn the
                    // harness began, the slow subagent stops within its own.
                    { "kind": "subagent_stall", "subagent": "ses_slow", "linger_ms": 800 }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--stop-timeout-ms", "1000", "--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    let second = woke["turn"].as_u64().unwrap();
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": second }))["reply"],
        "ok"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread, "turn": first }))["reply"],
        "ok"
    );
    let ended = read_until(&mut peer, |v| {
        v["event"] == "subagent" && v["title"] == "Slow" && v["state"] != "working"
    });
    assert_eq!(ended["state"], "stopped", "{ended}");
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"],
        true,
        "the harness was stopped before the subagent had its time"
    );
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_of_a_subagent_that_has_ended_is_put_to_nobody() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_done", "title": "Done", "prompt": "Done" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_end", "subagent": "ses_done", "status": "completed" },
                    { "kind": "subagent_ask", "subagent": "ses_done", "id": "call_late", "tool_kind": "read",
                      "title": "Read /tmp/late", "raw_input": { "path": "/tmp/late" }, "output": "late" },
                    { "kind": "text", "text": "Fin." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    send(&mut peer, None, "Go");
    read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "request" && v["state"] == "waiting"),
            "a subagent that ended asked: {v}"
        );
        v["event"] == "text" && v["text"] == "Fin."
    });
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_mode_set_while_a_subagent_works_is_the_sessions_before_the_harness_goes_on() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_waits", "title": "Waits", "prompt": "Wait" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_ask", "subagent": "ses_waits", "id": "call_child", "tool_kind": "read",
                      "title": "Read /tmp/example", "raw_input": { "path": "/tmp/example" }, "output": "example" },
                    { "kind": "subagent_end", "subagent": "ses_waits", "status": "completed" },
                    // The harness answers the report with a command, in the
                    // mode its session is in.
                    { "kind": "text", "text": "Reported." },
                    { "kind": "ask", "id": "call_make", "title": "make", "raw_input": { "command": "make" } }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    write_frame(
        &mut peer,
        &json!({ "op": "send", "text": "Go", "cwd": "/tmp", "mode": "full-access" }),
    )
    .unwrap();
    let sent = read_until(&mut peer, |v| v.get("reply").is_some());
    let thread = sent["thread"].as_str().unwrap().to_string();
    let turn = sent["turn"].as_u64().unwrap();
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");

    // Ask first, while the subagent waits: the mode is the thread's once the
    // session has it.
    let set = ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": thread, "mode": "ask-first" }),
    );
    assert_eq!(set["reply"], "mode-set", "{set}");
    write_frame(
        &mut peer,
        &json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    )
    .unwrap();
    let make = read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "act" && v["title"] == "make" && v["state"] == "done"),
            "the command ran without asking: {v}"
        );
        v["event"] == "request" && v["state"] == "waiting" && v["title"] == "make"
    });
    assert_eq!(make["turn"], turn + 1, "{make}");
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][1]["mode"], "ask-first", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_taken_up_again_after_its_harness_restarted_is_not_shown_working_on() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "first",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_again", "title": "Again", "prompt": "Look" },
                        { "kind": "subagent_end", "subagent": "ses_again", "status": "completed" },
                        { "kind": "text", "text": "Looked." }
                    ]
                },
                {
                    "match": "again",
                    "steps": [
                        // As OpenCode 2.4.1 does after it restarted: only the
                        // launcher names the child it takes up again.
                        { "kind": "subagent_start", "id": "ses_again", "title": "Again", "prompt": "Look again", "unfollowed": true },
                        { "kind": "text", "text": "Took it up again." }
                    ],
                    // It goes on, and reports after the person stopped the
                    // thread, within the harness's idle timeout.
                    "after": [
                        { "kind": "pause_ms", "ms": 200 },
                        { "kind": "text", "text": "Late report." }
                    ]
                }
            ]
        }),
    );
    let extra = stub_extra(
        &script,
        &[
            "--idle-timeout-ms",
            "300",
            "--wake-quiet-ms",
            "60000",
            "--stop-timeout-ms",
            "400",
        ],
    );
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "first");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    let started = std::time::Instant::now();
    while ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == true {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the harness was never stopped as idle"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let (_, again) = send(&mut peer, Some(&thread), "again");
    // A subagent keeps the turn that first started it.
    let shown = read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "done" && v["turn"] == again),
            "the turn ended with no word of its subagent"
        );
        v["event"] == "subagent" && v["title"] == "Again"
    });
    assert_eq!(shown["state"], "failed", "{shown}");
    assert!(
        shown["error"].as_str().unwrap_or("").contains("cannot be followed"),
        "{shown}"
    );
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == again);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );

    // Its report is a turn the harness began, which the Stop stops.
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    assert_eq!(woke["turn"], again + 1, "{woke}");
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == again + 1);
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][1]["answer"], "Took it up again.", "{record}");
    let listed = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_a_held_stop_catches_after_a_turn_the_harness_began_ended_is_a_turn_of_its_own() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_told", "title": "Told", "prompt": "Tell" },
                    { "kind": "subagent_end", "subagent": "ses_told", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "text", "text": "Reported." },
                    // That turn goes quiet and ends; the person stops the
                    // thread; the harness says more.
                    { "kind": "pause_ms", "ms": 1200 },
                    { "kind": "text", "text": "More." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "300", "--stop-timeout-ms", "400"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, first) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first);
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == first + 1);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    assert_eq!(woke["turn"], first + 2, "{woke}");
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == first + 2);
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][1]["state"], "done", "{record}");
    assert_eq!(record["turns"][1]["answer"], "Reported.", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_request_after_a_stop_of_a_thread_at_rest_is_put_to_nobody() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [{ "kind": "text", "text": "Done." }],
                // The harness asks after its turn ended, and after the person
                // stopped the thread, with nothing of it working, so no
                // session was told to stop.
                "after": [
                    { "kind": "pause_ms", "ms": 600 },
                    { "kind": "ask", "id": "call_late", "title": "make", "raw_input": { "command": "make" } }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &[]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    std::thread::sleep(Duration::from_millis(1000));
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "request" && v["state"] == "waiting"),
            "a request came to the person under a held Stop: {v}"
        );
        v["reply"] == "status"
    });
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][0]["requests"], json!([]), "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_mode_the_record_does_not_take_is_taken_back_from_the_session() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_waits", "title": "Waits", "prompt": "Wait" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "subagent_ask", "subagent": "ses_waits", "id": "call_child", "tool_kind": "read",
                      "title": "Read /tmp/example", "raw_input": { "path": "/tmp/example" }, "output": "example" },
                    { "kind": "subagent_end", "subagent": "ses_waits", "status": "completed" },
                    { "kind": "text", "text": "Reported." },
                    { "kind": "ask", "id": "call_make", "title": "make", "raw_input": { "command": "make" } }
                ]
            }]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");

    // The harness takes Full access, and the record does not.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch(
        "CREATE TRIGGER refuse_mode BEFORE UPDATE OF mode ON threads \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    )
    .unwrap();
    let set = ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": thread, "mode": "full-access" }),
    );
    assert_eq!(set["code"], "storage-error", "{set}");
    wait_until("the session goes back to Ask first", || {
        stub_modes_set(&stub_log) == ["build", "crowbar-ask-first"]
    });
    db.execute_batch("DROP TRIGGER refuse_mode").unwrap();
    drop(db);

    // What the harness goes on with runs in Ask first.
    let answered = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    );
    assert_eq!(answered["reply"], "ok", "{answered}");
    let make = read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "act" && v["title"] == "make" && v["state"] == "done"),
            "the command ran without asking: {v}"
        );
        v["event"] == "request" && v["state"] == "waiting" && v["title"] == "make"
    });
    assert_eq!(make["turn"], turn + 1, "{make}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_mode_the_live_session_refuses_between_turns_leaves_the_thread_as_it_was() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "refuse_mode": "build",
            "turns": [{ "steps": [{ "kind": "text", "text": "Done." }] }]
        }),
    );
    let uid = my_uid();
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let refused = ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": thread, "mode": "full-access" }),
    );
    assert_eq!(refused["code"], "harness-error", "{refused}");
    let record = resume(&mut peer, &thread);
    assert_eq!(record["mode"], "ask-first", "{record}");
    // The thread is free again.
    let (_, next) = send(&mut peer, Some(&thread), "Again");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == next);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_turn_the_record_cannot_take_under_a_held_stop_stops_the_harness() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_ran", "title": "Ran", "prompt": "Run" },
                    { "kind": "subagent_end", "subagent": "ses_ran", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 800 },
                    { "kind": "text", "text": "Went on." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "INSERT ON turns");

    // The harness goes on, and the record takes no turn for it.
    wait_until("the harness is stopped", || {
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == false
    });
    db.execute_batch("DROP TRIGGER refused").unwrap();
    drop(db);
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"].as_array().unwrap().len(), 1, "{record}");
    assert_eq!(record["turns"][0]["answer"], "Handed off.", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_comes_in_one_read_after_the_harness_is_stopped_is_not_taken() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            // The session takes Full access, and refuses to go back to Ask
            // first, with a word that the subagent runs in the same read.
            "refuse_mode": "crowbar-ask-first",
            "refusal_rides_with": "ses_bg",
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_bg", "title": "Background", "prompt": "Work" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [{ "kind": "subagent_stall", "subagent": "ses_bg" }]
            }]
        }),
    );
    let extra = stub_extra(&script, &[]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    let mut working = false;
    read_until(&mut peer, |v| {
        working |= v["event"] == "subagent" && v["state"] == "working";
        v["event"] == "done" && v["turn"] == turn
    });
    assert!(working, "the subagent was never told of");
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF mode ON threads");
    let set = ask_lever(
        &mut peer,
        json!({ "op": "set-mode", "thread": thread, "mode": "full-access" }),
    );
    assert_eq!(set["code"], "storage-error", "{set}");
    wait_until("the harness is stopped", || {
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == false
    });
    db.execute_batch("DROP TRIGGER refused").unwrap();
    drop(db);
    let listed = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");
    let record = resume(&mut peer, &thread);
    assert_ne!(record["turns"][0]["subagents"][0]["state"], "working", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_the_harness_says_of_a_mode_that_settles_is_not_told_until_it_has() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "mode_update_first": true,
            "turns": [{ "steps": [{ "kind": "text", "text": "Done." }] }]
        }),
    );
    let uid = my_uid();
    let mut lever = RunningLever::start_with_stub(&dir.0, uid, &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF mode ON threads");
    write_frame(
        &mut peer,
        &json!({ "op": "set-mode", "thread": thread, "mode": "full-access" }),
    )
    .unwrap();
    let mut told = Vec::new();
    let reply = read_until(&mut peer, |v| {
        if v["event"] == "thread-updated" && v.get("mode").is_some() {
            told.push(v.clone());
        }
        v.get("reply").is_some()
    });
    assert_eq!(reply["code"], "storage-error", "{reply}");
    // The session goes back to Ask first, and still nobody is told of Full access.
    std::thread::sleep(Duration::from_millis(400));
    write_frame(&mut peer, &json!({ "op": "status" })).unwrap();
    read_until(&mut peer, |v| {
        if v["event"] == "thread-updated" && v.get("mode").is_some() {
            told.push(v.clone());
        }
        v["reply"] == "status"
    });
    assert!(
        told.iter().all(|v| v["mode"] == "ask-first"),
        "a mode the thread does not have was told: {told:?}"
    );
    db.execute_batch("DROP TRIGGER refused").unwrap();
    drop(db);
    assert_eq!(resume(&mut peer, &thread)["mode"], "ask-first");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_the_harness_begins_under_a_held_stop_while_the_last_end_is_unsaved_stops_the_harness() {
    let dir = TempDir::new();
    let script = wake_script(
        &dir.0,
        json!([
            { "kind": "text", "text": "Reported." },
            // Stopped, that turn ends and its end is not saved; the harness
            // goes on while it waits to be.
            { "kind": "pause_ms", "ms": 1500 },
            { "kind": "text", "text": "Again." }
        ]),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000", "--stop-timeout-ms", "300"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Look");
    read_until(&mut peer, |v| {
        v["event"] == "message" && v["origin"] == "harness" && v["turn"] == turn + 1
    });
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "UPDATE OF state ON turns");
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    read_until(&mut peer, |v| v["event"] == "stopped" && v["turn"] == turn + 1);
    wait_until("the harness is stopped", || {
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == false
    });
    db.execute_batch("DROP TRIGGER refused").unwrap();
    drop(db);
    std::thread::sleep(Duration::from_millis(400));
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"][1]["answer"], "Reported.", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_subagent_under_a_held_stop_that_the_record_cannot_take_stops_the_harness() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_first", "title": "First", "prompt": "First" },
                    { "kind": "subagent_end", "subagent": "ses_first", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                // After the Stop, the harness starts another, which goes on
                // saying nothing.
                "after": [
                    { "kind": "pause_ms", "ms": 800 },
                    { "kind": "subagent_start", "id": "ses_quiet", "title": "Quiet", "prompt": "Quietly" },
                    { "kind": "subagent_stall", "subagent": "ses_quiet" }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    refuse_writes(&db, "INSERT ON subagents");
    wait_until("the harness is stopped", || {
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == false
    });
    db.execute_batch("DROP TRIGGER refused").unwrap();
    drop(db);
    let listed = ask_lever(&mut peer, json!({ "op": "list" }));
    assert_eq!(listed["threads"][0]["working"], false, "{listed}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn what_the_harness_begins_under_a_held_stop_when_its_thread_cannot_be_read_stops_the_harness() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [{
                "steps": [
                    { "kind": "subagent_start", "id": "ses_ran", "title": "Ran", "prompt": "Run" },
                    { "kind": "subagent_end", "subagent": "ses_ran", "status": "completed" },
                    { "kind": "text", "text": "Handed off." }
                ],
                "after": [
                    { "kind": "pause_ms", "ms": 800 },
                    { "kind": "text", "text": "Went on." }
                ]
            }]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    assert_eq!(
        ask_lever(&mut peer, json!({ "op": "stop", "thread": thread }))["reply"],
        "ok"
    );
    // The record of threads cannot be read when the harness goes on.
    let db = rusqlite::Connection::open(lever.state.join("lever.sqlite")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.execute_batch("ALTER TABLE threads RENAME TO threads_away").unwrap();
    wait_until("the harness is stopped", || {
        ask_lever(&mut peer, json!({ "op": "status" }))["harness_started"] == false
    });
    db.execute_batch("ALTER TABLE threads_away RENAME TO threads").unwrap();
    drop(db);
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"].as_array().unwrap().len(), 1, "{record}");
    assert_eq!(record["turns"][0]["answer"], "Handed off.", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_waits_while_a_subagent_of_its_thread_asks() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "turns": [
                {
                    "match": "Go",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_asks", "title": "Asks", "prompt": "Ask" },
                        { "kind": "text", "text": "Handed off." }
                    ],
                    "after": [
                        { "kind": "subagent_ask", "subagent": "ses_asks", "id": "call_ask", "tool_kind": "read",
                          "title": "Read /tmp/asked", "raw_input": { "path": "/tmp/asked" }, "output": "asked" },
                        { "kind": "subagent_end", "subagent": "ses_asks", "status": "completed" }
                    ]
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": "Next." }] }
            ]
        }),
    );
    let extra = stub_extra(&script, &["--wake-quiet-ms", "300"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");

    // The message waits on the subagent's request.
    let held = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp" }),
    );
    assert_eq!(held["code"], "subagent-asks", "{held}");

    // Answered, the subagent finishes, and the message goes.
    let answered = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    );
    assert_eq!(answered["reply"], "ok", "{answered}");
    read_until(&mut peer, |v| v["event"] == "subagent" && v["state"] == "done");
    let (_, next) = send(&mut peer, Some(&thread), "Next");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == next);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_prompt_that_waited_on_a_mode_waits_too_while_a_subagent_asks() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            // The mode takes long enough for the subagent to ask meanwhile.
            "set_config_delay_ms": 600,
            "turns": [
                {
                    "match": "Go",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_mid", "title": "Mid", "prompt": "Ask" },
                        { "kind": "text", "text": "Handed off." }
                    ],
                    "after": [
                        { "kind": "pause_ms", "ms": 250 },
                        { "kind": "subagent_ask", "subagent": "ses_mid", "id": "call_mid", "tool_kind": "read",
                          "title": "Read /tmp/mid", "raw_input": { "path": "/tmp/mid" }, "output": "mid" },
                        { "kind": "subagent_end", "subagent": "ses_mid", "status": "completed" }
                    ]
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": "Next." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "300"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    let next = sent["turn"].as_u64().unwrap();
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");

    // The mode is set, and the prompt waits while the subagent asks.
    wait_until("the mode is set", || stub_modes_set(&stub_log) == ["build"]);
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(stub_prompts(&stub_log).len(), 1, "a prompt went while a subagent asked");

    let answered = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    );
    assert_eq!(answered["reply"], "ok", "{answered}");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == next);
    assert_eq!(stub_prompts(&stub_log).len(), 2);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_prompt_whose_mode_comes_with_a_subagents_request_waits_on_it() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "mode_reply_rides_with_ask": "ses_rides",
            "turns": [
                {
                    "match": "Go",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_rides", "title": "Rides", "prompt": "Ride" },
                        { "kind": "text", "text": "Handed off." }
                    ]
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": "Next." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "300"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(stub_prompts(&stub_log).len(), 1, "a prompt went while a subagent asked");

    let answered = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    );
    assert_eq!(answered["reply"], "ok", "{answered}");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == sent["turn"]);
    assert_eq!(stub_prompts(&stub_log).len(), 2);

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_held_for_a_subagent_ends_unsent_when_the_harness_goes_on_by_itself() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "set_config_delay_ms": 600,
            "turns": [
                {
                    "match": "Go",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_asker", "title": "Asker", "prompt": "Ask" },
                        { "kind": "subagent_start", "id": "ses_teller", "title": "Teller", "prompt": "Tell" },
                        { "kind": "text", "text": "Handed off." }
                    ],
                    "after": [
                        // One subagent asks while the next message's mode is
                        // set; the other reports, and the harness answers it.
                        { "kind": "pause_ms", "ms": 200 },
                        { "kind": "subagent_ask", "subagent": "ses_asker", "id": "call_asker", "tool_kind": "read",
                          "title": "Read /tmp/asker", "raw_input": { "path": "/tmp/asker" }, "output": "asker",
                          "wait_for_answer": false },
                        { "kind": "pause_ms", "ms": 900 },
                        { "kind": "subagent_end", "subagent": "ses_teller", "status": "completed" },
                        { "kind": "text", "text": "Background report." }
                    ]
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": "Next." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    let next = sent["turn"].as_u64().unwrap();
    let failed = read_until(&mut peer, |v| {
        v["turn"] == next && (v["event"] == "failed" || v["event"] == "done")
    });
    assert_eq!(failed["event"], "failed", "{failed}");
    assert!(
        failed["message"].as_str().unwrap_or("").contains("Send it again"),
        "{failed}"
    );
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    assert_eq!(woke["turn"], next + 1, "{woke}");
    read_until(&mut peer, |v| v["event"] == "text" && v["turn"] == next + 1);
    let record = resume(&mut peer, &thread);
    let turns = record["turns"].as_array().unwrap();
    assert_eq!(turns[1]["answer"], "", "{record}");
    assert_eq!(turns[2]["answer"], "Background report.", "{record}");
    assert_eq!(stub_prompts(&stub_log).len(), 1, "the held message was sent");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_waiting_on_its_mode_ends_unsent_when_the_harness_goes_on_by_itself() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            // The mode is answered only after the other subagent reports.
            "set_config_delay_ms": 1500,
            "turns": [
                {
                    "match": "Go",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_asker", "title": "Asker", "prompt": "Ask" },
                        { "kind": "subagent_start", "id": "ses_teller", "title": "Teller", "prompt": "Tell" },
                        { "kind": "text", "text": "Handed off." }
                    ],
                    "after": [
                        // One subagent asks while the next message's mode is
                        // set; the other reports, and the harness answers it.
                        { "kind": "pause_ms", "ms": 200 },
                        { "kind": "subagent_ask", "subagent": "ses_asker", "id": "call_asker", "tool_kind": "read",
                          "title": "Read /tmp/asker", "raw_input": { "path": "/tmp/asker" }, "output": "asker",
                          "wait_for_answer": false },
                        { "kind": "pause_ms", "ms": 900 },
                        { "kind": "subagent_end", "subagent": "ses_teller", "status": "completed" },
                        { "kind": "text", "text": "Background report." }
                    ]
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": "Next." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    let next = sent["turn"].as_u64().unwrap();
    let failed = read_until(&mut peer, |v| {
        v["turn"] == next && (v["event"] == "failed" || v["event"] == "done")
    });
    assert_eq!(failed["event"], "failed", "{failed}");
    assert!(
        failed["message"].as_str().unwrap_or("").contains("Send it again"),
        "{failed}"
    );
    let woke = read_until(&mut peer, |v| v["event"] == "message" && v["origin"] == "harness");
    assert_eq!(woke["turn"], next + 1, "{woke}");
    read_until(&mut peer, |v| v["event"] == "text" && v["turn"] == next + 1);
    let record = resume(&mut peer, &thread);
    let turns = record["turns"].as_array().unwrap();
    assert_eq!(turns[1]["answer"], "", "{record}");
    assert_eq!(turns[2]["answer"], "Background report.", "{record}");
    // The mode is answered after, and the message still does not go.
    std::thread::sleep(Duration::from_millis(1000));
    assert_eq!(stub_prompts(&stub_log).len(), 1, "the unsent message was sent");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_that_takes_over_with_a_mode_keeps_what_the_harness_says_as_its_mode_is_set() {
    let dir = TempDir::new();
    let uid = my_uid();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            // The answer to the mode comes with more of what the harness says.
            "mode_reply_rides_with_text": " Still reporting.",
            "turns": [
                {
                    "match": "first",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_take_mode", "title": "Dig", "prompt": "Dig in" },
                        { "kind": "subagent_end", "subagent": "ses_take_mode", "status": "completed" },
                        { "kind": "text", "text": "Started." }
                    ],
                    "after": [{ "kind": "text", "text": "Report in." }]
                },
                { "match": "next", "steps": [{ "kind": "text", "text": " Next answer." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "60000"]);
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "first");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    read_until(&mut peer, |v| v["event"] == "text" && v["text"] == "Report in.");

    // The person's message, with a mode, takes over the turn the harness began.
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "next", "cwd": "/tmp", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    let next = sent["turn"].as_u64().unwrap();
    let ended = read_until(&mut peer, |v| {
        v["turn"] == next && ["done", "failed", "stopped", "interrupted"].contains(&v["event"].as_str().unwrap_or(""))
    });
    assert_eq!(ended["event"], "done", "{ended}");
    let record = resume(&mut peer, &thread);
    let answer = record["turns"][2]["answer"].as_str().unwrap_or("");
    assert!(answer.contains("Still reporting."), "{record}");
    assert!(answer.contains("Next answer."), "{record}");
    assert_eq!(stub_prompts(&stub_log).len(), 2, "the message was not sent");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_report_the_persons_message_took_over_stays_whole_when_a_subagent_asks_after() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "modes": true,
            "set_config_delay_ms": 700,
            "turns": [
                {
                    "match": "Go",
                    "steps": [
                        { "kind": "subagent_start", "id": "ses_reporter", "title": "Reporter", "prompt": "Report" },
                        { "kind": "subagent_start", "id": "ses_later", "title": "Later", "prompt": "Ask later" },
                        { "kind": "text", "text": "Handed off." }
                    ],
                    "after": [
                        // While the next message's mode is set, one subagent
                        // reports and the harness begins to answer it; the
                        // other asks before the answer is done.
                        { "kind": "pause_ms", "ms": 150 },
                        { "kind": "subagent_end", "subagent": "ses_reporter", "status": "completed" },
                        { "kind": "text", "text": "Report part one." },
                        { "kind": "pause_ms", "ms": 150 },
                        { "kind": "subagent_ask", "subagent": "ses_later", "id": "call_later", "tool_kind": "read",
                          "title": "Read /tmp/later", "raw_input": { "path": "/tmp/later" }, "output": "later",
                          "wait_for_answer": false },
                        { "kind": "pause_ms", "ms": 100 },
                        { "kind": "text", "text": " Part two." }
                    ]
                },
                { "match": "Next", "steps": [{ "kind": "text", "text": " Next." }] }
            ]
        }),
    );
    let stub_log = dir.0.join("stub.log");
    let extra = stub_args(&script, &stub_log, &["--wake-quiet-ms", "60000"]);
    let uid = my_uid();
    let mut lever = RunningLever::start_with_extra(&dir.0, uid, uid, &extra);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "Go");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == turn);
    let sent = ask_lever(
        &mut peer,
        json!({ "op": "send", "thread": thread, "text": "Next", "cwd": "/tmp", "mode": "full-access" }),
    );
    assert_eq!(sent["reply"], "sent", "{sent}");
    let next = sent["turn"].as_u64().unwrap();
    let asked = read_until(&mut peer, |v| v["event"] == "request" && v["state"] == "waiting");
    read_until(&mut peer, |v| {
        assert!(
            !(v["event"] == "failed" && v["turn"] == next),
            "the message ended unsent after it took over the report: {v}"
        );
        v["event"] == "text" && v["text"] == " Part two."
    });
    let answered = ask_lever(
        &mut peer,
        json!({ "op": "answer", "thread": thread, "request": asked["request"], "answer": "once" }),
    );
    assert_eq!(answered["reply"], "ok", "{answered}");
    read_until(&mut peer, |v| v["event"] == "done" && v["turn"] == next);
    let record = resume(&mut peer, &thread);
    let turns = record["turns"].as_array().unwrap();
    assert_eq!(turns.len(), 2, "{record}");
    assert_eq!(turns[1]["answer"], "Report part one. Part two. Next.", "{record}");

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

/// The harness's JSON-RPC traffic as the stub logged what it was sent.
fn rpc_sent(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
fn a_message_sent_while_a_turn_works_is_steered_into_it_where_the_harness_can() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({
            "steering": true,
            "turns": [{
                "match": "long task",
                "steps": [
                    { "kind": "text", "text": "Looking. " },
                    { "kind": "await_steer", "text": "Took: {steer}" }
                ]
            }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);
    let mut other = handshake(&lever.person);

    // The adapter says its harness can steer a turn.
    write_frame(&mut other, &json!({ "op": "models" })).unwrap();
    let models = read_until(&mut other, |f| f.get("reply").is_some());
    assert_eq!(models["steering"], true, "{models}");

    let (thread, turn) = send(&mut peer, None, "Do the long task");
    read_until(&mut peer, |f| f["event"] == "text" && f["text"] == "Looking. ");

    // A second client sends while the turn works: it is taken into that turn.
    write_frame(
        &mut other,
        &json!({ "op": "send", "thread": thread, "text": "Also check the docs" }),
    )
    .unwrap();
    let steered = read_until(&mut other, |f| f.get("reply").is_some());
    assert_eq!(
        steered,
        json!({ "reply": "steered", "thread": thread, "turn": turn }),
        "{steered}"
    );

    // Who watches the thread sees where the message was taken, then the rest.
    let events = turn_events(&mut peer, &thread, turn);
    let at = events
        .iter()
        .position(|e| e["event"] == "steered")
        .expect("a steered event");
    assert_eq!(
        events[at],
        json!({ "event": "steered", "thread": thread, "turn": turn, "text": "Also check the docs" })
    );
    assert_eq!(text_of(&events[at..]), "Took: Also check the docs");
    assert_eq!(events.last().unwrap()["event"], "done");

    // The record keeps it in its turn, at the place in the answer it was taken.
    let record = resume(&mut peer, &thread);
    assert_eq!(record["turns"].as_array().unwrap().len(), 1, "{record}");
    let kept = &record["turns"][0];
    assert_eq!(kept["answer"], "Looking. Took: Also check the docs");
    assert_eq!(
        kept["steered"],
        json!([{ "text": "Also check the docs", "at": 9 }]),
        "{kept}"
    );

    // One prompt, and the message steered into it through the adapter.
    let sent = rpc_sent(&rpc_log);
    assert_eq!(sent.iter().filter(|m| m["method"] == "session/prompt").count(), 1);
    let steer = sent
        .iter()
        .find(|m| m["method"] == "_session/steering")
        .expect("Lever steers through OpenCode's extension");
    let prompt = sent.iter().find(|m| m["method"] == "session/prompt").unwrap();
    assert_eq!(steer["params"]["sessionId"], prompt["params"]["sessionId"]);
    assert_eq!(
        steer["params"]["prompt"],
        json!([{ "type": "text", "text": "Also check the docs" }])
    );

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_the_harness_does_not_take_into_its_turn_is_refused() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "steering": true,
            "steer_refused": true,
            "turns": [{ "match": "slow", "stall": true, "steps": [{ "kind": "text", "text": "Working. " }] }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "slow work");
    read_until(&mut peer, |f| f["event"] == "text");

    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "Change of plan" }),
    )
    .unwrap();
    let refused = read_until(&mut peer, |f| f.get("reply").is_some());
    assert_eq!(refused["reply"], "error", "{refused}");
    assert_eq!(refused["code"], "not-steered", "{refused}");
    assert!(
        refused["message"].as_str().unwrap().contains("Send it again"),
        "the person is told what to do: {refused}"
    );

    // The turn goes on, and nothing of the message is in the record.
    let record = resume(&mut peer, &thread);
    assert_eq!(record["working"], true, "{record}");
    assert_eq!(record["turns"].as_array().unwrap().len(), 1, "{record}");
    assert!(record["turns"][0].get("steered").is_none(), "{record}");

    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": turn })).unwrap();
    read_until(&mut peer, |f| f["event"] == "stopped");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_harness_that_cannot_steer_is_never_asked_to_and_the_message_is_refused() {
    let dir = TempDir::new();
    let rpc_log = dir.0.join("rpc.jsonl");
    let script = write_script(
        &dir.0,
        &json!({ "turns": [{ "match": "slow", "stall": true, "steps": [{ "kind": "text", "text": "Working. " }] }] }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, Some(&rpc_log), None);
    let mut peer = handshake(&lever.person);

    write_frame(&mut peer, &json!({ "op": "models" })).unwrap();
    let models = read_until(&mut peer, |f| f.get("reply").is_some());
    assert_eq!(models["steering"], false, "{models}");

    let (thread, turn) = send(&mut peer, None, "slow work");
    read_until(&mut peer, |f| f["event"] == "text");
    write_frame(
        &mut peer,
        &json!({ "op": "send", "thread": thread, "text": "Change of plan" }),
    )
    .unwrap();
    let refused = read_until(&mut peer, |f| f.get("reply").is_some());
    assert_eq!(refused["code"], "working", "{refused}");
    assert!(
        refused["message"].as_str().unwrap().contains("cannot take a message"),
        "the person is told why: {refused}"
    );
    assert!(
        !rpc_sent(&rpc_log).iter().any(|m| m["method"] == "_session/steering"),
        "a harness that does not say it can steer is never asked to"
    );

    write_frame(&mut peer, &json!({ "op": "stop", "thread": thread, "turn": turn })).unwrap();
    read_until(&mut peer, |f| f["event"] == "stopped");
    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}

#[test]
fn a_message_whose_harness_ends_before_it_answers_is_refused_and_its_turn_interrupted() {
    let dir = TempDir::new();
    let script = write_script(
        &dir.0,
        &json!({
            "steering": true,
            "steer_exits": true,
            "turns": [{ "match": "slow", "stall": true, "steps": [{ "kind": "text", "text": "Working. " }] }]
        }),
    );
    let mut lever = RunningLever::start_with_stub(&dir.0, my_uid(), &script, None, None);
    let mut peer = handshake(&lever.person);
    let mut other = handshake(&lever.person);
    let (thread, turn) = send(&mut peer, None, "slow work");
    read_until(&mut peer, |f| f["event"] == "text");

    write_frame(
        &mut other,
        &json!({ "op": "send", "thread": thread, "text": "Change of plan" }),
    )
    .unwrap();
    let refused = read_until(&mut other, |f| f.get("reply").is_some());
    assert_eq!(refused["code"], "not-steered", "{refused}");
    assert!(
        refused["message"].as_str().unwrap().contains("Send it again"),
        "{refused}"
    );
    let events = turn_events(&mut peer, &thread, turn);
    assert_eq!(events.last().unwrap()["event"], "interrupted");
    assert!(!events.iter().any(|e| e["event"] == "steered"));

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));
}
