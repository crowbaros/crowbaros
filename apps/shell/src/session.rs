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
    /// It opened without the keyboard and has not had it since: the bar
    /// shows it wants attention. Only the CrowbarOS compositor says so.
    pub attention: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Snapshot {
    pub workspace: i32,
    pub clients: Vec<Client>,
    pub active: Client,
    pub connected: bool,
    /// The compositor locked the session: the lock screen shows.
    pub locked: bool,
    /// The confirm chord opened the lock screen's unlock entry.
    pub unlock_entry: bool,
}
#[derive(Clone, Debug)]
pub enum Event {
    Toggle,
    /// The compositor locked the session, or unlocked it.
    Lock(bool),
    /// The confirm chord opened the lock screen's unlock entry.
    UnlockEntry,
    Quit,
    Snapshot(Snapshot),
    Launch(Result<(), String>),
    /// The unlock helper's answer to the person's password: `Ok` once the
    /// compositor unlocked, or why not.
    Unlock(Result<(), String>),
}
/// What the shell asks of the compositor it runs under: the windows and
/// workspaces, and the two focus requests.
pub trait Adapter: Send + Sync {
    fn snapshot(&self) -> Result<Snapshot>;
    fn focus_workspace(&self, id: i32) -> Result<()>;
    fn focus_window(&self, address: &str) -> Result<()>;
    /// Says the person is launching an app, whose first window then takes
    /// the keyboard. A compositor that does not ask for it is not told.
    fn launching(&self) -> Result<()> {
        Ok(())
    }
    /// Reads the events the compositor sent while no request was waiting.
    fn pump(&self) {}
    /// Asks the compositor to lock the session, or to unlock it.
    fn lock(&self, _locked: bool) -> Result<()> {
        bail!("This compositor does not lock")
    }
}

/// Where the compositor's events go: the shell's own event channel.
static EVENTS: OnceLock<Sender<Event>> = OnceLock::new();

/// Sends the compositor's events to the shell's event channel from now on.
pub fn deliver_events_to(tx: Sender<Event>) {
    let _ = EVENTS.set(tx);
}

/// One event the compositor sent: `launcher` opens or closes the launcher,
/// as Super tapped alone asks (Fulcrum's proposal, ADR 0020's keys).
fn compositor_event(event: &serde_json::Value) {
    log::info!("SHELL event=compositor-event name={event}");
    let Some(tx) = EVENTS.get() else {
        return;
    };
    let _ = match event.as_str() {
        Some("launcher") => tx.send(Event::Toggle),
        Some("locked") => tx.send(Event::Lock(true)),
        Some("unlocked") => tx.send(Event::Lock(false)),
        Some("unlock_entry") => tx.send(Event::UnlockEntry),
        _ => Ok(()),
    };
}

/// The lines the compositor sends, read whole: what has arrived of a line
/// waits here for the rest, whichever of a request or the pump reads next.
struct Lines {
    stream: UnixStream,
    pending: Vec<u8>,
}

impl Lines {
    /// The longest line read.
    const MAX: usize = 4 * 1024 * 1024;

    fn take(&mut self) -> Option<String> {
        let end = self.pending.iter().position(|byte| *byte == b'\n')?;
        let line: Vec<u8> = self.pending.drain(..=end).collect();
        Some(String::from_utf8_lossy(&line).into_owned())
    }

    /// The next line, as long as the stream's read timeout waits for it.
    fn next(&mut self) -> Result<String> {
        loop {
            if let Some(line) = self.take() {
                return Ok(line);
            }
            if self.pending.len() > Self::MAX {
                bail!("A line from the compositor is too long")
            }
            let mut chunk = [0u8; 4096];
            let read = self.stream.read(&mut chunk)?;
            if read == 0 {
                bail!("The compositor closed the control channel")
            }
            self.pending.extend_from_slice(&chunk[..read]);
        }
    }

