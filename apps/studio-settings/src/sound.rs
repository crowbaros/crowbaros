// SPDX-License-Identifier: MIT
//! PipeWire / WirePlumber audio inspection, mutation, and event watching.
//!
//! Uses WirePlumber's `wpctl` for perceptual volume/mute/default queries and
//! mutations, and `pw-mon` to detect external audio graph changes (such as
//! keyboard media keys or the shell) without waking the UI while idle.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct AudioEndpoint {
    pub id: u32,
    pub name: String,
    /// Perceptual linear volume where `0.0` is 0% and `1.0` is 100%.
    pub volume: f32,
    pub muted: bool,
    pub is_default: bool,
}

impl AudioEndpoint {
    pub fn volume_percent(&self) -> u32 {
        (self.volume * 100.0).round().clamp(0.0, 150.0) as u32
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SoundSnapshot {
    pub sinks: Vec<AudioEndpoint>,
    pub sources: Vec<AudioEndpoint>,
}

impl SoundSnapshot {
    pub fn default_sink(&self) -> Option<&AudioEndpoint> {
        self.sinks
            .iter()
            .find(|s| s.is_default)
            .or_else(|| self.sinks.first())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundError {
    Unavailable(String),
    Failed(String),
}

impl SoundError {
    pub fn message(&self) -> &str {
        match self {
            Self::Unavailable(msg) | Self::Failed(msg) => msg,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Audio,
    Sinks,
    Sources,
}

fn strip_tree_prefix(line: &str) -> &str {
    line.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '│' | '├' | '└' | '─' | '|' | '-' | '`')
    })
}

fn parse_endpoint_line(line: &str) -> Option<AudioEndpoint> {
    let trimmed = strip_tree_prefix(line);
    if trimmed.is_empty() {
        return None;
    }
    let (is_default, rest) = if let Some(after_star) = trimmed.strip_prefix('*') {
        (true, after_star.trim_start())
    } else {
        (false, trimmed)
    };
    let (id_str, after_id) = rest.split_once('.')?;
    let id: u32 = id_str.trim().parse().ok()?;
    let after_id = after_id.trim();

    let (name_part, vol_part) = after_id.rsplit_once("[vol:")?;
    let name = name_part.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let inner = vol_part.trim().trim_end_matches(']').trim();
    let muted = inner.contains("MUTED");
    let vol_token = inner.split_whitespace().next()?;
    let volume: f32 = vol_token.parse().ok()?;
    if !volume.is_finite() || volume < 0.0 {
        return None;
    }

    Some(AudioEndpoint {
        id,
        name,
        volume: clamp_volume(volume),
        muted,
        is_default,
    })
}

pub fn clamp_volume(volume: f32) -> f32 {
    if !volume.is_finite() {
        return 0.0;
    }
    volume.clamp(0.0, 1.5)
}

pub fn format_wpctl_volume(volume: f32) -> String {
    format!("{:.2}", clamp_volume(volume))
}

pub fn parse_wpctl_status(raw: &str) -> Result<SoundSnapshot, SoundError> {
    if !raw.contains("PipeWire") && !raw.contains("Audio") {
        return Err(SoundError::Unavailable(
            "PipeWire audio service is not running.".into(),
        ));
    }

    let mut section = Section::None;
    let mut sinks = Vec::new();
    let mut sources = Vec::new();

    for line in raw.lines() {
        let plain = line.trim();
        if plain == "Audio" {
            section = Section::Audio;
            continue;
        }
        if plain == "Video" || plain == "Settings" {
            section = Section::None;
            continue;
        }
        if section == Section::None {
            continue;
        }
        let stripped = strip_tree_prefix(line);
        if stripped.starts_with("Sinks:") {
            section = Section::Sinks;
            continue;
        }
        if stripped.starts_with("Sources:") {
            section = Section::Sources;
            continue;
        }
        if stripped.starts_with("Devices:")
            || stripped.starts_with("Filters:")
            || stripped.starts_with("Streams:")
        {
            section = Section::Audio;
            continue;
        }

        match section {
            Section::Sinks => {
                if let Some(ep) = parse_endpoint_line(line) {
                    sinks.push(ep);
                }
            }
            Section::Sources => {
                if let Some(ep) = parse_endpoint_line(line) {
                    sources.push(ep);
                }
            }
            _ => {}
        }
    }

    Ok(SoundSnapshot { sinks, sources })
}

pub fn fetch_sound() -> Result<SoundSnapshot, SoundError> {
    let output = Command::new("wpctl").arg("status").output().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            SoundError::Unavailable("WirePlumber (`wpctl`) is not installed.".into())
        } else {
            SoundError::Unavailable(format!("Could not run `wpctl`: {err}"))
        }
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Could not connect")
            || stderr.contains("No such file")
            || stderr.trim().is_empty()
        {
            return Err(SoundError::Unavailable(
                "PipeWire audio service is not running.".into(),
            ));
        }
        return Err(SoundError::Failed(format!(
            "Could not query PipeWire audio state: {}",
            stderr.trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_wpctl_status(&stdout)
}

pub fn set_default(id: u32) -> Result<(), SoundError> {
    run_wpctl(&["set-default", &id.to_string()])
}

pub fn set_volume(id: u32, volume: f32) -> Result<(), SoundError> {
    let formatted = format_wpctl_volume(volume);
    run_wpctl(&["set-volume", &id.to_string(), &formatted])
}

pub fn set_mute(id: u32, muted: bool) -> Result<(), SoundError> {
    run_wpctl(&["set-mute", &id.to_string(), if muted { "1" } else { "0" }])
}

fn run_wpctl(args: &[&str]) -> Result<(), SoundError> {
    let output = Command::new("wpctl")
        .args(args)
        .output()
        .map_err(|e| SoundError::Failed(format!("Could not invoke `wpctl`: {e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        Err(SoundError::Failed(if detail.is_empty() {
            "PipeWire rejected the audio change.".into()
        } else {
            format!("PipeWire rejected the audio change: {detail}")
        }))
    }
}

/// Spawns an event-driven watcher (`pw-mon --no-colors`) that sends updated
/// [`SoundSnapshot`] values only when the parsed audio state actually changes.
/// While audio is idle, `pw-mon` produces no output and the UI receives zero
/// wakeups.
pub fn spawn_sound_watcher(tx: Sender<SoundSnapshot>) {
    let _ = thread::Builder::new()
        .name("crowbar-sound-watch".into())
        .spawn(move || {
            let Ok(mut child) = Command::new("pw-mon")
                .arg("--no-colors")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            else {
                return;
            };
            let Some(stdout) = child.stdout.take() else {
                let _ = child.kill();
                return;
            };
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let mut last_snapshot = fetch_sound().ok();
            let mut last_check = Instant::now();

            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let trimmed = line.trim();
                        // Only react to meaningful graph/property change lines, debounced.
                        if !(trimmed.starts_with("changed:")
                            || trimmed.starts_with("added:")
                            || trimmed.starts_with("removed:")
                            || trimmed.contains("default.audio.")
                            || trimmed.contains("Props:"))
                        {
                            continue;
                        }
                        if last_check.elapsed() < Duration::from_millis(120) {
                            continue;
                        }
                        last_check = Instant::now();
                        if let Ok(current) = fetch_sound()
                            && last_snapshot.as_ref() != Some(&current)
                        {
                            last_snapshot = Some(current.clone());
                            if tx.send(current).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qemu_dummy_output_with_no_sources() {
        let sample = r#"PipeWire 'pipewire-0' [1.6.10, crowbar@crowbaros, cookie:3338809089]
 └─ Clients:
        31. pipewire                            [1.6.10, crowbar@crowbaros, pid:723]
        33. WirePlumber                         [1.6.10, crowbar@crowbaros, pid:720]

Audio
 ├─ Devices:
 │  
 ├─ Sinks:
 │  *   32. Dummy Output                        [vol: 1.00]
 │  
 ├─ Sources:
 │  
 ├─ Filters:
 │  
 └─ Streams:

Video
 ├─ Devices:
"#;
        let snap = parse_wpctl_status(sample).unwrap();
        assert_eq!(snap.sinks.len(), 1);
        assert!(snap.sources.is_empty());
        let sink = &snap.sinks[0];
        assert_eq!(sink.id, 32);
        assert_eq!(sink.name, "Dummy Output");
        assert!((sink.volume - 1.0).abs() < 0.001);
        assert!(!sink.muted);
        assert!(sink.is_default);
        assert_eq!(sink.volume_percent(), 100);
    }

    #[test]
    fn parses_multiple_sinks_and_sources_with_brackets_and_muted() {
        let sample = r#"PipeWire 'pipewire-0' [1.6.9, user@host, cookie:1]
Audio
 ├─ Devices:
 │      80. HD Audio [alsa]
 │
 ├─ Sinks:
 │      74. HD Audio HDMI / DisplayPort 1 Output [DELL U4025QW] (Stereo) [vol: 0.40]
 │  *   82. HD Audio Speaker [vol: 0.55 MUTED]
 │
 ├─ Sources:
 │      59. HD Audio Microphones [vol: 1.00]
 │  *  113. C922 Pro Stream Webcam Analog Stereo [vol: 0.42]
 │
 └─ Streams:
"#;
        let snap = parse_wpctl_status(sample).unwrap();
        assert_eq!(snap.sinks.len(), 2);
        assert_eq!(
            snap.sinks[0].name,
            "HD Audio HDMI / DisplayPort 1 Output [DELL U4025QW] (Stereo)"
        );
        assert_eq!(snap.sinks[0].volume_percent(), 40);
        assert!(!snap.sinks[0].muted);
        assert!(!snap.sinks[0].is_default);

        assert_eq!(snap.sinks[1].id, 82);
        assert_eq!(snap.sinks[1].name, "HD Audio Speaker");
        assert_eq!(snap.sinks[1].volume_percent(), 55);
        assert!(snap.sinks[1].muted);
        assert!(snap.sinks[1].is_default);

        assert_eq!(snap.sources.len(), 2);
        assert_eq!(snap.sources[1].id, 113);
        assert!(snap.sources[1].is_default);
        assert_eq!(snap.sources[1].volume_percent(), 42);
    }

    #[test]
    fn returns_unavailable_when_pipewire_output_is_missing() {
        let err = parse_wpctl_status("").unwrap_err();
        assert!(matches!(err, SoundError::Unavailable(_)));
    }

    #[test]
    fn clamps_and_formats_volumes_safely() {
        assert_eq!(format_wpctl_volume(-0.2), "0.00");
        assert_eq!(format_wpctl_volume(0.65), "0.65");
        assert_eq!(format_wpctl_volume(2.5), "1.50");
        assert_eq!(format_wpctl_volume(f32::NAN), "0.00");
    }
}
