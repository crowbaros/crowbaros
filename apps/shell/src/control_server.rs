// SPDX-License-Identifier: MIT
//! Shell control socket (`control.sock`), bounded settings bridge
//! (`CROWBAR_DISPLAY_SOCKET`) for Studio Settings, and saved preference restore.

use anyhow::{Context, Result, bail};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::agent::AgentViewSnapshot;
use crate::compositor::{Adapter, CompositorClient};
use crate::events::{Event, Sender};
use crate::launcher_client::LauncherClient;

fn runtime() -> Result<PathBuf> {
    let dir = PathBuf::from(
        std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is required")?,
    )
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

pub fn server(
    tx: Sender,
    compositor: CompositorClient,
    agent_view: Arc<Mutex<AgentViewSnapshot>>,
) -> Result<File> {
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
                        let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
                        if let Ok(snapshot) = compositor.snapshot() {
                            let _ = serde_json::to_writer(&mut s, &snapshot);
                        }
                        continue;
                    }
                    b"agent" => {
                        let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
                        let view = agent_view.lock().ok().map(|guard| guard.clone());
                        if let Some(view) = view {
                            let _ = serde_json::to_writer(&mut s, &view);
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

fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    // SO_PEERCRED is supplied by the kernel, not by the request.
    unsafe {
        let mut peer: libc::ucred = std::mem::zeroed();
        let mut size = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        (libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut peer as *mut libc::ucred).cast(),
            &mut size,
        ) == 0)
            .then_some(peer.uid)
    }
}

fn same_user(stream: &UnixStream) -> bool {
    let uid = unsafe { libc::geteuid() };
    peer_uid(stream) == Some(uid)
}

/// Validates and handles one JSON request on the settings bridge.
/// Only display, input, and power configuration cross this seam; window,
/// workspace, launcher, and lock operations are always refused.
fn handle_display_request(line: &str, adapter: &dyn Adapter) -> serde_json::Value {
    let err = |msg: &str| serde_json::json!({ "reply": "error", "message": msg });
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(line) else {
        return err("Display request must be a JSON object");
    };
    let Some(op) = map.get("op").and_then(|v| v.as_str()) else {
        return err("Display request must name its op");
    };
    match op {
        "outputs" => {
            if map.len() != 1 {
                return err("outputs takes no additional fields");
            }
            match adapter.outputs() {
                Ok(reply) => reply,
                Err(e) => err(&e.to_string()),
            }
        }
        "configure_output" => {
            const ALLOWED: &[&str] = &[
                "op", "name", "width", "height", "refresh", "x", "y", "scale",
            ];
            if let Some(extra) = map.keys().find(|k| !ALLOWED.contains(&k.as_str())) {
                return err(&format!("configure_output takes no field {extra:?}"));
            }
            let Some(name) = map
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            else {
                return err("configure_output requires a non-empty output name");
            };
            let mut clean = serde_json::Map::new();
            clean.insert(
                "op".into(),
                serde_json::Value::String("configure_output".into()),
            );
            clean.insert("name".into(), serde_json::Value::String(name.into()));
            for field in ["width", "height", "refresh", "x", "y", "scale"] {
                if let Some(val) = map.get(field) {
                    if let Some(n) = val.as_i64() {
                        clean.insert(field.into(), serde_json::Value::Number(n.into()));
                    } else {
                        return err(&format!("{field} must be a whole number"));
                    }
                }
            }
            match adapter.configure_output(serde_json::Value::Object(clean)) {
                Ok(()) => serde_json::json!({ "reply": "ok" }),
                Err(e) => err(&e.to_string()),
            }
        }
        "input_config" => {
            if map.len() != 1 {
                return err("input_config takes no additional fields");
            }
            match adapter.input_config() {
                Ok(reply) => reply,
                Err(e) => err(&e.to_string()),
            }
        }
        "configure_input" => {
            const ALLOWED: &[&str] = &[
                "op",
                "layout",
                "repeat_rate",
                "repeat_delay",
                "pointer_speed",
                "natural_scroll",
            ];
            if let Some(extra) = map.keys().find(|k| !ALLOWED.contains(&k.as_str())) {
                return err(&format!("configure_input takes no field {extra:?}"));
            }
            let mut clean = serde_json::Map::new();
            clean.insert(
                "op".into(),
                serde_json::Value::String("configure_input".into()),
            );
            if let Some(val) = map.get("layout") {
                let Some(s) = val
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 32)
                else {
                    return err("layout must be a non-empty layout name");
                };
                clean.insert("layout".into(), serde_json::Value::String(s.into()));
            }
            for field in ["repeat_rate", "repeat_delay", "pointer_speed"] {
                if let Some(val) = map.get(field) {
                    let Some(n) = val.as_i64() else {
                        return err(&format!("{field} must be a whole number"));
                    };
                    clean.insert(field.into(), serde_json::Value::Number(n.into()));
                }
            }
            if let Some(val) = map.get("natural_scroll") {
                let Some(b) = val.as_bool() else {
                    return err("natural_scroll must be a boolean");
                };
                clean.insert("natural_scroll".into(), serde_json::Value::Bool(b));
            }
            match adapter.configure_input(serde_json::Value::Object(clean)) {
                Ok(()) => serde_json::json!({ "reply": "ok" }),
                Err(e) => err(&e.to_string()),
            }
        }
        "power_config" => {
            if map.len() != 1 {
                return err("power_config takes no additional fields");
            }
            match adapter.power_config() {
                Ok(reply) => reply,
                Err(e) => err(&e.to_string()),
            }
        }
        "configure_power" => {
            const ALLOWED: &[&str] = &["op", "lock_after_secs", "suspend_after_secs"];
            if let Some(extra) = map.keys().find(|k| !ALLOWED.contains(&k.as_str())) {
                return err(&format!("configure_power takes no field {extra:?}"));
            }
            let mut clean = serde_json::Map::new();
            clean.insert(
                "op".into(),
                serde_json::Value::String("configure_power".into()),
            );
            for field in ["lock_after_secs", "suspend_after_secs"] {
                if let Some(val) = map.get(field) {
                    let Some(n) = val.as_u64() else {
                        return err(&format!("{field} must be a non-negative integer"));
                    };
                    clean.insert(field.into(), serde_json::Value::Number(n.into()));
                }
            }
            match adapter.configure_power(serde_json::Value::Object(clean)) {
                Ok(()) => serde_json::json!({ "reply": "ok" }),
                Err(e) => err(&e.to_string()),
            }
        }
        _ => err("Only display, input, and power configuration ops are allowed"),
    }
}

