//! Fulcrum desktop context client for Lever (`--fulcrum-socket`).
//!
//! Fetches a point-in-time desktop context snapshot from Fulcrum's agent socket
//! when the person sends a message from the drawer: workspace, active window
//! title and app ID, open windows by workspace, and displays — never window
//! contents or the clipboard. When Fulcrum is away, unreachable, or slow,
//! returns `None` so the message proceeds without desktop context.

use crate::frames;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

/// Protocol version spoken on Fulcrum's agent socket.
const FULCRUM_VERSION: u64 = 1;
/// Longest frame Lever accepts from Fulcrum (1 MiB).
const MAX_FULCRUM_FRAME: usize = 1024 * 1024;
/// Maximum time spent connecting to and reading one snapshot from Fulcrum.
pub const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(500);
/// Cap on app-supplied window title/app_id/display label length (code points).
const MAX_LABEL_CHARS: usize = 200;

/// One open window reported by Fulcrum (metadata only, never contents).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopWindow {
    pub id: String,
    pub app_id: String,
    pub title: String,
    pub workspace: i32,
}

/// One display reported by Fulcrum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopDisplay {
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
    pub scale: i32,
}

/// Desktop context snapshot attached to a message sent from the Agent's surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopContext {
    pub workspace: i32,
    pub active: Option<DesktopWindow>,
    pub locked: bool,
    pub windows: Vec<DesktopWindow>,
    pub displays: Vec<DesktopDisplay>,
}

/// Collapses ASCII/Unicode control and line-separator characters and caps code points.
fn sanitize_label(value: &str) -> String {
    let mut cleaned = String::with_capacity(value.len());
    let mut prev_space = false;
    for ch in value.chars() {
        let is_ctrl = ch.is_control() || ch == '\u{2028}' || ch == '\u{2029}';
        if is_ctrl {
            if !prev_space {
                cleaned.push(' ');
                prev_space = true;
            }
        } else {
            cleaned.push(ch);
            prev_space = false;
        }
    }
    let trimmed = cleaned.trim();
    let count = trimmed.chars().count();
    if count <= MAX_LABEL_CHARS {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(MAX_LABEL_CHARS - 1).collect();
    out.push('…');
    out
}

fn format_scale(scale_120: i32) -> String {
    let factor = f64::from(scale_120) / 120.0;
    let rounded = (factor * 100.0).round() / 100.0;
    if (rounded.fract()).abs() < f64::EPSILON {
        format!("{}x", rounded as i64)
    } else {
        let s = format!("{rounded:.2}");
        let trimmed = s.trim_end_matches('0').trim_end_matches('.');
        format!("{trimmed}x")
    }
}

fn format_window(window: &DesktopWindow) -> String {
    let clean_title = sanitize_label(&window.title);
    let title = if clean_title.is_empty() {
        "(untitled)".to_string()
    } else {
        clean_title
    };
    let app_id = sanitize_label(&window.app_id);
    format!("{title} (app_id: {app_id})")
}

/// Parses Fulcrum's `{"reply":"snapshot",...}` into a [`DesktopContext`].
/// Returns `None` if the frame is malformed or not a snapshot reply.
pub fn parse_snapshot(raw: &Value) -> Option<DesktopContext> {
    let obj = raw.as_object()?;
    if obj.get("reply").and_then(Value::as_str) != Some("snapshot") {
        return None;
    }
    let workspace = i32::try_from(obj.get("workspace")?.as_i64()?).ok()?;
    let locked = obj.get("locked")?.as_bool()?;
    let raw_windows = obj.get("windows")?.as_array()?;
    let raw_displays = obj.get("displays")?.as_array()?;

    let mut windows = Vec::with_capacity(raw_windows.len());
    for item in raw_windows {
        let w = item.as_object()?;
        windows.push(DesktopWindow {
            id: w.get("id")?.as_str()?.to_string(),
            app_id: sanitize_label(w.get("app_id")?.as_str()?),
            title: sanitize_label(w.get("title")?.as_str()?),
            workspace: i32::try_from(w.get("workspace")?.as_i64()?).ok()?,
        });
    }

    let mut displays = Vec::with_capacity(raw_displays.len());
    for item in raw_displays {
        let d = item.as_object()?;
        displays.push(DesktopDisplay {
            name: sanitize_label(d.get("name")?.as_str()?),
            width: i32::try_from(d.get("width")?.as_i64()?).ok()?,
            height: i32::try_from(d.get("height")?.as_i64()?).ok()?,
            x: i32::try_from(d.get("x")?.as_i64()?).ok()?,
            y: i32::try_from(d.get("y")?.as_i64()?).ok()?,
            scale: i32::try_from(d.get("scale")?.as_i64()?).ok()?,
        });
    }

    let active_id = obj.get("active").and_then(Value::as_str);
    let active = active_id.and_then(|id| windows.iter().find(|w| w.id == id).cloned());

    Some(DesktopContext {
        workspace,
        active,
        locked,
        windows,
        displays,
    })
}

/// Formats desktop context as a plain-text header sent to the harness with
/// the person's message: active window title and app_id, every window by
/// workspace, and displays — never window contents or clipboard.
pub fn format_desktop_context(ctx: &DesktopContext) -> String {
    let mut lines = Vec::new();
    lines.push("[Desktop context]".to_string());
    lines.push(format!("Workspace: {}", ctx.workspace));
    match &ctx.active {
        Some(win) => lines.push(format!("Active window: {}", format_window(win))),
        None => lines.push("Active window: none".to_string()),
    }
    if ctx.windows.is_empty() {
        lines.push("Windows: none".to_string());
    } else {
        lines.push("Windows by workspace:".to_string());
        let mut by_ws: BTreeMap<i32, Vec<&DesktopWindow>> = BTreeMap::new();
        for win in &ctx.windows {
            by_ws.entry(win.workspace).or_default().push(win);
        }
        let active_id = ctx.active.as_ref().map(|w| w.id.as_str());
        for (ws, group) in by_ws {
            let formatted: Vec<String> = group
                .into_iter()
                .map(|w| {
                    let marker = if active_id == Some(w.id.as_str()) {
                        " [active]"
                    } else {
                        ""
                    };
                    format!("{}{marker}", format_window(w))
                })
                .collect();
            lines.push(format!("- Workspace {ws}: {}", formatted.join("; ")));
        }
    }
    if ctx.displays.is_empty() {
        lines.push("Displays: none".to_string());
    } else {
        lines.push("Displays:".to_string());
        for d in &ctx.displays {
            lines.push(format!(
                "- {}: {}x{} at ({}, {}), scale {}",
                d.name,
                d.width,
                d.height,
                d.x,
                d.y,
                format_scale(d.scale)
            ));
        }
    }
    lines.join("\n")
}

/// Fetches a single desktop context snapshot from Fulcrum's agent socket within
/// `timeout`. Returns `None` if Fulcrum is not running, refuses, times out, or
/// returns an unexpected frame.
pub fn fetch_snapshot(path: &Path, timeout: Duration) -> Option<DesktopContext> {
    let deadline = Instant::now() + timeout;
    let mut stream = UnixStream::connect(path).ok()?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return None;
    }
    stream.set_write_timeout(Some(remaining)).ok()?;
    stream.set_read_timeout(Some(remaining)).ok()?;

    // Handshake: {"version": 1}
    stream
        .write_all(&frames::encode(&json!({ "version": FULCRUM_VERSION })))
        .ok()?;
    let handshake = read_frame(&mut stream, deadline)?;
    if handshake != json!({ "version": FULCRUM_VERSION }) {
        return None;
    }

    // Request snapshot: {"op": "snapshot"}
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return None;
    }
    stream.set_write_timeout(Some(remaining)).ok()?;
    stream.write_all(&frames::encode(&json!({ "op": "snapshot" }))).ok()?;
    let reply = read_frame(&mut stream, deadline)?;
    parse_snapshot(&reply)
}