    /// The whole lines that wait now, without waiting for more.
    fn waiting(&mut self) -> Vec<String> {
        if self.stream.set_nonblocking(true).is_ok() {
            let mut chunk = [0u8; 4096];
            while self.pending.len() <= Self::MAX {
                match self.stream.read(&mut chunk) {
                    Ok(read) if read > 0 => self.pending.extend_from_slice(&chunk[..read]),
                    _ => break,
                }
            }
            let _ = self.stream.set_nonblocking(false);
        }
        std::iter::from_fn(|| self.take()).collect()
    }
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
            locked: false,
            unlock_entry: false,
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
/// The shell's wrapper connects to the compositor's control channel and hands
/// the shell the socket in `CROWBAR_SHELL_CONTROL_FD`. The wire format is one
/// JSON object per line: a request gets one reply, and the compositor may
/// send events at any time. See `compositor/src/control.rs`.
pub struct Compositor {
    /// The read and write halves of the socket. `None` once an exchange has
    /// failed.
    lines: Mutex<Option<(Lines, UnixStream)>>,
}

impl Compositor {
    pub fn new(stream: UnixStream) -> Result<Self> {
        stream.set_read_timeout(Some(Duration::from_millis(500)))?;
        stream.set_write_timeout(Some(Duration::from_millis(500)))?;
        Ok(Self {
            lines: Mutex::new(Some((
                Lines {
                    stream: stream.try_clone()?,
                    pending: Vec::new(),
                },
                stream,
            ))),
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
        reader: &mut Lines,
        writer: &mut UnixStream,
        request: serde_json::Value,
    ) -> Result<serde_json::Value> {
        writer.write_all(format!("{request}\n").as_bytes())?;
        loop {
            let line = reader.next()?;
            let message: serde_json::Value = serde_json::from_str(&line)?;
            if let Some(event) = message.get("event") {
                compositor_event(event);
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
            #[serde(default)]
            attention: bool,
        }
        #[derive(Deserialize)]
        struct Reply {
            workspace: i32,
            active: Option<String>,
            #[serde(default)]
            locked: bool,
            #[serde(default)]
            unlock_entry: bool,
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
                attention: w.attention,
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
            locked: reply.locked,
            unlock_entry: reply.unlock_entry,
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

    fn launching(&self) -> Result<()> {
        self.request(serde_json::json!({"op": "launching"})).map(drop)
    }

    fn lock(&self, locked: bool) -> Result<()> {
        let op = if locked { "lock" } else { "unlock" };
        self.request(serde_json::json!({ "op": op })).map(drop)
    }

    fn pump(&self) {
        let Ok(mut lines) = self.lines.lock() else {
            return;
        };
        let Some((reader, _)) = lines.as_mut() else {
            return;
        };
        for line in reader.waiting() {
            match serde_json::from_str::<serde_json::Value>(&line) {
                Ok(message) => {
                    if let Some(event) = message.get("event") {
                        compositor_event(event);
                    }
                }
                Err(e) => log::warn!("Compositor: {e}"),
            }
        }
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
/// Asks the compositor to lock the session, or to unlock it, as the person
/// asks the shell. Unlocking is the shell's word, in the place of the broker
/// ADR 0020 has end it: Fulcrum's proposal until a record decides.
pub fn lock(locked: bool) {
    std::thread::spawn(move || {
        if let Err(e) = connection().and_then(|c| c.lock(locked)) {
            log::warn!("Lock: {e}");
        }
    });
}
/// Whether an unlock helper checks the person's password. Where one does,
/// the compositor takes its word to unlock, and not the shell's.
pub fn unlocks_with_password() -> bool {
    std::env::var_os("CROWBAR_UNLOCK_SOCKET").is_some()
}
/// Sends the person's password to the unlock helper, which checks it and
/// tells the compositor to unlock; the answer comes back as
/// [`Event::Unlock`]. The password is not kept.
pub fn unlock_with_password(password: String, tx: Sender<Event>) {
    std::thread::spawn(move || {
        let result = std::env::var_os("CROWBAR_UNLOCK_SOCKET")
            .context("No unlock helper")
            .and_then(|path| {
                UnixStream::connect(path).context("The unlock helper is not running")
            })
            .and_then(|stream| ask_unlock_helper(stream, password));
        log::info!("SHELL unlock ok={}", result.is_ok());
        let _ = tx.send(Event::Unlock(result.map_err(|e| e.to_string())));
    });
}
/// One request to the unlock helper: the password, and its answer.
fn ask_unlock_helper(mut stream: UnixStream, password: String) -> Result<()> {
    // A wrong password costs a pause before the helper answers.
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut body = serde_json::to_vec(&serde_json::json!({ "password": password }))?;
    drop(password);
    body.push(b'\n');
    let sent = stream.write_all(&body);
    body.fill(0);
    sent?;
    let mut response = String::new();
    BufReader::new(stream).take(4097).read_line(&mut response)?;
    if response.len() > 4096 || !response.ends_with('\n') {
        bail!("The unlock helper did not answer")
    }
    let reply: serde_json::Value = serde_json::from_str(&response)?;
    if reply["ok"] != true {
        bail!("{}", reply["error"].as_str().unwrap_or("Could not unlock"));
    }
    Ok(())
}
/// Reads the compositor's events that wait, as Super tapped alone.
pub fn pump() {
    if let Ok(connection) = connection() {
        connection.pump();
    }
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
/// The command that starts the application behind a `.desktop` file.
///
/// The descriptors of the shell's channels to the compositor are
/// close-on-exec, so the program cannot use them. The variables that name
/// them are removed as well, so it does not mistake some other descriptor
/// for one.
fn launcher_command(path: &std::path::Path) -> Command {
    let mut command = Command::new("/usr/bin/systemd-run");
    command
        .args([
            "--user",
            "--scope",
            "--collect",
            "--quiet",
            "--expand-environment=no",
            "/usr/bin/gio",
            "launch",
        ])
        .arg(path)
        .env_remove("CROWBAR_SHELL_CONTROL_FD")
        .env_remove("WAYLAND_SOCKET");
    command
}
/// Alpha-only root stand-in. Its socket checks the shell UID; the helper
/// chooses app credentials and starts from an empty environment/descriptor set.
pub fn lab_request(request: serde_json::Value) -> Result<serde_json::Value> {
    let path = std::env::var_os("CROWBAR_LAUNCH_SOCKET").context("No Alpha launcher")?;
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut body = serde_json::to_vec(&request)?;
    body.push(b'\n');
    stream.write_all(&body)?;
    let mut response = String::new();
    BufReader::new(stream)
        .take(65538)
        .read_line(&mut response)?;
    if response.len() > 65537 || !response.ends_with('\n') {
        bail!("Invalid Alpha launcher response")
    }
    let reply: serde_json::Value = serde_json::from_str(&response)?;
    if reply["ok"] != true {
        bail!("Alpha launcher: {}", reply["error"]);
    }
    Ok(reply)
}

pub fn launch(path: PathBuf, tx: Sender<Event>) {
    std::thread::spawn(move || {
        // The app's first window takes the keyboard, as the person started it.
        if let Err(e) = connection().and_then(|c| c.launching()) {
            log::warn!("Launching: {e}");
        }
        let result = (|| -> Result<()> {
            if std::env::var_os("CROWBAR_LAUNCH_SOCKET").is_some() {
                lab_request(serde_json::json!({"op": "launch", "desktop": path}))?;
                return Ok(());
            }
            let mut child = launcher_command(&path)
                .current_dir(
                    std::env::var_os("HOME").context("HOME is required to launch applications")?,
                )
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
            if !same_user(&s) {
                continue;
            }
            let _ = s.set_read_timeout(Some(Duration::from_millis(250)));
            let mut bytes = [0u8; 32];
            if let Ok(n) = s.read(&mut bytes) {
                let event = match &bytes[..n] {
                    b"snapshot" => {
                        if let Ok(snapshot) = snapshot() {
                            let _ = serde_json::to_writer(&mut s, &snapshot);
                        }
                        continue;
                    }
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
fn same_user(stream: &UnixStream) -> bool {
    use std::os::fd::AsRawFd;
    // SO_PEERCRED is supplied by the kernel, not by the request.
    unsafe {
        let mut peer: libc::ucred = std::mem::zeroed();
        let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut size,
        ) == 0
            && peer.uid == libc::geteuid()
    }
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

    /// The lock screen sends the unlock helper the password alone, and says
    /// why it did not unlock.
    #[test]
    fn the_unlock_helper_hears_the_password_and_its_answer_is_said() {
        for (reply, expected) in [
            ("{\"ok\":true}\n", Ok(())),
            ("{\"ok\":false,\"error\":\"Wrong password\"}\n", Err("Wrong password")),
            ("{\"ok\":true}", Err("The unlock helper did not answer")),
        ] {
            let (ours, theirs) = UnixStream::pair().unwrap();
            let helper = std::thread::spawn(move || {
                let mut line = String::new();
                BufReader::new(theirs.try_clone().unwrap()).read_line(&mut line).unwrap();
                (&theirs).write_all(reply.as_bytes()).unwrap();
                line
            });
            let result = ask_unlock_helper(ours, "crowbar".into()).map_err(|e| e.to_string());
            assert_eq!(result, expected.map_err(str::to_string));
            let sent: serde_json::Value = serde_json::from_str(&helper.join().unwrap()).unwrap();
            assert_eq!(sent, serde_json::json!({ "password": "crowbar" }));
        }
    }

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
    fn super_tapped_alone_toggles_the_launcher_and_a_launch_is_announced() {
        let (tx, rx) = std::sync::mpsc::channel();
        deliver_events_to(tx);
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let compositor = Compositor::new(ours).unwrap();

        // An event that waits with no request is read by the pump.
        theirs.write_all(b"{\"event\":\"launcher\"}\n").unwrap();
        compositor.pump();
        assert!(matches!(rx.try_recv(), Ok(Event::Toggle)));

        // Half a line waits for the rest, and nothing is lost.
        theirs.write_all(b"{\"event\":\"laun").unwrap();
        compositor.pump();
        assert!(rx.try_recv().is_err());
        theirs.write_all(b"cher\"}\n").unwrap();
        compositor.pump();
        assert!(matches!(rx.try_recv(), Ok(Event::Toggle)));

        // The compositor locking and unlocking the session reaches the shell.
        theirs
            .write_all(b"{\"event\":\"locked\"}\n{\"event\":\"unlocked\"}\n")
            .unwrap();
        compositor.pump();
        assert!(matches!(rx.try_recv(), Ok(Event::Lock(true))));
        assert!(matches!(rx.try_recv(), Ok(Event::Lock(false))));
        theirs.write_all(b"{\"event\":\"unlock_entry\"}\n").unwrap();
        compositor.pump();
        assert!(matches!(rx.try_recv(), Ok(Event::UnlockEntry)));

        // Half an event that a request's reply follows is read with it.
        theirs.write_all(b"{\"event\":\"laun").unwrap();
        compositor.pump();
        let peer = std::thread::spawn(move || {
            let mut reader = BufReader::new(theirs.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, "{\"op\":\"launching\"}\n");
            theirs.write_all(b"cher\"}\n{\"reply\":\"ok\"}\n").unwrap();
        });
        compositor.launching().unwrap();
        peer.join().unwrap();
        assert!(matches!(rx.try_recv(), Ok(Event::Toggle)));
    }

    #[test]
    fn a_window_that_wants_attention_is_marked_in_the_snapshot() {
        let (compositor, peer) = scripted(vec![(
            r#"{"op":"snapshot"}"#,
            concat!(
                r#"{"reply":"snapshot","workspace":1,"active":null,"windows":["#,
                r#"{"id":"0x1","app_id":"org.example.Notes","title":"Notes","workspace":3,"attention":true}]}"#,
                "\n"
            ),
        )]);
        let snapshot = compositor.snapshot().unwrap();
        peer.join().unwrap();
        assert!(snapshot.clients[0].attention);
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
            attention: false,
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
                        attention: false,
                    },
                    studio.clone(),
                ],
                active: studio,
                connected: true,
                locked: false,
                unlock_entry: false,
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

    /// A program the shell starts must not learn where the shell's own
    /// channels to the compositor are.
    #[test]
    fn a_launched_program_is_not_given_the_shells_channels() {
        let command = launcher_command(std::path::Path::new("/usr/share/applications/x.desktop"));

        let removed: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        assert!(
            removed.contains(&"CROWBAR_SHELL_CONTROL_FD".to_string()),
            "{removed:?}"
        );
        assert!(
            removed.contains(&"WAYLAND_SOCKET".to_string()),
            "{removed:?}"
        );
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
            attention: false,
        };
        assert_eq!(
            snapshot,
            Snapshot {
                workspace: 2,
                clients: vec![terminal.clone()],
                active: terminal,
                connected: true,
                locked: false,
                unlock_entry: false,
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
                    attention: false,
                },
                Client {
                    address: "0x2".into(),
                    class: "org.example.Notes".into(),
                    title: "Notes".into(),
                    workspace: Workspace { id: 0 },
                    attention: false,
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