/// Restores a schema-1 preference through the same validation as a live
/// settings request. The file cannot choose an operation or omit fields.
fn restore_preference(kind: &str, saved: serde_json::Value, adapter: &dyn Adapter) -> Result<()> {
    if saved.is_null() {
        return Ok(());
    }
    let (op, fields): (&str, &[&str]) = match kind {
        "input" => (
            "configure_input",
            &[
                "layout",
                "repeat_rate",
                "repeat_delay",
                "pointer_speed",
                "natural_scroll",
            ],
        ),
        "power" => (
            "configure_power",
            &["lock_after_secs", "suspend_after_secs"],
        ),
        _ => bail!("Unknown preference kind"),
    };
    let mut request = saved
        .as_object()
        .context("Preferences must be an object")?
        .clone();
    if request.remove("schema_version") != Some(serde_json::json!(1))
        || request.len() != fields.len()
        || fields.iter().any(|field| !request.contains_key(*field))
    {
        bail!("Invalid or unsupported saved {kind} preferences");
    }
    request.insert("op".into(), serde_json::json!(op));
    let reply = handle_display_request(&serde_json::to_string(&request)?, adapter);
    if reply["reply"] != "ok" {
        bail!("Saved {kind} preferences were rejected");
    }
    Ok(())
}

/// The launcher reads the person's private files; the shell alone applies
/// them to Fulcrum. This runs before accepting live settings changes, each
/// time the shell starts (including after the compositor restarts).
fn restore_saved_preferences(compositor: &CompositorClient, launcher: &LauncherClient) {
    if !launcher.is_configured() {
        return;
    }
    for kind in ["input", "power"] {
        // runit starts the launcher and shell independently. Allow the
        // launcher time to bind, without blocking the shell's interface.
        let deadline = Instant::now() + Duration::from_secs(5);
        let saved = loop {
            match launcher.saved_preferences(kind) {
                Ok(prefs) => break Ok(prefs),
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100))
                }
                Err(error) => break Err(error),
            }
        };
        let result = saved.and_then(|prefs| {
            restore_preference(kind, prefs, compositor.adapter()?.as_ref())
        });
        match result {
            Ok(()) => log::info!("SHELL preferences restored kind={kind}"),
            Err(error) => log::warn!("Could not restore {kind} preferences: {error}"),
        }
    }
}