fn read_frame(stream: &mut UnixStream, deadline: Instant) -> Option<Value> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return None;
    }
    stream.set_read_timeout(Some(remaining)).ok()?;
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).ok()?;
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FULCRUM_FRAME {
        return None;
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return None;
    }
    stream.set_read_timeout(Some(remaining)).ok()?;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn parses_snapshot_resolves_active_and_sanitizes_labels() {
        let raw = json!({
            "reply": "snapshot",
            "workspace": 2,
            "active": "0x2",
            "locked": false,
            "windows": [
                { "id": "0x1", "app_id": "com.crowbaros.StudioSettings", "title": "Settings", "workspace": 1 },
                { "id": "0x2", "app_id": "foot", "title": "shell\n\tprompt\u{2028}ok", "workspace": 2 }
            ],
            "displays": [
                { "name": "Virtual-1", "width": 1280, "height": 960, "x": 0, "y": 0, "scale": 120 },
                { "name": "Virtual-2", "width": 1920, "height": 1080, "x": 1280, "y": 0, "scale": 180 }
            ]
        });
        let ctx = parse_snapshot(&raw).expect("valid snapshot");
        assert_eq!(ctx.workspace, 2);
        assert!(!ctx.locked);
        assert_eq!(ctx.active.as_ref().map(|w| w.id.as_str()), Some("0x2"));
        assert_eq!(ctx.active.as_ref().map(|w| w.title.as_str()), Some("shell prompt ok"));

        let formatted = format_desktop_context(&ctx);
        assert!(formatted.starts_with("[Desktop context]\n"));
        assert!(formatted.contains("Workspace: 2"));
        assert!(formatted.contains("Active window: shell prompt ok (app_id: foot)"));
        assert!(formatted.contains("- Workspace 1: Settings (app_id: com.crowbaros.StudioSettings)"));
        assert!(formatted.contains("- Workspace 2: shell prompt ok (app_id: foot) [active]"));
        assert!(formatted.contains("- Virtual-1: 1280x960 at (0, 0), scale 1x"));
        assert!(formatted.contains("- Virtual-2: 1920x1080 at (1280, 0), scale 1.5x"));
    }

    #[test]
    fn rejects_malformed_or_non_snapshot_replies() {
        assert!(parse_snapshot(&json!({ "reply": "error", "message": "no" })).is_none());
        assert!(parse_snapshot(&json!({ "reply": "snapshot", "workspace": 1 })).is_none());
    }

    #[test]
    fn fetch_snapshot_returns_none_when_socket_is_missing_or_stalls() {
        let missing = Path::new("/tmp/crowbar-nonexistent-fulcrum-socket-check");
        assert!(fetch_snapshot(missing, Duration::from_millis(100)).is_none());

        let sock = Path::new("/tmp").join(format!("cl-fulcrum-stall-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let handle = std::thread::spawn(move || {
            let (_peer, _) = listener.accept().unwrap();
            std::thread::sleep(Duration::from_millis(300));
        });
        assert!(fetch_snapshot(&sock, Duration::from_millis(80)).is_none());
        let _ = handle.join();
        let _ = std::fs::remove_file(&sock);
    }
}
