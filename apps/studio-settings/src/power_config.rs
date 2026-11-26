// SPDX-License-Identifier: MIT
//! Power & Lock idle timeout settings persistence and live Fulcrum wiring.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const LOCK_TIMEOUTS: [(u32, &str); 6] = [
    (0, "Never"),
    (120, "2 minutes"),
    (300, "5 minutes"),
    (600, "10 minutes"),
    (900, "15 minutes"),
    (1800, "30 minutes"),
];

pub const SUSPEND_TIMEOUTS: [(u32, &str); 5] = [
    (0, "Never"),
    (900, "15 minutes"),
    (1800, "30 minutes"),
    (3600, "1 hour"),
    (7200, "2 hours"),
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerSettings {
    pub schema_version: u32,
    pub lock_after_secs: u32,
    pub suspend_after_secs: u32,
}

impl Default for PowerSettings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            lock_after_secs: 300,
            suspend_after_secs: 0,
        }
    }
}

impl PowerSettings {
    pub fn clamped(mut self) -> Self {
        self.schema_version = 1;
        self.lock_after_secs = self.lock_after_secs.min(7200);
        self.suspend_after_secs = self.suspend_after_secs.min(14400);
        self
    }
}

pub fn format_timeout(secs: u32) -> String {
    if secs == 0 {
        "Never".into()
    } else if secs < 60 {
        format!("{secs} seconds")
    } else if secs.is_multiple_of(3600) {
        let h = secs / 3600;
        if h == 1 {
            "1 hour".into()
        } else {
            format!("{h} hours")
        }
    } else {
        let m = secs / 60;
        if m == 1 {
            "1 minute".into()
        } else {
            format!("{m} minutes")
        }
    }
}

pub fn config_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| PathBuf::from(p).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .ok_or("Cannot find your configuration directory.")?;
    Ok(base.join("crowbaros/power.json"))
}

pub fn load_from(path: &Path) -> Result<Option<PowerSettings>, String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Could not read power settings: {e}")),
    };
    let parsed: PowerSettings =
        serde_json::from_str(&text).map_err(|e| format!("Invalid power settings: {e}"))?;
    if parsed.schema_version != 1 {
        return Err(format!("Unsupported power schema {}", parsed.schema_version));
    }
    Ok(Some(parsed.clamped()))
}

pub fn save_to(path: &Path, settings: &PowerSettings) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid power settings path.")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|e| format!("Could not create config directory: {e}"))?;
    let clean = settings.clone().clamped();
    let json = serde_json::to_string_pretty(&clean).map_err(|e| e.to_string())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = parent.join(format!(".power.json.tmp.{}.{stamp}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("Could not stage power settings: {e}"))?;
    file.write_all(json.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("Could not write power settings: {e}")
        })?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Could not save power settings: {e}")
    })
}

fn bridge_request(request: serde_json::Value) -> Result<serde_json::Value, String> {
    let path = std::env::var_os("CROWBAR_DISPLAY_SOCKET")
        .ok_or_else(|| "Fulcrum settings bridge is not available in this session.".to_string())?;
    let mut stream = UnixStream::connect(path)
        .map_err(|e| format!("Could not connect to Fulcrum settings bridge: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(4)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let mut line = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
    line.push(b'\n');
    stream.write_all(&line).map_err(|e| e.to_string())?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .map_err(|e| e.to_string())?;
    let reply: serde_json::Value = serde_json::from_str(&response).map_err(|e| e.to_string())?;
    if reply["reply"] == "error" {
        return Err(reply["message"]
            .as_str()
            .unwrap_or("Fulcrum rejected power change.")
            .to_string());
    }
    Ok(reply)
}

pub fn load_live_or_saved() -> (PowerSettings, bool) {
    if let Ok(reply) = bridge_request(serde_json::json!({"op": "power_config"}))
        && reply["reply"] == "power_config"
    {
        let live = PowerSettings {
            schema_version: 1,
            lock_after_secs: reply["lock_after_secs"].as_u64().unwrap_or(0) as u32,
            suspend_after_secs: reply["suspend_after_secs"].as_u64().unwrap_or(0) as u32,
        }
        .clamped();
        return (live, true);
    }
    let saved = config_path().ok().and_then(|p| load_from(&p).ok()).flatten();
    (saved.unwrap_or_default(), false)
}

pub fn apply_and_save(settings: &PowerSettings) -> Result<bool, String> {
    let clean = settings.clone().clamped();
    let live_applied = match bridge_request(serde_json::json!({
        "op": "configure_power",
        "lock_after_secs": clean.lock_after_secs,
        "suspend_after_secs": clean.suspend_after_secs,
    })) {
        Ok(_) => true,
        Err(_) if std::env::var_os("CROWBAR_DISPLAY_SOCKET").is_none() => false,
        Err(e) => return Err(e),
    };
    let path = config_path()?;
    save_to(&path, &clean)?;
    Ok(live_applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_settings_roundtrip_and_timeout_formatting() {
        let dir = std::env::temp_dir().join(format!("crowbar-power-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("crowbaros/power.json");
        assert_eq!(load_from(&path).unwrap(), None);

        let raw = PowerSettings {
            schema_version: 1,
            lock_after_secs: 600,
            suspend_after_secs: 1800,
        };
        save_to(&path, &raw).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded, raw);
        assert_eq!(format_timeout(0), "Never");
        assert_eq!(format_timeout(300), "5 minutes");
        assert_eq!(format_timeout(3600), "1 hour");
        let _ = fs::remove_dir_all(&dir);
    }
}
