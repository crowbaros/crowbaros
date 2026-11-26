//! The launcher service, `crowbar-launcher`: it starts the apps the shell asks
//! for, as the person.
//!
//! runit starts it as root. It binds its socket in a directory root owns, so
//! no app can take the socket's place, and then becomes the person's account,
//! with its groups, before it reads a request. Every app it starts runs as
//! the person, with their home, at the compositor's socket for apps (ADR
//! 0032), as an app would on any desktop. It takes requests only from the
//! shell's user ID. Read-only requests give the shell the person's saved
//! appearance, Input, and Power preferences from fixed filenames.

use crate::request;
use crate::sys::{self, Account};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const SOCKET: &str = "/run/crowbar-launch/socket";
const SHELL: &str = "crowbar-shell";
const APPS_LOG: &str = "/var/log/crowbar-apps.log";
/// The compositor's socket for apps, as Base links it into the person's
/// runtime directory.
const WAYLAND_DISPLAY: &str = "wayland-0";
/// Where Void keeps the system's `LANG`.
const LOCALE: &str = "/etc/locale.conf";
const MAX_PREFERENCES: usize = 64 << 10;
/// How long `gio launch` may take to start an app.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(8);

/// Runs a launch command to its end, or says why it failed.
pub type Run<'a> = dyn FnMut(&mut Command) -> Result<(), String> + 'a;

/// The desktop entry at this full path, if it is a file.
pub fn desktop_path(value: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if !path.is_absolute() || path.extension().is_none_or(|e| e != "desktop") {
        return Err("Only a desktop entry, by its full path, can be launched".into());
    }
    if !std::fs::metadata(&path).map_err(|e| e.to_string())?.is_file() {
        return Err("A desktop entry must be a file".into());
    }
    Ok(path)
}

/// The person's saved appearance, ~/.config/crowbaros/appearance.json, or
/// None when there is none. No symbolic link is followed on the way, and the
/// file must be a regular one, the person's alone, of at most 64 KiB.
pub fn appearance(home: &Path, uid: u32) -> Result<Option<Value>, String> {
    preferences(home, uid, "appearance.json")
}

