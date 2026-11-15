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
#[serde(default)]
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
/// What the shell asks of the compositor it runs under: the windows and
/// workspaces, and the two focus requests.
pub trait Adapter: Send + Sync {
    fn snapshot(&self) -> Result<Snapshot>;
    fn focus_workspace(&self, id: i32) -> Result<()>;
    fn focus_window(&self, address: &str) -> Result<()>;
}

/// The Hyprland socket. The Fedora lab runs the shell under Hyprland, which
/// hands over no control channel, so the shell falls back to this there.
pub struct Hyprland {
    socket: PathBuf,
}

impl Hyprland {
    /// Where the running Hyprland session listens, from its environment.
    fn socket_path() -> Result<PathBuf> {
        let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .context("No Hyprland session signature")?;
        if sig.contains('/') || sig.contains("..") {
            bail!("Invalid Hyprland signature")
        }
        Ok(
            PathBuf::from(
                std::env::var_os("XDG_RUNTIME_DIR").context("Missing runtime directory")?,
            )
            .join("hypr")
            .join(sig)
            .join(".socket.sock"),
        )
    }

    fn request(&self, command: &str) -> Result<String> {
        let mut s = UnixStream::connect(&self.socket)?;
        s.set_read_timeout(Some(Duration::from_millis(500)))?;
        s.set_write_timeout(Some(Duration::from_millis(500)))?;
        s.write_all(command.as_bytes())?;
        let mut out = String::new();
        s.take(4 * 1024 * 1024).read_to_string(&mut out)?;
        Ok(out)
    }
}

impl Adapter for Hyprland {
    fn snapshot(&self) -> Result<Snapshot> {
        Ok(Snapshot {
            workspace: serde_json::from_str::<Workspace>(&self.request("j/activeworkspace")?)?.id,
            clients: serde_json::from_str(&self.request("j/clients")?)?,
            active: serde_json::from_str(&self.request("j/activewindow")?)?,
            connected: true,
        })
    }

    fn focus_workspace(&self, id: i32) -> Result<()> {
        self.request(&format!(
            "/eval hl.dispatch(hl.dsp.focus({{workspace=\"{id}\"}}))"
        ))
        .map(drop)
    }

    fn focus_window(&self, address: &str) -> Result<()> {
        self.request(&format!(
            "/eval hl.dispatch(hl.dsp.focus({{window=\"address:{address}\"}}))"
        ))
        .map(drop)
    }
}

/// The shell's end of the compositor's control channel.
///
/// The CrowbarOS compositor starts the shell and hands it a connected socket
/// in `CROWBAR_SHELL_CONTROL_FD`. The wire format is one JSON object per
/// line: a request gets one reply, and the compositor may send events at any
/// time. See `compositor/src/control.rs`.
pub struct Compositor {
    /// The read and write halves of the socket. `None` once an exchange has
    /// failed.
    lines: Mutex<Option<(BufReader<UnixStream>, UnixStream)>>,
}

impl Compositor {
    pub fn new(stream: UnixStream) -> Result<Self> {
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        stream.set_write_timeout(Some(Duration::from_millis(500)))?;
        Ok(Self {
            lines: Mutex::new(Some((BufReader::new(stream.try_clone()?), stream))),
        })
    }

    /// Adopts the socket the compositor handed over, named by the value of
    /// `CROWBAR_SHELL_CONTROL_FD`. The descriptor is made close-on-exec, so
    /// the programs the shell starts do not inherit it.
    fn inherited(descriptor: &str) -> Result<Self> {
        let fd: RawFd = descriptor
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
    ///
    /// Requests carry no id. If the exchange fails part-way, for example
    /// because the reply is late, the channel is closed for good: a late
    /// reply must never be read as the answer to a later request.
    fn request(&self, request: serde_json::Value) -> Result<serde_json::Value> {
        let mut lines = self
            .lines
            .lock()
            .map_err(|_| anyhow::anyhow!("Compositor channel failed"))?;
        let (mut reader, mut writer) = lines.take().context("The compositor channel is closed")?;
        let message = Self::exchange(&mut reader, &mut writer, request)?;
        *lines = Some((reader, writer));
        match message["reply"].as_str() {
            Some("error") => bail!("The compositor refused: {}", message["message"]),
            _ => Ok(message),
        }
    }

    fn exchange(
        reader: &mut BufReader<UnixStream>,
        writer: &mut UnixStream,
        request: serde_json::Value,
    ) -> Result<serde_json::Value> {
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
            if message["reply"].is_string() {
                return Ok(message);
            }
            bail!("Unexpected message from the compositor")
        }
    }
}

