// SPDX-License-Identifier: MIT
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::{FromRawFd, RawFd},
        unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
    },
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Mutex, OnceLock, mpsc::Sender},
    time::{Duration, Instant},
};
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: i32,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Client {
    pub address: String,
    pub class: String,
    pub title: String,
    pub workspace: Workspace,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub workspace: i32,
    pub clients: Vec<Client>,
    pub active: Client,
    pub connected: bool,
}
#[derive(Clone, Debug)]
pub enum Event {
    Toggle,
    Quit,
    Snapshot(Snapshot),
    Launch(Result<(), String>),
}
/// The shell's end of the compositor's control channel.
///
/// The CrowbarOS compositor starts the shell and hands it a connected socket
/// in `CROWBAR_SHELL_CONTROL_FD`. The wire format is one JSON object per
/// line: a request gets one reply, and the compositor may send events at any
/// time. See `compositor/src/control.rs`.
pub struct Compositor {
    lines: Mutex<(BufReader<UnixStream>, UnixStream)>,
}

impl Compositor {
    pub fn new(stream: UnixStream) -> Result<Self> {
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        stream.set_write_timeout(Some(Duration::from_millis(500)))?;
        Ok(Self {
            lines: Mutex::new((BufReader::new(stream.try_clone()?), stream)),
        })
    }

    /// Adopts the socket the compositor handed over. The descriptor is made
    /// close-on-exec, so the programs the shell starts do not inherit it.
    fn inherited() -> Result<Self> {
        let fd: RawFd = std::env::var("CROWBAR_SHELL_CONTROL_FD")
            .context("No compositor control channel")?
            .parse()
            .context("Invalid compositor control channel")?;
        // Safety: the compositor passed this descriptor to this process for
        // this purpose, and `connection()` adopts it once.
        let stream = unsafe {
            if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
                bail!("Invalid compositor control channel")
            }
            UnixStream::from_raw_fd(fd)
        };
        Self::new(stream)
    }

    /// Sends one request and returns its reply. Events that arrive first are
    /// logged and skipped.
    fn request(&self, request: serde_json::Value) -> Result<serde_json::Value> {
        let mut lines = self
            .lines
            .lock()
            .map_err(|_| anyhow::anyhow!("Compositor channel failed"))?;
        let (reader, writer) = &mut *lines;
        writer.write_all(format!("{request}\n").as_bytes())?;
        loop {
            let mut line = String::new();
            if reader.take(4 * 1024 * 1024).read_line(&mut line)? == 0 {
                bail!("The compositor closed the control channel")
            }
            let message: serde_json::Value = serde_json::from_str(&line)?;
            if let Some(event) = message.get("event") {
                log::info!("SHELL event=compositor-event name={event}");
                continue;
            }
            return match message["reply"].as_str() {
                Some("error") => bail!("The compositor refused: {}", message["message"]),
                Some(_) => Ok(message),
                None => bail!("Unexpected message from the compositor"),
            };
        }
    }

    pub fn snapshot(&self) -> Result<Snapshot> {
        #[derive(Deserialize)]
        struct Window {
            id: String,
            app_id: String,
            title: String,
            workspace: i32,
        }
        #[derive(Deserialize)]
        struct Reply {
            workspace: i32,
            active: Option<String>,
            windows: Vec<Window>,
        }
        let reply: Reply = serde_json::from_value(self.request(serde_json::json!({"op": "snapshot"}))?)?;
        let clients: Vec<Client> = reply
            .windows
            .into_iter()
            .map(|w| Client {
                address: w.id,
                class: w.app_id,
                title: w.title,
                workspace: Workspace { id: w.workspace },
            })
            .collect();
        let active = clients
            .iter()
            .find(|c| Some(&c.address) == reply.active.as_ref())
            .cloned()
            .unwrap_or_default();
        Ok(Snapshot {
            workspace: reply.workspace,
            clients,
            active,
            connected: true,
        })
    }

    pub fn focus_workspace(&self, id: i32) -> Result<()> {
        self.request(serde_json::json!({"op": "focus_workspace", "id": id}))
            .map(drop)
    }

    pub fn focus_window(&self, address: &str) -> Result<()> {
        self.request(serde_json::json!({"op": "focus_window", "id": address}))
            .map(drop)
    }
}

fn connection() -> Result<&'static Compositor> {
    static CONNECTION: OnceLock<Option<Compositor>> = OnceLock::new();
    CONNECTION
        .get_or_init(|| {
            Compositor::inherited()
                .inspect_err(|e| log::warn!("Compositor: {e}"))
                .ok()
        })
        .as_ref()
        .context("No compositor control channel")
}

/// Adopts the compositor's control channel. Call before starting any thread
/// or program, so that no child inherits the descriptor.
pub fn connect() {
    let _ = connection();
}

pub fn snapshot() -> Result<Snapshot> {
    connection()?.snapshot()
}
pub fn workspace(id: i32) {
    if id > 0 {
        std::thread::spawn(move || {
            if let Err(e) = connection().and_then(|c| c.focus_workspace(id)) {
                log::warn!("Workspace: {e}");
            }
        });
    }
}
pub fn focus(address: String) {
    if address.starts_with("0x") && address[2..].chars().all(|c| c.is_ascii_hexdigit()) {
        std::thread::spawn(move || {
            if let Err(e) = connection().and_then(|c| c.focus_window(&address)) {
                log::warn!("Focus: {e}");
            }
        });
    }
}
pub fn launch(path: PathBuf, tx: Sender<Event>) {
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            let mut child = Command::new("/usr/bin/systemd-run")
                .current_dir(
                    std::env::var_os("HOME").context("HOME is required to launch applications")?,
                )
                .args([
                    "--user",
                    "--scope",
                    "--collect",
                    "--quiet",
                    "--expand-environment=no",
                    "/usr/bin/gio",
                    "launch",
                ])
                .arg(&path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .context("Could not start the desktop launcher")?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(s) = child.try_wait()? {
                    if s.success() {
                        return Ok(());
                    }
                    bail!("The application could not start. Try again or check its installation.")
                }
                if Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("The application did not respond. Try again.")
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })();
        let _ = tx.send(Event::Launch(result.map_err(|e| e.to_string())));
    });
}
fn runtime() -> Result<PathBuf> {
    let dir =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is required")?)
            .join("crowbaros-shell");
    fs::create_dir_all(&dir)?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}
