// SPDX-License-Identifier: MIT
//! Typed client for Base's launcher service (`CROWBAR_LAUNCH_SOCKET`), which
//! runs as the person's account and takes requests from the shell's UID alone
//! (ADR 0032, ADR 0038). Replaces untyped `launcher_request(json!(...))`
//! scattered across appearance, quick settings, theme switcher, and session.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use crate::appearance::{Preferences, ThemeCatalog};
use crate::compositor::CompositorClient;
use crate::events::{Event, Sender};
use crate::quick_settings::{
    AudioQuickState, BluetoothQuickState, BrightnessState, QuickStatus, WifiQuickState,
    clamp_brightness_percent, clamp_volume_percent, parse_audio, parse_bluetooth, parse_brightness,
    parse_quick_status, parse_wifi,
};

/// Base's desktop entry that opens OpenCode's sign-in in foot.
pub const AGENT_SIGN_IN_DESKTOP: &str = "/usr/share/applications/com.crowbaros.AgentSignIn.desktop";
/// Base's desktop entry for Theme Studio.
pub const THEME_STUDIO_DESKTOP: &str =
    "/usr/share/applications/com.crowbaros.ThemeStudio.desktop";

const DEFAULT_MAX_REPLY_BYTES: usize = 65537;
const THEMES_MAX_REPLY_BYTES: usize = 512 * 1024;

/// Typed client for Base's launcher socket. Cheap to clone and injectable in
/// tests without mutating process environment variables.
#[derive(Clone, Debug, Default)]
pub struct LauncherClient {
    socket: Option<PathBuf>,
}

impl LauncherClient {
    pub fn from_env() -> Self {
        Self {
            socket: std::env::var_os("CROWBAR_LAUNCH_SOCKET").map(PathBuf::from),
        }
    }

    #[cfg(test)]
    pub fn with_socket(socket: PathBuf) -> Self {
        Self {
            socket: Some(socket),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.socket.is_some()
    }

    fn request(&self, request: Value) -> Result<Value> {
        self.request_bounded(request, DEFAULT_MAX_REPLY_BYTES)
    }

    pub(crate) fn request_bounded(&self, request: Value, max_bytes: usize) -> Result<Value> {
        let path = self.socket.as_deref().context("No launcher service")?;
        let mut stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let mut body = serde_json::to_vec(&request)?;
        body.push(b'\n');
        stream.write_all(&body)?;
        let mut response = String::new();
        BufReader::new(stream)
            .take(max_bytes as u64 + 1)
            .read_line(&mut response)?;
        if response.len() > max_bytes || !response.ends_with('\n') {
            bail!("Invalid launcher service response")
        }
        let reply: Value = serde_json::from_str(&response)?;
        if reply["ok"] != true {
            bail!("Launcher service: {}", reply["error"]);
        }
        Ok(reply)
    }

    /// Reads the person's saved appearance preferences through the launcher
    /// socket on Base, or from `$XDG_CONFIG_HOME/crowbaros/appearance.json`
    /// in same-user development sessions.
    pub fn appearance(&self) -> Option<Preferences> {
        if self.is_configured() {
            let reply = self.request(json!({ "op": "appearance" })).ok()?;
            let prefs: Preferences = serde_json::from_value(reply["preferences"].clone()).ok()?;
            return (prefs.schema_version == 1).then_some(prefs);
        }
        let home = std::env::var_os("HOME")?;
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| PathBuf::from(home).join(".config"));
        let p: Preferences =
            serde_json::from_slice(&std::fs::read(config.join("crowbaros/appearance.json")).ok()?)
                .ok()?;
        (p.schema_version == 1).then_some(p)
    }

    /// Reads the installed theme catalog and active theme through the launcher.
    pub fn themes(&self) -> Result<ThemeCatalog> {
        let reply = self.request_bounded(json!({ "op": "themes" }), THEMES_MAX_REPLY_BYTES)?;
        Ok(serde_json::from_value(reply)?)
    }

    /// Applies an installed theme (and optional named background).
    pub fn apply_theme(&self, id: &str, background: Option<&str>) -> Result<()> {
        let req = match background {
            Some(bg) => json!({ "op": "apply_theme", "id": id, "background": bg }),
            None => json!({ "op": "apply_theme", "id": id }),
        };
        self.request(req).map(drop)
    }

    /// Cycles to the next background of the active theme.
    pub fn cycle_background(&self) -> Result<()> {
        self.request(json!({ "op": "cycle_background" })).map(drop)
    }