impl Adapter for Compositor {
    fn snapshot(&self) -> Result<Snapshot> {
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
        let reply: Reply =
            serde_json::from_value(self.request(serde_json::json!({"op": "snapshot"}))?)?;
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

    fn focus_workspace(&self, id: i32) -> Result<()> {
        self.request(serde_json::json!({"op": "focus_workspace", "id": id}))
            .map(drop)
    }

    fn focus_window(&self, address: &str) -> Result<()> {
        self.request(serde_json::json!({"op": "focus_window", "id": address}))
            .map(drop)
    }
}

/// Picks the adapter for the compositor the shell runs under.
///
/// A control channel means the CrowbarOS compositor started the shell, and
/// only that channel is used: if it is unusable, the shell does not fall back
/// to Hyprland. With no control channel, the shell uses the Hyprland socket.
fn adapter_for(
    control_descriptor: Option<String>,
    hyprland_socket: Result<PathBuf>,
) -> Result<Box<dyn Adapter>> {
    match control_descriptor {
        Some(descriptor) => Ok(Box::new(Compositor::inherited(&descriptor)?)),
        None => Ok(Box::new(Hyprland {
            socket: hyprland_socket.context("No compositor control channel")?,
        })),
    }
}

fn connection() -> Result<&'static dyn Adapter> {
    static CONNECTION: OnceLock<Option<Box<dyn Adapter>>> = OnceLock::new();
    CONNECTION
        .get_or_init(|| {
            adapter_for(
                std::env::var("CROWBAR_SHELL_CONTROL_FD").ok(),
                Hyprland::socket_path(),
            )
            .inspect_err(|e| log::warn!("Compositor: {e}"))
            .ok()
        })
        .as_deref()
        .context("No compositor to talk to")
}