pub fn send(command: &str) -> Result<()> {
    let mut s = UnixStream::connect(runtime()?.join("control.sock"))
        .context("CrowbarOS shell is not running")?;
    s.set_write_timeout(Some(Duration::from_secs(1)))?;
    s.write_all(command.as_bytes())?;
    Ok(())
}
pub fn server(tx: Sender<Event>) -> Result<File> {
    let dir = runtime()?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("lock"))?;
    lock.try_lock()
        .context("CrowbarOS shell is already running")?;
    let path = dir.join("control.sock");
    if path.exists() {
        fs::remove_file(&path)?
    }
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_millis(250)));
            let mut bytes = [0u8; 32];
            if let Ok(n) = s.read(&mut bytes) {
                let event = match &bytes[..n] {
                    b"launcher" => Event::Toggle,
                    b"quit" => Event::Quit,
                    _ => continue,
                };
                let _ = tx.send(event);
            }
        }
    });
    Ok(lock)
}
pub fn start_session() -> Result<()> {
    let status = Command::new("systemctl")
        .args([
            "--user",
            "import-environment",
            "WAYLAND_DISPLAY",
            "HYPRLAND_INSTANCE_SIGNATURE",
            "XDG_CURRENT_DESKTOP",
            "XDG_SESSION_TYPE",
        ])
        .status()?;
    if !status.success() {
        bail!("Unable to import Wayland session")
    };
    let s = Command::new("systemctl")
        .args(["--user", "restart", "crowbaros-shell.service"])
        .status()?;
    if !s.success() {
        bail!("Unable to start CrowbarOS shell service")
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::JoinHandle;

    /// Plays the compositor: for each expected request, checks it and sends
    /// the scripted lines back.
    fn scripted(script: Vec<(&'static str, &'static str)>) -> (Compositor, JoinHandle<()>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            let mut reader = BufReader::new(theirs.try_clone().unwrap());
            let mut writer = theirs;
            for (expected, reply) in script {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let sent: serde_json::Value = serde_json::from_str(&line).unwrap();
                let expected: serde_json::Value = serde_json::from_str(expected).unwrap();
                assert_eq!(sent, expected);
                writer.write_all(reply.as_bytes()).unwrap();
            }
        });
        (Compositor::new(ours).unwrap(), peer)
    }

    #[test]
    fn a_snapshot_reply_becomes_the_shells_snapshot() {
        let (compositor, peer) = scripted(vec![(
            r#"{"op":"snapshot"}"#,
            concat!(
                r#"{"reply":"snapshot","workspace":2,"active":"0x2","windows":["#,
                r#"{"id":"0x1","app_id":"org.example.Terminal","title":"Terminal","workspace":1},"#,
                r#"{"id":"0x2","app_id":"com.crowbaros.StudioSettings","title":"Studio \"Settings\"","workspace":2}]}"#,
                "\n"
            ),
        )]);

        let snapshot = compositor.snapshot().unwrap();

        let studio = Client {
            address: "0x2".into(),
            class: "com.crowbaros.StudioSettings".into(),
            title: "Studio \"Settings\"".into(),
            workspace: Workspace { id: 2 },
        };
        assert_eq!(
            snapshot,
            Snapshot {
                workspace: 2,
                clients: vec![
                    Client {
                        address: "0x1".into(),
                        class: "org.example.Terminal".into(),
                        title: "Terminal".into(),
                        workspace: Workspace { id: 1 },
                    },
                    studio.clone(),
                ],
                active: studio,
                connected: true,
            }
        );
        peer.join().unwrap();
    }

    #[test]
    fn focus_requests_use_the_compositors_wire_format() {
        let (compositor, peer) = scripted(vec![
            (r#"{"id":3,"op":"focus_workspace"}"#, "{\"reply\":\"ok\"}\n"),
            (r#"{"id":"0x1f","op":"focus_window"}"#, "{\"reply\":\"ok\"}\n"),
        ]);

        compositor.focus_workspace(3).unwrap();
        compositor.focus_window("0x1f").unwrap();

        peer.join().unwrap();
    }

    #[test]
    fn an_event_that_arrives_before_the_reply_is_skipped() {
        let (compositor, peer) = scripted(vec![(
            r#"{"id":1,"op":"focus_workspace"}"#,
            "{\"event\":\"confirm\"}\n{\"reply\":\"ok\"}\n",
        )]);

        compositor.focus_workspace(1).unwrap();

        peer.join().unwrap();
    }

    #[test]
    fn a_refusal_from_the_compositor_is_an_error() {
        let (compositor, peer) = scripted(vec![(
            r#"{"id":"0x99","op":"focus_window"}"#,
            "{\"reply\":\"error\",\"message\":\"no such window\"}\n",
        )]);

        let error = compositor.focus_window("0x99").unwrap_err();

        assert!(error.to_string().contains("no such window"), "{error}");
        peer.join().unwrap();
    }
}
