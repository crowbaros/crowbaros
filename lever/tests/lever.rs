use serde_json::{Value, json};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_crowbar-lever");
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
        let state = dir.join("state");
        let surface_dir = dir.join("surface-dir");
        let person_dir = dir.join("person-dir");
        std::fs::create_dir_all(&surface_dir).unwrap();
        let surface = surface_dir.join("surface");
        let person = person_dir.join("person");

        let child = Command::new(BIN)
            .args([
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
            ])
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
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let len = u32::from_be_bytes(header) as usize;
    assert!(len <= MAX_FRAME, "unexpectedly large frame: {len}");
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
                "harnesses": []
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

    lever.signal(libc::SIGTERM);
    assert_eq!(lever.wait().code(), Some(0));

    let log = lever.log_text();
    assert!(
        !log.contains("DO_NOT_LOG_THIS_SECRET"),
        "peer payload leaked into log: {log}"
    );
    assert!(!log.contains("unknown-secret-op"), "peer op leaked into log: {log}");
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
