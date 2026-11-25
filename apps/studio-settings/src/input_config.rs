// SPDX-License-Identifier: MIT
//! Keyboard and pointer input settings persistence and live Fulcrum application.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const LAYOUTS: [(&str, &str); 8] = [
    ("us", "English (US)"),
    ("gb", "English (UK)"),
    ("de", "German"),
    ("fr", "French"),
    ("es", "Spanish"),
    ("ca", "Canadian"),
    ("se", "Swedish"),
    ("jp", "Japanese"),
];

pub const REPEAT_RATES: [i32; 5] = [15, 25, 35, 50, 65];
pub const REPEAT_DELAYS: [i32; 5] = [150, 200, 300, 450, 600];
pub const POINTER_SPEEDS: [(i32, &str); 5] = [
    (-6, "Slow"),
    (-3, "Slower"),
    (0, "Default"),
    (4, "Faster"),
    (8, "Fast"),
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputSettings {
    pub schema_version: u32,
    pub layout: String,
    pub repeat_rate: i32,
    pub repeat_delay: i32,
    pub pointer_speed: i32,
    pub natural_scroll: bool,
}

impl Default for InputSettings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            layout: "us".into(),
            repeat_rate: 25,
            repeat_delay: 200,
            pointer_speed: 0,
            natural_scroll: false,
        }
    }
}

impl InputSettings {
    pub fn clamped(mut self) -> Self {
        if self.layout.trim().is_empty() {
            self.layout = "us".into();
        }
        self.repeat_rate = self.repeat_rate.clamp(10, 80);
        self.repeat_delay = self.repeat_delay.clamp(150, 1000);
        self.pointer_speed = self.pointer_speed.clamp(-10, 10);
        self.schema_version = 1;
        self
    }
}

pub fn config_path() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| PathBuf::from(p).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .ok_or("Cannot find your configuration directory.")?;
    Ok(base.join("crowbaros/input.json"))
}

pub fn load_from(path: &Path) -> Result<Option<InputSettings>, String> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("Could not read input settings: {e}")),
    };
    let parsed: InputSettings =
        serde_json::from_str(&text).map_err(|e| format!("Invalid input settings: {e}"))?;
    if parsed.schema_version != 1 {
        return Err(format!("Unsupported input schema {}", parsed.schema_version));
    }
    Ok(Some(parsed.clamped()))
}

pub fn save_to(path: &Path, settings: &InputSettings) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid input settings path.")?;
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
    let tmp = parent.join(format!(".input.json.tmp.{}.{stamp}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("Could not stage input settings: {e}"))?;
    file.write_all(json.as_bytes())
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.sync_all())
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("Could not write input settings: {e}")
        })?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Could not save input settings: {e}")
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
            .unwrap_or("Fulcrum rejected input change.")
            .to_string());
    }
    Ok(reply)
}

pub fn load_live_or_saved() -> (InputSettings, bool) {
    if let Ok(reply) = bridge_request(serde_json::json!({"op": "input_config"}))
        && reply["reply"] == "input_config"
    {
        let live = InputSettings {
            schema_version: 1,
            layout: reply["layout"].as_str().unwrap_or("us").to_string(),
            repeat_rate: reply["repeat_rate"].as_i64().unwrap_or(25) as i32,
            repeat_delay: reply["repeat_delay"].as_i64().unwrap_or(200) as i32,
            pointer_speed: reply["pointer_speed"].as_i64().unwrap_or(0) as i32,
            natural_scroll: reply["natural_scroll"].as_bool().unwrap_or(false),
        }
        .clamped();
        return (live, true);
    }
    let saved = config_path().ok().and_then(|p| load_from(&p).ok()).flatten();
    (saved.unwrap_or_default(), false)
}

pub fn apply_and_save(settings: &InputSettings) -> Result<bool, String> {
    let clean = settings.clone().clamped();
    let live_applied = match bridge_request(serde_json::json!({
        "op": "configure_input",
        "layout": clean.layout,
        "repeat_rate": clean.repeat_rate,
        "repeat_delay": clean.repeat_delay,
        "pointer_speed": clean.pointer_speed,
        "natural_scroll": clean.natural_scroll,
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
    fn input_settings_roundtrip_and_clamp() {
        let dir = std::env::temp_dir().join(format!("crowbar-input-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("crowbaros/input.json");
        assert_eq!(load_from(&path).unwrap(), None);

        let raw = InputSettings {
            schema_version: 1,
            layout: "de".into(),
            repeat_rate: 999,
            repeat_delay: 50,
            pointer_speed: -99,
            natural_scroll: true,
        };
        save_to(&path, &raw).unwrap();
        let loaded = load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.layout, "de");
        assert_eq!(loaded.repeat_rate, 80);
        assert_eq!(loaded.repeat_delay, 150);
        assert_eq!(loaded.pointer_speed, -10);
        assert!(loaded.natural_scroll);
        let _ = fs::remove_dir_all(&dir);
    }
}
