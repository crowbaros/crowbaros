// SPDX-License-Identifier: MIT
//! Version-specific Hyprland adapter. Only fixed commands and boolean data cross IPC.
use crate::settings::Motion;
use std::{
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn command(args: &[&str]) -> Result<String, String> {
    let mut child = Command::new("/usr/bin/hyprctl")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot contact the desktop: {e}"))?;
    let start = Instant::now();
    loop {
        if child.try_wait().map_err(|e| e.to_string())?.is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(2) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(
                "The desktop did not respond. Your preferences were not saved; try again.".into(),
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(
            "Could not update desktop motion. Check that Hyprland is running, then try again."
                .into(),
        );
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

pub struct Hyprland {
    instance: String,
}

/// The compositor's start event can precede its IPC/instance discovery readiness.
/// Retry only session restoration; interactive saves still fail promptly.
pub fn restore_motion(reduced: bool) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match Hyprland::connect().and_then(|session| session.set_reduced(reduced)) {
            Ok(()) => {
                log::info!("STUDIO event=session-restored reduced_motion={reduced}");
                return Ok(());
            }
            Err(error) if Instant::now() >= deadline => {
                return Err(format!("Could not restore saved desktop motion: {error}"));
            }
            Err(_) => thread::sleep(Duration::from_millis(200)),
        }
    }
}

impl Hyprland {
    pub fn connect() -> Result<Self, String> {
        let instances: serde_json::Value =
            serde_json::from_str(&command(&["instances", "-j"])?).map_err(|e| e.to_string())?;
        let display = std::env::var("WAYLAND_DISPLAY")
            .map_err(|_| "No Wayland desktop is available.".to_string())?;
        let matches: Vec<_> = instances
            .as_array()
            .ok_or("Invalid desktop response.")?
            .iter()
            .filter(|i| i["wl_socket"].as_str() == Some(&display))
            .collect();
        if matches.len() != 1 {
            return Err(
                "Could not identify this Hyprland session. Preferences were not saved.".into(),
            );
        }
        Ok(Self {
            instance: matches[0]["instance"]
                .as_str()
                .ok_or("Missing desktop session ID.")?
                .to_string(),
        })
    }
}
impl Motion for Hyprland {
    fn reduced(&self) -> Result<bool, String> {
        let output = command(&[
            "-i",
            &self.instance,
            "getoption",
            "animations:enabled",
            "-j",
        ])?;
        let value: serde_json::Value = serde_json::from_str(&output).map_err(|e| e.to_string())?;
        value["bool"]
            .as_bool()
            .map(|enabled| !enabled)
            .ok_or("The desktop's animation API is not supported by this version.".into())
    }
    fn set_reduced(&self, reduced: bool) -> Result<(), String> {
        let expression = if reduced {
            "hl.config({animations={enabled=false}})"
        } else {
            "hl.config({animations={enabled=true}})"
        };
        let result = command(&["-i", &self.instance, "eval", expression])?;
        if result.trim() != "ok" {
            return Err(
                "The desktop rejected the motion setting. Preferences were not saved.".into(),
            );
        }
        if self.reduced()? != reduced {
            return Err(
                "The desktop did not apply the motion setting. Preferences were not saved.".into(),
            );
        }
        Ok(())
    }
}