/// Exposes a strictly bounded Unix socket (`CROWBAR_DISPLAY_SOCKET`) that lets
/// Studio Settings inspect and configure settings via Fulcrum without giving
/// any app access to the shell's control channel.
pub fn start_display_bridge(compositor: CompositorClient, launcher: LauncherClient) {
    let Some(path) = std::env::var_os("CROWBAR_DISPLAY_SOCKET").map(PathBuf::from) else {
        return;
    };
    let allowed_uid = std::env::var("CROWBAR_DISPLAY_UID")
        .ok()
        .and_then(|s| s.parse::<u32>().ok());
    let self_uid = unsafe { libc::geteuid() };
    let _ = fs::remove_file(&path);
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("Display bridge {}: {e}", path.display());
            return;
        }
    };
    if let Err(e) = fs::set_permissions(&path, fs::Permissions::from_mode(0o660)) {
        log::warn!("Display bridge permissions {}: {e}", path.display());
        return;
    }
    log::info!("SHELL display-bridge listening={}", path.display());
    std::thread::spawn(move || {
        restore_saved_preferences(&compositor, &launcher);
        for stream in listener.incoming().flatten() {
            let Some(uid) = peer_uid(&stream) else {
                continue;
            };
            let allowed = uid == self_uid || allowed_uid == Some(uid);
            if !allowed {
                log::warn!("SHELL display-bridge refused uid={uid}");
                continue;
            }
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
            let mut line = String::new();
            let reply = match BufReader::new(&stream).take(4097).read_line(&mut line) {
                Ok(n) if n > 0 && line.len() <= 4096 && line.ends_with('\n') => {
                    match compositor.adapter() {
                        Ok(adapter) => handle_display_request(line.trim_end(), adapter.as_ref()),
                        Err(e) => {
                            serde_json::json!({ "reply": "error", "message": e.to_string() })
                        }
                    }
                }
                _ => serde_json::json!({
                    "reply": "error",
                    "message": "Invalid or oversized display request",
                }),
            };
            if let Ok(mut bytes) = serde_json::to_vec(&reply) {
                bytes.push(b'\n');
                let _ = (&stream).write_all(&bytes);
            }
        }
    });
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
    use crate::compositor::test_support::scripted;

    #[test]
    fn saved_input_and_power_are_restored_through_the_bounded_bridge() {
        let (adapter, peer) = scripted(vec![
            (
                r#"{"op":"configure_input","layout":"gb","repeat_rate":40,"repeat_delay":300,"pointer_speed":4,"natural_scroll":true}"#,
                &[r#"{"reply":"ok"}"#],
            ),
            (
                r#"{"op":"configure_power","lock_after_secs":600,"suspend_after_secs":1800}"#,
                &[r#"{"reply":"ok"}"#],
            ),
        ]);
        restore_preference(
            "input",
            serde_json::json!({
                "schema_version": 1, "layout": "gb", "repeat_rate": 40,
                "repeat_delay": 300, "pointer_speed": 4, "natural_scroll": true,
            }),
            &adapter,
        )
        .unwrap();
        restore_preference(
            "power",
            serde_json::json!({
                "schema_version": 1, "lock_after_secs": 600, "suspend_after_secs": 1800,
            }),
            &adapter,
        )
        .unwrap();
        peer.join().unwrap();
    }

    #[test]
    fn saved_preferences_cannot_choose_ops_or_skip_validation() {
        let (adapter, peer) = scripted(vec![]);
        restore_preference("input", serde_json::Value::Null, &adapter).unwrap();
        for saved in [
            serde_json::json!({"schema_version": 2, "lock_after_secs": 600, "suspend_after_secs": 1800}),
            serde_json::json!({"schema_version": 1, "lock_after_secs": 600}),
            serde_json::json!({"schema_version": 1, "lock_after_secs": 600, "suspend_after_secs": 1800, "op": "unlock"}),
            serde_json::json!({"schema_version": 1, "lock_after_secs": "600", "suspend_after_secs": 1800}),
        ] {
            assert!(restore_preference("power", saved, &adapter).is_err());
        }
        peer.join().unwrap();
    }

    #[test]
    fn display_bridge_forwards_outputs_and_configure_output_and_refuses_everything_else() {
        const OUTPUTS: &[&str] = &[concat!(
            r#"{"reply":"outputs","outputs":[{"name":"Virtual-1","identity":"QEMU","#,
            r#""width":1280,"height":960,"refresh":60000,"x":0,"y":0,"scale":120,"#,
            r#""modes":[{"width":1280,"height":960,"refresh":60000}]}]}"#,
        )];
        const INPUT_CFG: &[&str] = &[
            r#"{"reply":"input_config","layout":"us","repeat_rate":25,"repeat_delay":200,"pointer_speed":0,"natural_scroll":false}"#,
        ];
        const POWER_CFG: &[&str] = &[
            r#"{"reply":"power_config","lock_after_secs":300,"suspend_after_secs":0}"#,
        ];
        let (compositor, peer) = scripted(vec![
            (r#"{"op":"outputs"}"#, OUTPUTS),
            (
                r#"{"name":"Virtual-1","op":"configure_output","scale":150}"#,
                &[r#"{"reply":"ok"}"#],
            ),
            (r#"{"op":"input_config"}"#, INPUT_CFG),
            (
                r#"{"layout":"gb","natural_scroll":true,"op":"configure_input","pointer_speed":4,"repeat_delay":300,"repeat_rate":35}"#,
                &[r#"{"reply":"ok"}"#],
            ),
            (r#"{"op":"power_config"}"#, POWER_CFG),
            (
                r#"{"lock_after_secs":600,"op":"configure_power","suspend_after_secs":1800}"#,
                &[r#"{"reply":"ok"}"#],
            ),
        ]);

        let listed = handle_display_request(r#"{"op":"outputs"}"#, &compositor);
        assert_eq!(listed["reply"], "outputs");
        assert_eq!(listed["outputs"][0]["name"], "Virtual-1");

        let configured = handle_display_request(
            r#"{"op":"configure_output","name":"Virtual-1","scale":150}"#,
            &compositor,
        );
        assert_eq!(configured["reply"], "ok");

        let input = handle_display_request(r#"{"op":"input_config"}"#, &compositor);
        assert_eq!(input["reply"], "input_config");
        assert_eq!(input["layout"], "us");

        let set_input = handle_display_request(
            r#"{"op":"configure_input","layout":"gb","repeat_rate":35,"repeat_delay":300,"pointer_speed":4,"natural_scroll":true}"#,
            &compositor,
        );
        assert_eq!(set_input["reply"], "ok");

        let power = handle_display_request(r#"{"op":"power_config"}"#, &compositor);
        assert_eq!(power["reply"], "power_config");
        assert_eq!(power["lock_after_secs"], 300);

        let set_power = handle_display_request(
            r#"{"op":"configure_power","lock_after_secs":600,"suspend_after_secs":1800}"#,
            &compositor,
        );
        assert_eq!(set_power["reply"], "ok");

        for disallowed in [
            r#"{"op":"snapshot"}"#,
            r#"{"op":"lock"}"#,
            r#"{"op":"unlock"}"#,
            r#"{"op":"focus_workspace","id":2}"#,
            r#"{"op":"focus_window","id":"0x1"}"#,
            r#"{"op":"outputs","extra":1}"#,
            r#"{"op":"input_config","extra":1}"#,
            r#"{"op":"power_config","extra":1}"#,
            r#"{"op":"configure_output","name":"Virtual-1","evil":true}"#,
            r#"{"op":"configure_output","name":"","scale":120}"#,
            r#"{"op":"configure_output","name":"Virtual-1","scale":"120"}"#,
            r#"{"op":"configure_input","evil":true}"#,
            r#"{"op":"configure_input","layout":""}"#,
            r#"{"op":"configure_input","natural_scroll":"yes"}"#,
            r#"{"op":"configure_power","evil":true}"#,
            r#"{"op":"configure_power","lock_after_secs":-5}"#,
        ] {
            let reply = handle_display_request(disallowed, &compositor);
            assert_eq!(
                reply["reply"], "error",
                "expected refusal for {disallowed}"
            );
        }

        peer.join().unwrap();
    }
}