/// Reads one fixed preference filename with the same bounds and ownership
/// checks as appearance. Callers choose the filename, never a request.
fn preferences(home: &Path, uid: u32, filename: &str) -> Result<Option<Value>, String> {
    use std::os::fd::AsRawFd;
    let found = (|| -> io::Result<File> {
        let home = home.to_str().ok_or_else(|| io::Error::other("The home is not UTF-8"))?;
        let mut directory = sys::open_at(libc::AT_FDCWD, home, libc::O_RDONLY | libc::O_DIRECTORY)?;
        for name in [".config", "crowbaros"] {
            directory = sys::open_at(directory.as_raw_fd(), name, libc::O_RDONLY | libc::O_DIRECTORY)?;
        }
        let file = sys::open_at(directory.as_raw_fd(), filename, libc::O_RDONLY | libc::O_NONBLOCK)?;
        Ok(File::from(file))
    })();
    let file = match found {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        found => found.map_err(|e| e.to_string())?,
    };
    let info = file.metadata().map_err(|e| e.to_string())?;
    if !info.file_type().is_file() || info.uid() != uid || info.nlink() != 1 {
        return Err("Preferences must be a regular file owned only by the person".into());
    }
    let mut data = Vec::new();
    file.take(MAX_PREFERENCES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() > MAX_PREFERENCES {
        return Err("Preferences exceed 64 KiB".into());
    }
    serde_json::from_slice(&data).map(Some).map_err(|e| e.to_string())
}

/// The system's `LANG`, as Void's locale.conf sets it, or C.UTF-8.
pub fn system_locale(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter_map(|line| line.split_once('='))
        .find(|(name, value)| name.trim() == "LANG" && !value.trim().is_empty())
        .map(|(_, value)| value.trim().trim_matches('"').to_string())
        .unwrap_or_else(|| "C.UTF-8".into())
}

/// What a login gives the person, and what the desktop adds.
pub fn app_environment(person: &Account, locale: &str) -> Vec<(String, String)> {
    let home = person.dir.display();
    let shell = if person.shell.is_empty() {
        "/bin/sh"
    } else {
        &person.shell
    };
    [
        ("HOME", home.to_string()),
        ("USER", person.name.clone()),
        ("LOGNAME", person.name.clone()),
        ("SHELL", shell.into()),
        (
            "PATH",
            format!("{home}/.local/bin:/usr/local/sbin:/usr/local/bin:/usr/bin:/usr/sbin:/sbin:/bin"),
        ),
        ("LANG", locale.into()),
        ("XDG_RUNTIME_DIR", format!("/run/user/{}", person.uid)),
        ("XDG_CURRENT_DESKTOP", "CrowbarOS".into()),
        ("XDG_SESSION_TYPE", "wayland".into()),
        ("WAYLAND_DISPLAY", WAYLAND_DISPLAY.into()),
        ("BROWSER", "chromium".into()),
        ("XCURSOR_THEME", "Vanilla-DMZ".into()),
        ("XCURSOR_SIZE", "24".into()),
        ("CROWBAR_DISPLAY_SOCKET", "/run/crowbar-displays/socket".into()),
        ("CROWBAR_SESSION", "crowbaros".into()),
        ("RUST_LOG", "info".into()),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect()
}

/// A program as the launcher starts an app: with the person's environment
/// and nothing of this process's, starting in the person's home.
fn app_command(program: &str, person: &Account, locale: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .envs(app_environment(person, locale))
        .current_dir(&person.dir);
    command
}

/// `gio launch` for this entry, as the launcher itself, so as the person,
/// with the person's environment and nothing of the launcher's.
fn launch_command(entry: &Path, person: &Account) -> Command {
    let mut command = app_command("/usr/bin/gio", person, &system_locale(Path::new(LOCALE)));
    command.arg("launch").arg(entry).stdin(Stdio::null());
    command
}

/// `crowbar-launcher as-app PROGRAM [ARGS]`: the program, as the launcher
/// starts an app. It changes no account, so only a process that is already
/// the person's, with user ID `runner`, gets one.
pub fn as_app(command: &[String], runner: u32, person: &Account, locale: &str) -> Result<Command, String> {
    let Some((program, args)) = command.split_first() else {
        return Err("usage: crowbar-launcher as-app PROGRAM [ARGS]".into());
    };
    if runner != person.uid {
        return Err("Only the person's account runs a program as an app".into());
    }
    let mut command = app_command(program, person, locale);
    command.args(args);
    Ok(command)
}

fn person_tool(program: &str, person: &Account) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("HOME", &person.dir)
        .env("USER", &person.name)
        .env("LOGNAME", &person.name)
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("XDG_RUNTIME_DIR", format!("/run/user/{}", person.uid))
        .stdin(Stdio::null());
    command
}

fn parse_wpctl_volume(raw: &str) -> Option<(u32, bool)> {
    let rest = raw.trim().strip_prefix("Volume:")?.trim();
    let muted = rest.contains("[MUTED]");
    let num: f32 = rest.split_whitespace().next()?.parse().ok()?;
    if !num.is_finite() || num < 0.0 {
        return None;
    }
    Some(((num * 100.0).round().clamp(0.0, 150.0) as u32, muted))
}

fn parse_default_sink_name(status: &str) -> Option<String> {
    let mut in_audio = false;
    let mut in_sinks = false;
    for line in status.lines() {
        let plain = line.trim();
        if plain == "Audio" {
            in_audio = true;
            continue;
        }
        if plain == "Video" || plain == "Settings" {
            break;
        }
        if !in_audio {
            continue;
        }
        let stripped = line
            .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '│' | '├' | '└' | '─' | '|' | '-' | '`'));
        if stripped.starts_with("Sinks:") {
            in_sinks = true;
            continue;
        }
        if stripped.starts_with("Sources:")
            || stripped.starts_with("Devices:")
            || stripped.starts_with("Filters:")
            || stripped.starts_with("Streams:")
        {
            in_sinks = false;
            continue;
        }
        if in_sinks
            && let Some(after_star) = stripped.strip_prefix('*')
            && let Some((_, after_id)) = after_star.trim_start().split_once('.')
            && let Some((name_part, _)) = after_id.trim().rsplit_once("[vol:")
        {
            let name = name_part.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn query_audio(person: &Account) -> Value {
    let sink_out = person_tool("/usr/bin/wpctl", person)
        .args(["get-volume", "@DEFAULT_AUDIO_SINK@"])
        .output();
    let Ok(out) = sink_out else {
        return json!({"available": false});
    };
    if !out.status.success() {
        return json!({"available": false});
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Some((volume, muted)) = parse_wpctl_volume(&stdout) else {
        return json!({"available": false});
    };
    let sink_name = person_tool("/usr/bin/wpctl", person)
        .arg("status")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_default_sink_name(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_else(|| "Audio Output".into());
    let (mic_available, mic_muted) = person_tool("/usr/bin/wpctl", person)
        .args(["get-volume", "@DEFAULT_AUDIO_SOURCE@"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_wpctl_volume(&String::from_utf8_lossy(&o.stdout)))
        .map(|(_, m)| (true, m))
        .unwrap_or((false, false));
    json!({
        "available": true,
        "volume": volume,
        "muted": muted,
        "sink_name": sink_name,
        "mic_available": mic_available,
        "mic_muted": mic_muted,
    })
}

fn parse_dbus_objects_for_iface(raw: &str, target_iface: &str) -> Vec<(String, String)> {
    // Extracts (object_path, block_text) pairs for entries containing `target_iface`
    // from `dbus-send --print-reply ... GetManagedObjects`.
    let mut results = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_block = String::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("object path \"")
            && let Some(path) = rest.strip_suffix('"')
        {
            if let Some(prev) = current_path.take()
                && current_block.contains(target_iface)
            {
                results.push((prev, std::mem::take(&mut current_block)));
            } else {
                current_block.clear();
            }
            current_path = Some(path.to_string());
        } else if current_path.is_some() {
            current_block.push_str(trimmed);
            current_block.push('\n');
        }
    }
    if let Some(prev) = current_path
        && current_block.contains(target_iface)
    {
        results.push((prev, current_block));
    }
    results
}

fn extract_dbus_bool(block: &str, key: &str) -> Option<bool> {
    let needle = format!("string \"{key}\"");
    let mut lines = block.lines();
    while let Some(line) = lines.next() {
        if line.contains(&needle)
            && let Some(val_line) = lines.next()
        {
            if val_line.contains("boolean true") {
                return Some(true);
            }
            if val_line.contains("boolean false") {
                return Some(false);
            }
        }
    }
    None
}

fn extract_dbus_string(block: &str, key: &str) -> Option<String> {
    let needle = format!("string \"{key}\"");
    let mut lines = block.lines();
    while let Some(line) = lines.next() {
        if line.contains(&needle)
            && let Some(val_line) = lines.next()
            && let Some(start) = val_line.find("string \"")
        {
            let rest = &val_line[start + 8..];
            if let Some(end) = rest.rfind('"') {
                return Some(rest[..end].to_string());
            }
        }
    }
    None
}

fn query_wifi(person: &Account) -> Value {
    let Ok(out) = person_tool("/usr/bin/dbus-send", person)
        .args([
            "--system",
            "--print-reply",
            "--dest=net.connman.iwd",
            "/",
            "org.freedesktop.DBus.ObjectManager.GetManagedObjects",
        ])
        .output()
    else {
        return json!({"available": false});
    };
    if !out.status.success() {
        return json!({"available": false});
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let devices = parse_dbus_objects_for_iface(&text, "net.connman.iwd.Device");
    let Some((dev_path, dev_block)) = devices.into_iter().next() else {
        return json!({"available": false});
    };
    let powered = extract_dbus_bool(&dev_block, "Powered").unwrap_or(true);
    let connected_ssid = parse_dbus_objects_for_iface(&text, "net.connman.iwd.Network")
        .into_iter()
        .find(|(_, block)| extract_dbus_bool(block, "Connected") == Some(true))
        .and_then(|(_, block)| extract_dbus_string(&block, "Name"));
    json!({
        "available": true,
        "powered": powered,
        "ssid": connected_ssid,
        "device_path": dev_path,
    })
}

fn query_bluetooth(person: &Account) -> Value {
    let Ok(out) = person_tool("/usr/bin/dbus-send", person)
        .args([
            "--system",
            "--print-reply",
            "--dest=org.bluez",
            "/",
            "org.freedesktop.DBus.ObjectManager.GetManagedObjects",
        ])
        .output()
    else {
        return json!({"available": false});
    };
    if !out.status.success() {
        return json!({"available": false});
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let adapters = parse_dbus_objects_for_iface(&text, "org.bluez.Adapter1");
    let Some((adapter_path, block)) = adapters.into_iter().next() else {
        return json!({"available": false});
    };
    let powered = extract_dbus_bool(&block, "Powered").unwrap_or(false);
    json!({
        "available": true,
        "powered": powered,
        "adapter_path": adapter_path,
    })
}

fn read_backlight_dir(base: &Path) -> Option<(PathBuf, u32, u32)> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("brightness").is_file() && p.join("max_brightness").is_file())
        .collect();
    entries.sort();
    let dir = entries.into_iter().next()?;
    let cur: u32 = std::fs::read_to_string(dir.join("brightness"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let max: u32 = std::fs::read_to_string(dir.join("max_brightness"))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    if max == 0 {
        return None;
    }
    Some((dir, cur.min(max), max))
}

fn query_brightness_in(base: &Path) -> Value {
    let Some((dir, cur, max)) = read_backlight_dir(base) else {
        return json!({"available": false});
    };
    let percent = ((cur as f64 / max as f64) * 100.0).round().clamp(0.0, 100.0) as u32;
    let device = dir.file_name().and_then(|n| n.to_str()).unwrap_or("backlight");
    json!({
        "available": true,
        "percent": percent,
        "device": device,
    })
}

fn set_brightness_in(base: &Path, percent: u32) -> Result<Value, String> {
    let Some((dir, _, max)) = read_backlight_dir(base) else {
        return Err("No backlight device available".into());
    };
    let clamped = percent.clamp(5, 100);
    let raw = (((clamped as f64 / 100.0) * max as f64).round() as u32).clamp(1, max);
    std::fs::write(dir.join("brightness"), format!("{raw}\n"))
        .map_err(|e| format!("Could not adjust display brightness: {e}"))?;
    Ok(query_brightness_in(base))
}

fn set_dbus_bool_property(
    person: &Account,
    dest: &str,
    path: &str,
    iface: &str,
    prop: &str,
    value: bool,
) -> Result<(), String> {
    if !path.starts_with('/')
        || path.contains("..")
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_'))
    {
        return Err("Invalid D-Bus object path".into());
    }
    let out = person_tool("/usr/bin/dbus-send", person)
        .args([
            "--system",
            "--print-reply",
            &format!("--dest={dest}"),
            path,
            "org.freedesktop.DBus.Properties.Set",
            &format!("string:{iface}"),
            &format!("string:{prop}"),
            &format!("variant:boolean:{value}"),
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

fn dispatch(request: serde_json::Map<String, Value>, person: &Account, run: &mut Run) -> Result<Value, String> {
    let op = request.get("op").and_then(Value::as_str);
    match op {
        Some("appearance") if request.len() == 1 => {
            Ok(json!({"ok": true, "preferences": appearance(&person.dir, person.uid)?}))
        }
        Some("input_preferences") if request.len() == 1 => {
            Ok(json!({"ok": true, "preferences": preferences(&person.dir, person.uid, "input.json")?}))
        }
        Some("power_preferences") if request.len() == 1 => {
            Ok(json!({"ok": true, "preferences": preferences(&person.dir, person.uid, "power.json")?}))
        }
        Some("launch") if request.len() == 2 => {
            let entry = request
                .get("desktop")
                .and_then(Value::as_str)
                .ok_or("A desktop entry is a path")?;
            let entry = desktop_path(entry)?;
            run(&mut launch_command(&entry, person))?;
            Ok(json!({"ok": true}))
        }
        Some("quick_status") if request.len() == 1 => Ok(json!({
            "ok": true,
            "audio": query_audio(person),
            "wifi": query_wifi(person),
            "bluetooth": query_bluetooth(person),
            "brightness": query_brightness_in(Path::new("/sys/class/backlight")),
        })),
        Some("set_volume") if request.len() == 2 => {
            let pct = request
                .get("percent")
                .and_then(Value::as_u64)
                .ok_or("percent must be an integer 0..=100")?;
            let clamped = pct.min(100);
            let formatted = format!("{:.2}", clamped as f32 / 100.0);
            let out = person_tool("/usr/bin/wpctl", person)
                .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &formatted])
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err("Could not set volume".into());
            }
            Ok(json!({"ok": true, "audio": query_audio(person)}))
        }
        Some("step_volume") if request.len() == 2 => {
            let delta = request
                .get("delta")
                .and_then(Value::as_i64)
                .ok_or("delta must be an integer")?;
            let clamped_delta = delta.clamp(-25, 25);
            let current = query_audio(person)["volume"].as_u64().unwrap_or(50) as i64;
            let next = (current + clamped_delta).clamp(0, 100);
            let formatted = format!("{:.2}", next as f32 / 100.0);
            let out = person_tool("/usr/bin/wpctl", person)
                .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &formatted])
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err("Could not adjust volume".into());
            }
            Ok(json!({"ok": true, "audio": query_audio(person)}))
        }
        Some("toggle_mute") if request.len() == 1 => {
            let out = person_tool("/usr/bin/wpctl", person)
                .args(["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err("Could not toggle mute".into());
            }
            Ok(json!({"ok": true, "audio": query_audio(person)}))
        }
        Some("toggle_mic_mute") if request.len() == 1 => {
            let out = person_tool("/usr/bin/wpctl", person)
                .args(["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"])
                .output()
                .map_err(|e| e.to_string())?;
            if !out.status.success() {
                return Err("Could not toggle microphone mute".into());
            }
            Ok(json!({"ok": true, "audio": query_audio(person)}))
        }
        Some("set_brightness") if request.len() == 2 => {
            let pct = request
                .get("percent")
                .and_then(Value::as_u64)
                .ok_or("percent must be an integer 5..=100")? as u32;
            let brightness = set_brightness_in(Path::new("/sys/class/backlight"), pct)?;
            Ok(json!({"ok": true, "brightness": brightness}))
        }
        Some("step_brightness") if request.len() == 2 => {
            let delta = request
                .get("delta")
                .and_then(Value::as_i64)
                .ok_or("delta must be an integer")?;
            let current = query_brightness_in(Path::new("/sys/class/backlight"))["percent"]
                .as_u64()
                .ok_or("No backlight device available")? as i64;
            let next = (current + delta.clamp(-25, 25)).clamp(5, 100) as u32;
            let brightness = set_brightness_in(Path::new("/sys/class/backlight"), next)?;
            Ok(json!({"ok": true, "brightness": brightness}))
        }
        Some("set_wifi_power") if request.len() == 2 => {
            let powered = request
                .get("powered")
                .and_then(Value::as_bool)
                .ok_or("powered must be a boolean")?;
            let wifi = query_wifi(person);
            let path = wifi["device_path"].as_str().ok_or("No Wi-Fi adapter detected")?;
            set_dbus_bool_property(
                person,
                "net.connman.iwd",
                path,
                "net.connman.iwd.Device",
                "Powered",
                powered,
            )?;
            Ok(json!({"ok": true, "wifi": query_wifi(person)}))
        }
        Some("set_bluetooth_power") if request.len() == 2 => {
            let powered = request
                .get("powered")
                .and_then(Value::as_bool)
                .ok_or("powered must be a boolean")?;
            let bt = query_bluetooth(person);
            let path = bt["adapter_path"].as_str().ok_or("No Bluetooth adapter detected")?;
            set_dbus_bool_property(person, "org.bluez", path, "org.bluez.Adapter1", "Powered", powered)?;
            Ok(json!({"ok": true, "bluetooth": query_bluetooth(person)}))
        }
        Some("power") if request.len() == 2 => {
            let action = request
                .get("action")
                .and_then(Value::as_str)
                .ok_or("action must be suspend, reboot, or poweroff")?;
            let target = match action {
                "suspend" => "/usr/bin/zzz",
                "reboot" => "/usr/bin/reboot",
                "poweroff" => "/usr/bin/poweroff",
                _ => return Err("Unknown power action".into()),
            };
            let mut command = person_tool("/usr/bin/sudo", person);
            command.args(["-n", target]);
            run(&mut command)?;
            Ok(json!({"ok": true}))
        }
        _ => Err("Unknown request".into()),
    }
}

/// The reply to one connection from a peer with this user ID. Only the
/// shell's is read.
pub fn answer(peer: u32, shell_uid: u32, stream: &mut impl Read, person: &Account, run: &mut Run) -> Value {
    let reply = (|| {
        if peer != shell_uid {
            return Err("Only the shell may make a request".to_string());
        }
        dispatch(
            request::parse_object(&request::read_line(stream, request::MAX_REQUEST)?)?,
            person,
            run,
        )
    })();
    reply.unwrap_or_else(request::refusal)
}

/// Answers one connection on the launcher's socket.
pub fn serve_one(listener: &UnixListener, shell_uid: u32, person: &Account, run: &mut Run) {
    let Ok((mut connection, _)) = listener.accept() else {
        return;
    };
    let _ = connection.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = connection.set_write_timeout(Some(Duration::from_secs(2)));
    let reply = match sys::peer_uid(&connection) {
        Ok(peer) => answer(peer, shell_uid, &mut connection, person, run),
        Err(error) => request::refusal(error),
    };
    let _ = connection.write_all(format!("{reply}\n").as_bytes());
}

/// Runs a launch in a session of its own, its output to the apps' log, and
/// waits for it to finish, as `gio launch` does once the app has started.
fn run_launch(command: &mut Command) -> Result<(), String> {
    let log = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(APPS_LOG)
        .map_err(|e| e.to_string())?;
    command.stdout(log.try_clone().map_err(|e| e.to_string())?).stderr(log);
    sys::new_session(command);
    let mut child = command.spawn().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err("The application failed to launch".into())
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "The launch took longer than {} seconds",
                LAUNCH_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// `crowbar-launcher`, as runit starts it, or `crowbar-launcher as-app`, as
/// the Harness's service does.
pub fn main(args: &[String]) -> Result<(), String> {
    let uid = crate::PERSON_UID;
    if let Some(("as-app", command)) = args.split_first().map(|(first, rest)| (first.as_str(), rest)) {
        let person = sys::require(sys::account_by_uid(uid), &format!("with user ID {uid}"))?;
        let mut command = as_app(command, sys::uid(), &person, &system_locale(Path::new(LOCALE)))?;
        // Only returns when the program could not be run.
        let error = command.exec();
        return Err(format!("{:?}: {error}", command.get_program()));
    }
    if !args.is_empty() {
        return Err("usage: crowbar-launcher [as-app PROGRAM [ARGS]]".into());
    }
    let shell = sys::require(sys::account_by_name(SHELL), SHELL)?;
    let person = sys::require(sys::account_by_uid(uid), &format!("with user ID {uid}"))?;
    // Root's socket in root's directory, which the shell's group may enter:
    // the person, and so every app, may neither replace nor remove it. The
    // peer's user ID is the check that counts.
    let listener = sys::listen_as(Path::new(SOCKET), 0, shell.gid).map_err(|e| e.to_string())?;
    sys::become_account(&person).map_err(|e| format!("The launcher could not become the person: {e}"))?;
    loop {
        serve_one(&listener, shell.uid, &person, &mut run_launch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::tests::TempDir;
    use serde_json::json;
    use std::cell::RefCell;

    const SHELL_UID: u32 = 1001;

    fn person(home: &Path) -> Account {
        Account {
            name: "alex".into(),
            uid: crate::sys::uid(),
            gid: 0,
            dir: home.into(),
            shell: "/bin/bash".into(),
        }
    }

    /// The reply to `line` from `peer`, and each command the launcher ran.
    fn ask(line: &[u8], peer: u32, person: &Account) -> (serde_json::Value, Vec<String>) {
        let ran = RefCell::new(vec![]);
        let reply = answer(peer, SHELL_UID, &mut &line[..], person, &mut |command: &mut Command| {
            ran.borrow_mut().push(format!("{command:?}"));
            Ok(())
        });
        (reply, ran.into_inner())
    }

    struct Home {
        dir: TempDir,
    }

    impl Home {
        fn new() -> Home {
            let dir = TempDir::new();
            std::fs::create_dir_all(dir.0.join(".config/crowbaros")).unwrap();
            Home { dir }
        }
        fn path(&self) -> &Path {
            &self.dir.0
        }
        fn folder(&self) -> PathBuf {
            self.dir.0.join(".config/crowbaros")
        }
        fn read(&self, uid: u32) -> Result<Option<serde_json::Value>, String> {
            appearance(self.path(), uid)
        }
    }

    #[test]
    fn only_the_shell_may_make_a_request() {
        let home = Home::new();
        for peer in [0, 1003, 1000, 1002] {
            let (reply, ran) = ask(
                b"{\"op\":\"launch\",\"desktop\":\"/usr/share/applications/foot.desktop\"}\n",
                peer,
                &person(home.path()),
            );
            assert_eq!(
                reply,
                json!({"ok": false, "error": "Only the shell may make a request"}),
                "{peer}"
            );
            assert!(ran.is_empty());
        }
    }

    #[test]
    fn the_shell_reads_the_persons_appearance() {
        let home = Home::new();
        std::fs::write(home.folder().join("appearance.json"), r#"{"theme":"light"}"#).unwrap();
        let (reply, _) = ask(b"{\"op\":\"appearance\"}\n", SHELL_UID, &person(home.path()));
        assert_eq!(reply, json!({"ok": true, "preferences": {"theme": "light"}}));
    }

    #[test]
    fn saved_input_and_power_are_available_only_to_the_shell_from_fixed_files() {
        let home = Home::new();
        let person = person(home.path());
        for (op, filename, saved) in [
            (
                "input_preferences",
                "input.json",
                json!({"schema_version": 1, "layout": "gb"}),
            ),
            (
                "power_preferences",
                "power.json",
                json!({"schema_version": 1, "lock_after_secs": 600}),
            ),
        ] {
            let request = format!("{}\n", json!({"op": op}));
            let (reply, _) = ask(request.as_bytes(), SHELL_UID, &person);
            assert_eq!(reply, json!({"ok": true, "preferences": null}));
            std::fs::write(home.folder().join(filename), saved.to_string()).unwrap();
            let (reply, ran) = ask(request.as_bytes(), SHELL_UID, &person);
            assert_eq!(reply, json!({"ok": true, "preferences": saved}));
            assert!(ran.is_empty());
            // The test runner may itself have SHELL_UID (as on GitHub).
            // Keep the simulated app peer distinct from the shell.
            let (reply, _) = ask(request.as_bytes(), SHELL_UID + 1, &person);
            assert_eq!(reply["ok"], false);
            let request_with_path = format!("{}\n", json!({"op": op, "path": "/etc/shadow"}));
            let (reply, _) = ask(request_with_path.as_bytes(), SHELL_UID, &person);
            assert_eq!(reply["ok"], false);
            std::fs::remove_file(home.folder().join(filename)).unwrap();
            std::os::unix::fs::symlink("appearance.json", home.folder().join(filename)).unwrap();
            std::fs::write(home.folder().join("appearance.json"), "{}").unwrap();
            let (reply, _) = ask(request.as_bytes(), SHELL_UID, &person);
            assert_eq!(reply["ok"], false);
        }
    }

    #[test]
    fn a_launch_runs_gio_as_the_launcher_itself_at_the_persons_socket() {
        let home = Home::new();
        let entry = home.path().join("foot.desktop");
        std::fs::write(&entry, "[Desktop Entry]\n").unwrap();
        let line = format!("{}\n", json!({"op": "launch", "desktop": entry}));
        let mut seen = None;
        let reply = answer(
            SHELL_UID,
            SHELL_UID,
            &mut line.as_bytes(),
            &person(home.path()),
            &mut |command: &mut Command| {
                seen = Some((
                    command.get_program().to_owned(),
                    command.get_args().map(ToOwned::to_owned).collect::<Vec<_>>(),
                    command
                        .get_envs()
                        .map(|(k, v)| (k.to_owned(), v.map(ToOwned::to_owned)))
                        .collect::<Vec<_>>(),
                    command.get_current_dir().map(ToOwned::to_owned),
                ));
                Ok(())
            },
        );
        assert_eq!(reply, json!({"ok": true}));
        // As on any desktop: sudo and su work from the terminal, so the
        // launcher sets no other user or group for the app.
        let (program, args, env, dir) = seen.unwrap();
        assert_eq!(program, "/usr/bin/gio");
        assert_eq!(args, ["launch", entry.to_str().unwrap()]);
        assert!(env.contains(&("WAYLAND_DISPLAY".into(), Some("wayland-0".into()))));
        assert_eq!(dir.as_deref(), Some(home.path()));
    }

    #[test]
    fn a_failed_launch_is_reported() {
        let home = Home::new();
        let entry = home.path().join("broken.desktop");
        std::fs::write(&entry, "").unwrap();
        let line = format!("{}\n", json!({"op": "launch", "desktop": entry}));
        let reply = answer(
            SHELL_UID,
            SHELL_UID,
            &mut line.as_bytes(),
            &person(home.path()),
            &mut |_: &mut Command| Err("The application failed to launch".into()),
        );
        assert_eq!(reply, json!({"ok": false, "error": "The application failed to launch"}));
    }

    #[test]
    fn a_malformed_or_unknown_request_is_refused() {
        let home = Home::new();
        let long = [b"x".repeat(5000), b"\n".to_vec()].concat();
        for line in [
            b"{\"op\":\"appearance\"}".as_slice(),
            b"[]\n",
            b"{\"op\":\"run\",\"command\":\"sh\"}\n",
            b"{\"op\":\"launch\",\"desktop\":\"/usr/share/applications/foot.desktop\",\"uid\":0}\n",
            b"{\"op\":\"launch\",\"desktop\":1}\n",
            b"{\"op\":\"appearance\",\"path\":\"/etc/shadow\"}\n",
            &long,
        ] {
            let (reply, ran) = ask(line, SHELL_UID, &person(home.path()));
            assert_eq!(
                reply["ok"],
                false,
                "{}",
                String::from_utf8_lossy(&line[..line.len().min(40)])
            );
            assert!(ran.is_empty());
        }
    }

    #[test]
    fn absent_and_regular_preferences() {
        let home = Home::new();
        assert_eq!(home.read(crate::sys::uid()), Ok(None));
        std::fs::write(home.folder().join("appearance.json"), r#"{"theme":"light"}"#).unwrap();
        assert_eq!(home.read(crate::sys::uid()), Ok(Some(json!({"theme": "light"}))));
    }

    #[test]
    fn a_file_symlink_cannot_make_the_launcher_read_another_file() {
        let home = Home::new();
        let secret = home.path().join("unrelated.json");
        std::fs::write(&secret, r#"{"secret":"must not escape"}"#).unwrap();
        std::os::unix::fs::symlink(&secret, home.folder().join("appearance.json")).unwrap();
        assert!(home.read(crate::sys::uid()).is_err());
    }

    #[test]
    fn a_directory_symlink_cannot_escape_the_preferences_directory() {
        let home = Home::new();
        std::fs::remove_dir(home.folder()).unwrap();
        let outside = home.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("appearance.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&outside, home.folder()).unwrap();
        assert!(home.read(crate::sys::uid()).is_err());
    }

    #[test]
    fn a_fifo_or_an_oversized_file_is_refused_without_blocking() {
        let home = Home::new();
        let file = home.folder().join("appearance.json");
        crate::sys::tests::mkfifo(&file);
        assert!(home.read(crate::sys::uid()).is_err());
        std::fs::remove_file(&file).unwrap();
        std::fs::write(&file, " ".repeat(65537)).unwrap();
        assert!(home.read(crate::sys::uid()).is_err());
    }

    #[test]
    fn preferences_cannot_be_a_different_owners_file_or_a_hard_link() {
        let home = Home::new();
        let file = home.folder().join("appearance.json");
        std::fs::write(&file, "{}").unwrap();
        assert!(home.read(crate::sys::uid() + 1).is_err());
        std::fs::hard_link(&file, home.path().join("second-link.json")).unwrap();
        assert!(home.read(crate::sys::uid()).is_err());
    }

    #[test]
    fn any_desktop_entry_launches_by_its_full_path() {
        let home = Home::new();
        let entry = home.path().join(".local/share/applications/mine.desktop");
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(&entry, "[Desktop Entry]\n").unwrap();
        assert_eq!(desktop_path(entry.to_str().unwrap()), Ok(entry.clone()));
        let script = home.path().join("script.sh");
        std::fs::write(&script, "").unwrap();
        for path in [
            "mine.desktop",
            script.to_str().unwrap(),
            entry.parent().unwrap().to_str().unwrap(),
        ] {
            assert!(desktop_path(path).is_err(), "{path}");
        }
        let folder = home.path().join("x.desktop");
        std::fs::rename(entry.parent().unwrap(), &folder).unwrap();
        assert!(desktop_path(folder.to_str().unwrap()).is_err());
    }

    fn environment(person: &Account) -> std::collections::HashMap<String, String> {
        app_environment(person, "en_GB.UTF-8").into_iter().collect()
    }

    #[test]
    fn an_app_gets_the_cursor_theme_and_size() {
        let env = environment(&person(Path::new("/home/alex")));
        assert_eq!(env["XCURSOR_THEME"], "Vanilla-DMZ");
        assert_eq!(env["XCURSOR_SIZE"], "24");
    }

    #[test]
    fn an_app_learns_it_is_on_crowbaros_desktop() {
        let env = environment(&person(Path::new("/home/alex")));
        assert_eq!(env["XDG_CURRENT_DESKTOP"], "CrowbarOS");
        assert_eq!(env["CROWBAR_SESSION"], "crowbaros");
    }

    #[test]
    fn an_app_is_the_person_at_the_ordinary_wayland_socket() {
        let mut alex = person(Path::new("/home/alex"));
        alex.uid = 1003;
        let env = environment(&alex);
        assert_eq!(
            [&env["HOME"], &env["USER"], &env["LOGNAME"], &env["SHELL"]],
            ["/home/alex", "alex", "alex", "/bin/bash"]
        );
        assert_eq!(
            [&env["XDG_RUNTIME_DIR"], &env["WAYLAND_DISPLAY"]],
            ["/run/user/1003", "wayland-0"]
        );
        assert!(env["PATH"].starts_with("/home/alex/.local/bin:"));
        assert_eq!(env["LANG"], "en_GB.UTF-8");
        alex.shell = String::new();
        assert_eq!(environment(&alex)["SHELL"], "/bin/sh");
    }

    #[test]
    fn a_program_run_as_an_app_gets_the_apps_environment_alone_and_starts_in_the_persons_home() {
        let home = TempDir::new();
        let alex = person(&home.0);
        let run = |program: &str| {
            let command = [program.to_string()];
            let out = as_app(&command, alex.uid, &alex, "en_GB.UTF-8")
                .unwrap()
                .output()
                .unwrap();
            assert!(out.status.success(), "{program}: {out:?}");
            String::from_utf8(out.stdout).unwrap()
        };
        let mut got: Vec<String> = run("/usr/bin/env").lines().map(str::to_string).collect();
        let mut app: Vec<String> = app_environment(&alex, "en_GB.UTF-8")
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        got.sort();
        app.sort();
        assert_eq!(got, app);
        assert!(got.contains(&"CROWBAR_SESSION=crowbaros".to_string()));
        assert_eq!(Path::new(run("/bin/pwd").trim_end()), home.0.canonicalize().unwrap());
    }

    #[test]
    fn a_program_run_as_an_app_is_given_its_arguments() {
        let home = TempDir::new();
        let alex = person(&home.0);
        let command = ["/bin/echo", "--state", "a b"].map(str::to_string);
        let out = as_app(&command, alex.uid, &alex, "C.UTF-8").unwrap().output().unwrap();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), "--state a b\n");
    }

    #[test]
    fn only_the_person_runs_a_program_as_an_app_and_only_a_program() {
        // The build runs these tests as root, so the person's user ID is
        // given here, and is not the one the tests run as.
        let mut alex = person(Path::new("/home/alex"));
        alex.uid = 1003;
        let env = ["/usr/bin/env".to_string()];
        assert!(as_app(&env, 1003, &alex, "C.UTF-8").is_ok());
        assert!(as_app(&env, 1001, &alex, "C.UTF-8").is_err());
        assert!(as_app(&env, 0, &alex, "C.UTF-8").is_err());
        assert!(as_app(&[], 1003, &alex, "C.UTF-8").is_err());
    }

    #[test]
    fn an_app_gets_the_systems_locale() {
        let dir = TempDir::new();
        let conf = dir.0.join("locale.conf");
        std::fs::write(&conf, "# comment\nLANG=\"en_GB.UTF-8\"\n").unwrap();
        assert_eq!(system_locale(&conf), "en_GB.UTF-8");
        assert_eq!(system_locale(&dir.0.join("missing")), "C.UTF-8");
    }

    #[test]
    fn over_its_socket_the_launcher_refuses_anyone_but_the_shell() {
        let home = Home::new();
        let dir = TempDir::new();
        let path = dir.0.join("socket");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let asker = std::thread::spawn(move || {
            let mut stream = std::os::unix::net::UnixStream::connect(&path).unwrap();
            let _ = stream.write_all(b"{\"op\":\"appearance\"}\n");
            let mut reply = String::new();
            stream.read_to_string(&mut reply).ok().map(|_| reply)
        });
        let mut ran = 0;
        serve_one(
            &listener,
            crate::sys::uid() + 1,
            &person(home.path()),
            &mut |_: &mut Command| {
                ran += 1;
                Ok(())
            },
        );
        if let Some(reply) = asker.join().unwrap() {
            assert_eq!(serde_json::from_str::<serde_json::Value>(&reply).unwrap()["ok"], false);
        }
        assert_eq!(ran, 0);
    }

    #[test]
    fn quick_settings_parsers_and_backlight_helpers() {
        assert_eq!(parse_wpctl_volume("Volume: 0.65\n"), Some((65, false)));
        assert_eq!(parse_wpctl_volume("Volume: 0.40 [MUTED]\n"), Some((40, true)));
        assert_eq!(parse_wpctl_volume("bad"), None);

        let status = "Audio\n ├─ Sinks:\n │  *   32. Dummy Output [vol: 1.00]\n ├─ Sources:\n";
        assert_eq!(parse_default_sink_name(status).as_deref(), Some("Dummy Output"));

        let dir = TempDir::new();
        assert_eq!(query_brightness_in(&dir.0)["available"], false);
        let bl = dir.0.join("intel_backlight");
        std::fs::create_dir_all(&bl).unwrap();
        std::fs::write(bl.join("max_brightness"), "200\n").unwrap();
        std::fs::write(bl.join("brightness"), "100\n").unwrap();
        let state = query_brightness_in(&dir.0);
        assert_eq!(state["available"], true);
        assert_eq!(state["percent"], 50);
        let updated = set_brightness_in(&dir.0, 75).unwrap();
        assert_eq!(updated["percent"], 75);
        assert_eq!(std::fs::read_to_string(bl.join("brightness")).unwrap().trim(), "150");
    }

    #[test]
    fn power_op_only_accepts_bounded_actions_from_the_shell() {
        let home = Home::new();
        let (reply, ran) = ask(
            b"{\"op\":\"power\",\"action\":\"suspend\"}\n",
            SHELL_UID,
            &person(home.path()),
        );
        assert_eq!(reply, json!({"ok": true}));
        assert_eq!(ran.len(), 1);
        assert!(ran[0].contains("/usr/bin/zzz"));

        let (bad_action, ran_bad) = ask(
            b"{\"op\":\"power\",\"action\":\"rm -rf /\"}\n",
            SHELL_UID,
            &person(home.path()),
        );
        assert_eq!(bad_action["ok"], false);
        assert!(ran_bad.is_empty());

        let (non_shell, ran_non_shell) = ask(
            b"{\"op\":\"power\",\"action\":\"reboot\"}\n",
            1003,
            &person(home.path()),
        );
        assert_eq!(non_shell["ok"], false);
        assert!(ran_non_shell.is_empty());
    }
}
