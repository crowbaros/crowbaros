// SPDX-License-Identifier: MIT
//! Quick Settings data model, volume/brightness clamping, OSD timeout state,
//! and co-located popover/OSD views.

pub mod view;

pub use view::{OsdOverlay, QuickSettingsPopover, close_osd, handle_media_key, show_osd, toggle};

use serde_json::Value;
use std::time::{Duration, Instant};

pub const OSD_DURATION: Duration = Duration::from_millis(1600);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioQuickState {
    pub available: bool,
    pub volume: u32,
    pub muted: bool,
    pub sink_name: String,
    pub mic_available: bool,
    pub mic_muted: bool,
}

impl Default for AudioQuickState {
    fn default() -> Self {
        Self {
            available: false,
            volume: 0,
            muted: false,
            sink_name: "Audio Output".into(),
            mic_available: false,
            mic_muted: false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WifiQuickState {
    pub available: bool,
    pub powered: bool,
    pub ssid: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BluetoothQuickState {
    pub available: bool,
    pub powered: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BrightnessState {
    pub available: bool,
    pub percent: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct QuickStatus {
    pub audio: AudioQuickState,
    pub wifi: WifiQuickState,
    pub bluetooth: BluetoothQuickState,
    pub brightness: BrightnessState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerConfirm {
    Reboot,
    PowerOff,
}

impl PowerConfirm {
    pub fn action_name(self) -> &'static str {
        match self {
            Self::Reboot => "reboot",
            Self::PowerOff => "poweroff",
        }
    }

    pub fn prompt(self) -> &'static str {
        match self {
            Self::Reboot => "Reboot this machine now?",
            Self::PowerOff => "Power off this machine now?",
        }
    }

    pub fn confirm_label(self) -> &'static str {
        match self {
            Self::Reboot => "Reboot",
            Self::PowerOff => "Power Off",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsdKind {
    Volume,
    Microphone,
    Brightness,
}

impl OsdKind {
    pub fn label(self, muted: bool) -> &'static str {
        match (self, muted) {
            (Self::Volume, true) => "Muted",
            (Self::Volume, false) => "Volume",
            (Self::Microphone, true) => "Mic muted",
            (Self::Microphone, false) => "Microphone",
            (Self::Brightness, _) => "Brightness",
        }
    }

    pub fn log_name(self) -> &'static str {
        match self {
            Self::Volume => "volume",
            Self::Microphone => "microphone",
            Self::Brightness => "brightness",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OsdState {
    pub kind: OsdKind,
    pub percent: u32,
    pub muted: bool,
    pub expires_at: Instant,
    pub sequence: u64,
}

impl OsdState {
    pub fn new(kind: OsdKind, percent: u32, muted: bool, now: Instant, sequence: u64) -> Self {
        let clamped = match kind {
            OsdKind::Brightness => clamp_brightness_percent(percent),
            OsdKind::Volume | OsdKind::Microphone => clamp_volume_percent(percent),
        };
        Self {
            kind,
            percent: clamped,
            muted,
            expires_at: now + OSD_DURATION,
            sequence,
        }
    }

    pub fn is_expired(&self, now: Instant) -> bool {
        now >= self.expires_at
    }

    pub fn value_text(&self) -> String {
        match self.kind {
            OsdKind::Microphone => {
                if self.muted {
                    "Off".into()
                } else {
                    "On".into()
                }
            }
            OsdKind::Volume if self.muted => "Muted".into(),
            _ => format!("{}%", self.percent),
        }
    }
}

pub fn clamp_volume_percent(percent: u32) -> u32 {
    percent.min(100)
}

#[allow(dead_code)]
pub fn step_volume_percent(current: u32, delta: i32) -> u32 {
    (current as i32 + delta).clamp(0, 100) as u32
}

pub fn clamp_brightness_percent(percent: u32) -> u32 {
    percent.clamp(5, 100)
}

#[allow(dead_code)]
pub fn step_brightness_percent(current: u32, delta: i32) -> u32 {
    (current as i32 + delta).clamp(5, 100) as u32
}

pub fn parse_audio(v: &Value) -> AudioQuickState {
    let available = v["available"].as_bool().unwrap_or(false);
    if !available {
        return AudioQuickState::default();
    }
    AudioQuickState {
        available: true,
        volume: clamp_volume_percent(v["volume"].as_u64().unwrap_or(0) as u32),
        muted: v["muted"].as_bool().unwrap_or(false),
        sink_name: v["sink_name"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("Audio Output")
            .to_string(),
        mic_available: v["mic_available"].as_bool().unwrap_or(false),
        mic_muted: v["mic_muted"].as_bool().unwrap_or(false),
    }
}

pub fn parse_wifi(v: &Value) -> WifiQuickState {
    if !v["available"].as_bool().unwrap_or(false) {
        return WifiQuickState::default();
    }
    WifiQuickState {
        available: true,
        powered: v["powered"].as_bool().unwrap_or(false),
        ssid: v["ssid"].as_str().map(str::to_string),
    }
}

pub fn parse_bluetooth(v: &Value) -> BluetoothQuickState {
    if !v["available"].as_bool().unwrap_or(false) {
        return BluetoothQuickState::default();
    }
    BluetoothQuickState {
        available: true,
        powered: v["powered"].as_bool().unwrap_or(false),
    }
}

pub fn parse_brightness(v: &Value) -> BrightnessState {
    if !v["available"].as_bool().unwrap_or(false) {
        return BrightnessState::default();
    }
    BrightnessState {
        available: true,
        percent: clamp_brightness_percent(v["percent"].as_u64().unwrap_or(50) as u32),
    }
}

pub fn parse_quick_status(reply: &Value) -> QuickStatus {
    QuickStatus {
        audio: parse_audio(&reply["audio"]),
        wifi: parse_wifi(&reply["wifi"]),
        bluetooth: parse_bluetooth(&reply["bluetooth"]),
        brightness: parse_brightness(&reply["brightness"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn volume_and_brightness_clamping_and_stepping() {
        assert_eq!(clamp_volume_percent(0), 0);
        assert_eq!(clamp_volume_percent(150), 100);
        assert_eq!(step_volume_percent(50, 5), 55);
        assert_eq!(step_volume_percent(2, -5), 0);
        assert_eq!(step_volume_percent(98, 5), 100);

        assert_eq!(clamp_brightness_percent(0), 5);
        assert_eq!(clamp_brightness_percent(120), 100);
        assert_eq!(step_brightness_percent(10, -10), 5);
        assert_eq!(step_brightness_percent(95, 10), 100);
    }

    #[test]
    fn osd_timeout_state_tracks_expiry_and_clamps_values() {
        let start = Instant::now();
        let osd = OsdState::new(OsdKind::Volume, 140, false, start, 1);
        assert_eq!(osd.percent, 100);
        assert_eq!(osd.value_text(), "100%");
        assert!(!osd.is_expired(start + Duration::from_millis(800)));
        assert!(osd.is_expired(start + OSD_DURATION));

        let muted = OsdState::new(OsdKind::Volume, 60, true, start, 2);
        assert_eq!(muted.kind.label(muted.muted), "Muted");
        assert_eq!(muted.value_text(), "Muted");

        let bright = OsdState::new(OsdKind::Brightness, 0, false, start, 3);
        assert_eq!(bright.percent, 5);
        assert_eq!(bright.value_text(), "5%");
    }

    #[test]
    fn parses_quick_status_with_absent_optional_hardware() {
        let reply = json!({
            "ok": true,
            "audio": {
                "available": true,
                "volume": 65,
                "muted": false,
                "sink_name": "Dummy Output",
                "mic_available": false,
                "mic_muted": false
            },
            "wifi": {"available": false},
            "bluetooth": {"available": false},
            "brightness": {"available": false}
        });
        let status = parse_quick_status(&reply);
        assert!(status.audio.available);
        assert_eq!(status.audio.volume, 65);
        assert_eq!(status.audio.sink_name, "Dummy Output");
        assert!(!status.audio.mic_available);
        assert!(!status.wifi.available);
        assert!(!status.bluetooth.available);
        assert!(!status.brightness.available);
    }
}