/// Picks the compositor adapter and, under the CrowbarOS compositor, adopts
/// its control channel. Call before starting any thread or program, so that
/// no child inherits the descriptor.
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
            (
                r#"{"id":"0x1f","op":"focus_window"}"#,
                "{\"reply\":\"ok\"}\n",
            ),
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

    #[test]
    fn a_control_channel_descriptor_is_adopted_and_used() {
        use std::os::fd::IntoRawFd;
        let (ours, theirs) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            let mut reader = BufReader::new(theirs.try_clone().unwrap());
            let mut writer = theirs;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let sent: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(sent, serde_json::json!({"op": "focus_workspace", "id": 2}));
            writer.write_all(b"{\"reply\":\"ok\"}\n").unwrap();
        });
        let descriptor = ours.into_raw_fd().to_string();

        let adapter = adapter_for(
            Some(descriptor),
            Err(anyhow::anyhow!("No Hyprland session signature")),
        )
        .unwrap();
        adapter.focus_workspace(2).unwrap();

        peer.join().unwrap();
    }

    /// Plays Hyprland: each connection carries one command and gets one
    /// reply, then closes.
    fn scripted_hyprland(script: Vec<(&'static str, &'static str)>) -> (PathBuf, JoinHandle<()>) {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "crowbaros-shell-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hyprland.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let peer = std::thread::spawn(move || {
            for (expected, reply) in script {
                let (mut stream, _) = listener.accept().unwrap();
                let mut bytes = [0u8; 256];
                let n = stream.read(&mut bytes).unwrap();
                assert_eq!(std::str::from_utf8(&bytes[..n]).unwrap(), expected);
                stream.write_all(reply.as_bytes()).unwrap();
            }
            let _ = fs::remove_dir_all(dir);
        });
        (path, peer)
    }

    #[test]
    fn without_a_control_channel_the_shell_reads_its_snapshot_from_hyprland() {
        let (socket, peer) = scripted_hyprland(vec![
            ("j/activeworkspace", r#"{"id":2,"name":"2"}"#),
            (
                "j/clients",
                r#"[{"address":"0x1","class":"org.example.Terminal","title":"Terminal","workspace":{"id":1,"name":"1"}}]"#,
            ),
            (
                "j/activewindow",
                r#"{"address":"0x1","class":"org.example.Terminal","title":"Terminal","workspace":{"id":1,"name":"1"}}"#,
            ),
        ]);

        let adapter = adapter_for(None, Ok(socket)).unwrap();
        let snapshot = adapter.snapshot().unwrap();

        let terminal = Client {
            address: "0x1".into(),
            class: "org.example.Terminal".into(),
            title: "Terminal".into(),
            workspace: Workspace { id: 1 },
        };
        assert_eq!(
            snapshot,
            Snapshot {
                workspace: 2,
                clients: vec![terminal.clone()],
                active: terminal,
                connected: true,
            }
        );
        peer.join().unwrap();
    }

    /// Hyprland 0.57.2 was seen to list a window whose workspace has no `id`.
    #[test]
    fn a_hyprland_window_whose_workspace_has_no_id_does_not_lose_the_snapshot() {
        let (socket, peer) = scripted_hyprland(vec![
            ("j/activeworkspace", r#"{"id":1,"name":"1"}"#),
            (
                "j/clients",
                concat!(
                    r#"[{"address":"0x1","class":"org.example.Terminal","title":"Terminal","workspace":{"id":1,"name":"1"}},"#,
                    r#"{"address":"0x2","class":"org.example.Notes","title":"Notes","workspace":{"address":"0x0","type":"special","name":"notes"}}]"#
                ),
            ),
            (
                "j/activewindow",
                r#"{"address":"0x1","class":"org.example.Terminal","title":"Terminal","workspace":{"id":1,"name":"1"}}"#,
            ),
        ]);

        let snapshot = adapter_for(None, Ok(socket)).unwrap().snapshot().unwrap();

        assert_eq!(snapshot.workspace, 1);
        assert_eq!(
            snapshot.clients,
            vec![
                Client {
                    address: "0x1".into(),
                    class: "org.example.Terminal".into(),
                    title: "Terminal".into(),
                    workspace: Workspace { id: 1 },
                },
                Client {
                    address: "0x2".into(),
                    class: "org.example.Notes".into(),
                    title: "Notes".into(),
                    workspace: Workspace { id: 0 },
                },
            ]
        );
        peer.join().unwrap();
    }

    #[test]
    fn without_a_control_channel_focus_requests_go_to_hyprland() {
        let (socket, peer) = scripted_hyprland(vec![
            (r#"/eval hl.dispatch(hl.dsp.focus({workspace="3"}))"#, "ok"),
            (
                r#"/eval hl.dispatch(hl.dsp.focus({window="address:0x1f"}))"#,
                "ok",
            ),
        ]);

        let adapter = adapter_for(None, Ok(socket)).unwrap();
        adapter.focus_workspace(3).unwrap();
        adapter.focus_window("0x1f").unwrap();

        peer.join().unwrap();
    }

    /// A compositor that handed over a channel is the one to talk to. If the
    /// channel is unusable, the shell must not quietly talk to something else.
    #[test]
    fn a_broken_control_channel_does_not_fall_back_to_hyprland() {
        let (socket, _peer) = scripted_hyprland(vec![]);

        assert!(adapter_for(Some("not-a-descriptor".into()), Ok(socket)).is_err());
    }

    #[test]
    fn with_neither_a_control_channel_nor_hyprland_there_is_no_adapter() {
        assert!(adapter_for(None, Err(anyhow::anyhow!("No Hyprland session signature"))).is_err());
    }

    /// Requests carry no id, so a reply that arrives after the shell stopped
    /// waiting must not be read as the answer to the next request.
    #[test]
    fn after_a_late_reply_the_channel_fails_instead_of_answering_the_wrong_request() {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let peer = std::thread::spawn(move || {
            let mut reader = BufReader::new(theirs.try_clone().unwrap());
            let mut writer = theirs;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            std::thread::sleep(Duration::from_millis(800));
            // The shell may have closed the channel by now; either is fine.
            let _ = writer.write_all(b"{\"reply\":\"ok\"}\n");
            // Keep the peer's end open, so a next request could be written.
            writer
        });
        let compositor = Compositor::new(ours).unwrap();

        assert!(compositor.focus_workspace(1).is_err());
        let _peer_end = peer.join().unwrap();

        assert!(compositor.focus_window("0x1").is_err());
    }
}