    /// Launches a `.desktop` entry synchronously through the launcher socket.
    pub fn launch_desktop_sync(&self, desktop: &str) -> Result<()> {
        self.request(json!({ "op": "launch", "desktop": desktop }))
            .map(drop)
    }

    /// Reads a saved preference document (`"input"` or `"power"`).
    pub fn saved_preferences(&self, kind: &str) -> Result<Value> {
        let reply = self.request(json!({ "op": format!("{kind}_preferences") }))?;
        Ok(reply["preferences"].clone())
    }

    /// Fetches Quick Settings hardware status (defaults to unavailable on error).
    pub fn quick_status(&self) -> QuickStatus {
        match self.request(json!({ "op": "quick_status" })) {
            Ok(reply) => parse_quick_status(&reply),
            Err(_) => QuickStatus::default(),
        }
    }

    pub fn set_volume(&self, percent: u32) -> Result<AudioQuickState, String> {
        let clamped = clamp_volume_percent(percent);
        let reply = self
            .request(json!({ "op": "set_volume", "percent": clamped }))
            .map_err(|e| e.to_string())?;
        Ok(parse_audio(&reply["audio"]))
    }

    pub fn step_volume(&self, delta: i32) -> Result<AudioQuickState, String> {
        let reply = self
            .request(json!({ "op": "step_volume", "delta": delta }))
            .map_err(|e| e.to_string())?;
        Ok(parse_audio(&reply["audio"]))
    }

    pub fn toggle_mute(&self) -> Result<AudioQuickState, String> {
        let reply = self
            .request(json!({ "op": "toggle_mute" }))
            .map_err(|e| e.to_string())?;
        Ok(parse_audio(&reply["audio"]))
    }

    pub fn toggle_mic_mute(&self) -> Result<AudioQuickState, String> {
        let reply = self
            .request(json!({ "op": "toggle_mic_mute" }))
            .map_err(|e| e.to_string())?;
        Ok(parse_audio(&reply["audio"]))
    }

    pub fn set_brightness(&self, percent: u32) -> Result<BrightnessState, String> {
        let clamped = clamp_brightness_percent(percent);
        let reply = self
            .request(json!({ "op": "set_brightness", "percent": clamped }))
            .map_err(|e| e.to_string())?;
        Ok(parse_brightness(&reply["brightness"]))
    }

    pub fn step_brightness(&self, delta: i32) -> Result<BrightnessState, String> {
        let reply = self
            .request(json!({ "op": "step_brightness", "delta": delta }))
            .map_err(|e| e.to_string())?;
        Ok(parse_brightness(&reply["brightness"]))
    }

    pub fn set_wifi_power(&self, powered: bool) -> Result<WifiQuickState, String> {
        let reply = self
            .request(json!({ "op": "set_wifi_power", "powered": powered }))
            .map_err(|e| e.to_string())?;
        Ok(parse_wifi(&reply["wifi"]))
    }

    pub fn set_bluetooth_power(&self, powered: bool) -> Result<BluetoothQuickState, String> {
        let reply = self
            .request(json!({ "op": "set_bluetooth_power", "powered": powered }))
            .map_err(|e| e.to_string())?;
        Ok(parse_bluetooth(&reply["bluetooth"]))
    }

    pub fn power_action(&self, action: &str) -> Result<(), String> {
        self.request(json!({ "op": "power", "action": action }))
            .map(drop)
            .map_err(|e| e.to_string())
    }

    fn launch_desktop_async(
        &self,
        path: PathBuf,
        compositor: CompositorClient,
        tx: Sender,
        wrap: fn(Result<(), String>) -> Event,
    ) {
        let client = self.clone();
        std::thread::spawn(move || {
            // The app's first window takes the keyboard, as the person started it.
            if let Err(e) = compositor.launching() {
                log::warn!("Launching: {e}");
            }
            let result = (|| -> Result<()> {
                if client.is_configured() {
                    client.request(json!({ "op": "launch", "desktop": path }))?;
                    return Ok(());
                }
                let mut child = launcher_command(&path)
                    .current_dir(
                        std::env::var_os("HOME")
                            .context("HOME is required to launch applications")?,
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
                        bail!(
                            "The application could not start. Try again or check its installation."
                        )
                    }
                    if Instant::now() > deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        bail!("The application did not respond. Try again.")
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            })();
            let _ = tx.send(wrap(result.map_err(|e| e.to_string())));
        });
    }

