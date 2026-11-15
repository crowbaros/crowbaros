// SPDX-License-Identifier: MIT
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc::Sender,
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
pub fn hypr_path() -> Result<PathBuf> {
    let sig =
        std::env::var("HYPRLAND_INSTANCE_SIGNATURE").context("No Hyprland session signature")?;
    if sig.contains('/') || sig.contains("..") {
        bail!("Invalid Hyprland signature")
    }
    Ok(
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("Missing runtime directory")?)
            .join("hypr")
            .join(sig)
            .join(".socket.sock"),
    )
}
pub fn request(command: &str) -> Result<String> {
    let mut s = UnixStream::connect(hypr_path()?)?;
    s.set_read_timeout(Some(Duration::from_millis(500)))?;
    s.set_write_timeout(Some(Duration::from_millis(500)))?;
    s.write_all(command.as_bytes())?;
    let mut out = String::new();
    s.take(4 * 1024 * 1024).read_to_string(&mut out)?;
    Ok(out)
}
pub fn snapshot() -> Result<Snapshot> {
    Ok(Snapshot {
        workspace: serde_json::from_str::<Workspace>(&request("j/activeworkspace")?)?.id,
        clients: serde_json::from_str(&request("j/clients")?)?,
        active: serde_json::from_str(&request("j/activewindow")?)?,
        connected: true,
    })
}
pub fn workspace(id: i32) {
    if id > 0 {
        std::thread::spawn(move || {
            if let Err(e) = request(&format!(
                "/eval hl.dispatch(hl.dsp.focus({{workspace=\"{id}\"}}))"
            )) {
                log::warn!("Workspace: {e}");
            }
        });
    }
}
pub fn focus(address: String) {
    if address.starts_with("0x") && address[2..].chars().all(|c| c.is_ascii_hexdigit()) {
        std::thread::spawn(move || {
            if let Err(e) = request(&format!(
                "/eval hl.dispatch(hl.dsp.focus({{window=\"address:{address}\"}}))"
            )) {
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