    pub fn launch(&self, path: PathBuf, compositor: CompositorClient, tx: Sender) {
        self.launch_desktop_async(path, compositor, tx, Event::Launch);
    }

    /// Opens Base's sign-in desktop entry through the launcher service, as any app.
    pub fn launch_agent_sign_in(&self, compositor: CompositorClient, tx: Sender) {
        self.launch_desktop_async(
            PathBuf::from(AGENT_SIGN_IN_DESKTOP),
            compositor,
            tx,
            Event::AgentLaunch,
        );
    }
}

/// The command that starts the application behind a `.desktop` file when no
/// Base launcher socket is configured.
///
/// The descriptors of the shell's channels to the compositor are
/// close-on-exec, so the program cannot use them. The variables that name
/// them are removed as well, so it does not mistake some other descriptor
/// for one.
fn launcher_command(path: &Path) -> Command {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// A program the shell starts must not learn where the shell's own
    /// channels to the compositor are.
    #[test]
    fn a_launched_program_is_not_given_the_shells_channels() {
        let command = launcher_command(Path::new("/usr/share/applications/x.desktop"));

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

    #[test]
    fn typed_launcher_client_sends_expected_wire_ops_and_decodes_replies() {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let base = std::env::temp_dir();
        let root = if base.as_os_str().len() > 48 {
            PathBuf::from("/tmp")
        } else {
            base
        };
        let dir = root.join(format!(
            "crowbaros-launcher-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("launch.sock");
        let listener = UnixListener::bind(&path).unwrap();

        let peer = std::thread::spawn(move || {
            let expectations = [
                (
                    json!({"op":"apply_theme","id":"dark"}),
                    json!({"ok":true}),
                ),
                (
                    json!({"op":"apply_theme","id":"dark","background":"nightwatch.png"}),
                    json!({"ok":true}),
                ),
                (
                    json!({"op":"cycle_background"}),
                    json!({"ok":true}),
                ),
                (
                    json!({"op":"set_volume","percent":100}),
                    json!({"ok":true,"audio":{"available":true,"volume":100,"muted":false,"sink_name":"Speakers","mic_available":false,"mic_muted":false}}),
                ),
                (
                    json!({"op":"power","action":"suspend"}),
                    json!({"ok":true}),
                ),
            ];
            for (expected, reply) in expectations {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let got: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(got, expected);
                let mut out = serde_json::to_vec(&reply).unwrap();
                out.push(b'\n');
                (&stream).write_all(&out).unwrap();
            }
            let _ = std::fs::remove_dir_all(dir);
        });

        let client = LauncherClient::with_socket(path);
        client.apply_theme("dark", None).unwrap();
        client
            .apply_theme("dark", Some("nightwatch.png"))
            .unwrap();
        client.cycle_background().unwrap();
        let audio = client.set_volume(140).unwrap();
        assert_eq!(audio.volume, 100);
        assert_eq!(audio.sink_name, "Speakers");
        client.power_action("suspend").unwrap();

        peer.join().unwrap();
    }

    #[test]
    fn launcher_client_surfaces_errors_and_rejects_oversized_or_malformed_replies() {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let base = std::env::temp_dir();
        let root = if base.as_os_str().len() > 48 {
            PathBuf::from("/tmp")
        } else {
            base
        };
        let dir = root.join(format!(
            "crowbaros-launcher-err-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("launcher.sock");
        let listener = UnixListener::bind(&path).unwrap();

        let peer = std::thread::spawn(move || {
            // 1. Explicit error reply
            {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                (&stream)
                    .write_all(b"{\"ok\":false,\"error\":\"theme not found\"}\n")
                    .unwrap();
            }
            // 2. Malformed JSON reply
            {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                (&stream).write_all(b"{not-json\n").unwrap();
            }
            // 3. Oversized reply exceeding DEFAULT_MAX_REPLY_BYTES
            {
                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                let huge = vec![b'a'; DEFAULT_MAX_REPLY_BYTES + 64];
                let _ = (&stream).write_all(&huge);
            }
            let _ = std::fs::remove_dir_all(dir);
        });

        let client = LauncherClient::with_socket(path);
        let err = client.apply_theme("missing", None).unwrap_err();
        assert!(err.to_string().contains("theme not found"), "{err}");

        assert!(client.cycle_background().is_err());
        let err = client.power_action("reboot").unwrap_err();
        assert!(
            err.to_string().contains("Invalid launcher service response"),
            "{err}"
        );

        peer.join().unwrap();
    }
}
