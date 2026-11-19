// SPDX-License-Identifier: MIT
use serde::{Deserialize, Serialize};
use std::{
    env,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

pub const SOCKET_ENV: &str = "CROWBAR_DISPLAY_SOCKET";

/// Fractional scales Fulcrum accepts, in 120ths (100% through 300% in 25% steps).
pub const SCALES: [i32; 9] = [120, 150, 180, 210, 240, 270, 300, 330, 360];

const IO_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_OUTPUTS_REPLY: u64 = 1024 * 1024;
const MAX_ACK_REPLY: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct DisplayMode {
    pub width: i32,
    pub height: i32,
    pub refresh: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OutputInfo {
    pub name: String,
    #[serde(default)]
    pub identity: String,
    pub width: i32,
    pub height: i32,
    pub refresh: i32,
    pub x: i32,
    pub y: i32,
    pub scale: i32,
    #[serde(default)]
    pub modes: Vec<DisplayMode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDraft {
    pub width: i32,
    pub height: i32,
    pub refresh: i32,
    pub x: i32,
    pub y: i32,
    pub scale: i32,
}

impl OutputDraft {
    pub fn from_output(output: &OutputInfo) -> Self {
        Self {
            width: output.width,
            height: output.height,
            refresh: output.refresh,
            x: output.x,
            y: output.y,
            scale: output.scale,
        }
    }

    pub fn matches_output(&self, output: &OutputInfo) -> bool {
        self.width == output.width
            && self.height == output.height
            && self.refresh == output.refresh
            && self.x == output.x
            && self.y == output.y
            && self.scale == output.scale
    }

    pub fn mode(&self) -> DisplayMode {
        DisplayMode {
            width: self.width,
            height: self.height,
            refresh: self.refresh,
        }
    }

    pub fn to_configure_request(&self, live: &OutputInfo) -> Option<serde_json::Value> {
        if self.matches_output(live) {
            return None;
        }
        let mut obj = serde_json::Map::new();
        obj.insert(
            "op".into(),
            serde_json::Value::String("configure_output".into()),
        );
        obj.insert("name".into(), serde_json::Value::String(live.name.clone()));
        if self.width != live.width || self.height != live.height || self.refresh != live.refresh {
            obj.insert("width".into(), serde_json::json!(self.width));
            obj.insert("height".into(), serde_json::json!(self.height));
            obj.insert("refresh".into(), serde_json::json!(self.refresh));
        }
        if self.x != live.x || self.y != live.y {
            obj.insert("x".into(), serde_json::json!(self.x));
            obj.insert("y".into(), serde_json::json!(self.y));
        }
        if self.scale != live.scale {
            obj.insert("scale".into(), serde_json::json!(self.scale));
        }
        Some(serde_json::Value::Object(obj))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelativeSide {
    Left,
    Right,
    Above,
    Below,
}

impl RelativeSide {
    pub const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Above, Self::Below];

    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
            Self::Above => "Above",
            Self::Below => "Below",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiagramRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisplayError {
    Unavailable(String),
    Failed(String),
}

impl DisplayError {
    pub fn message(&self) -> &str {
        match self {
            Self::Unavailable(msg) | Self::Failed(msg) => msg,
        }
    }
}

pub fn format_scale(scale_120: i32) -> String {
    let pct = ((i64::from(scale_120) * 100) + 60) / 120;
    format!("{pct}%")
}

pub fn format_refresh(mhz: i32) -> String {
    if mhz <= 0 {
        return "Default".into();
    }
    if mhz % 1000 == 0 {
        return format!("{} Hz", mhz / 1000);
    }
    let formatted = format!("{:.2}", f64::from(mhz) / 1000.0);
    let trimmed = formatted.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed} Hz")
}

pub fn logical_size(width: i32, height: i32, scale_120: i32) -> (i32, i32) {
    let scale = if scale_120 > 0 { scale_120 } else { 120 };
    (
        ((i64::from(width) * 120) / i64::from(scale)).max(1) as i32,
        ((i64::from(height) * 120) / i64::from(scale)).max(1) as i32,
    )
}

pub fn dedup_sorted_modes(modes: &[DisplayMode], current: DisplayMode) -> Vec<DisplayMode> {
    let mut list: Vec<DisplayMode> = Vec::with_capacity(modes.len() + 1);
    for &mode in modes {
        if mode.width > 0 && mode.height > 0 && !list.contains(&mode) {
            list.push(mode);
        }
    }
    if current.width > 0 && current.height > 0 && !list.contains(&current) {
        list.push(current);
    }
    list.sort_by(|a, b| {
        let area_a = i64::from(a.width) * i64::from(a.height);
        let area_b = i64::from(b.width) * i64::from(b.height);
        area_b
            .cmp(&area_a)
            .then_with(|| b.width.cmp(&a.width))
            .then_with(|| b.refresh.cmp(&a.refresh))
    });
    list
}

/// Repositions `selected` relative to the anchor output (index 0, or index 1
/// when index 0 itself is selected) and normalizes the layout origin to (0, 0).
pub fn arrange_relative(drafts: &mut [OutputDraft], selected: usize, side: RelativeSide) {
    if drafts.len() < 2 || selected >= drafts.len() {
        return;
    }
    let anchor = if selected == 0 { 1 } else { 0 };
    let (anchor_w, anchor_h) = logical_size(
        drafts[anchor].width,
        drafts[anchor].height,
        drafts[anchor].scale,
    );
    let (sel_w, sel_h) = logical_size(
        drafts[selected].width,
        drafts[selected].height,
        drafts[selected].scale,
    );
    let (ax, ay) = (drafts[anchor].x, drafts[anchor].y);
    let (nx, ny) = match side {
        RelativeSide::Left => (ax - sel_w, ay),
        RelativeSide::Right => (ax + anchor_w, ay),
        RelativeSide::Above => (ax, ay - sel_h),
        RelativeSide::Below => (ax, ay + anchor_h),
    };
    drafts[selected].x = nx;
    drafts[selected].y = ny;

    let min_x = drafts.iter().map(|d| d.x).min().unwrap_or(0);
    let min_y = drafts.iter().map(|d| d.y).min().unwrap_or(0);
    if min_x != 0 || min_y != 0 {
        for d in drafts.iter_mut() {
            d.x -= min_x;
            d.y -= min_y;
        }
    }
}

/// Projects logical display rectangles into a centered preview canvas.
pub fn project_diagram(
    drafts: &[OutputDraft],
    canvas_w: f32,
    canvas_h: f32,
    pad: f32,
) -> Vec<DiagramRect> {
    if drafts.is_empty() {
        return Vec::new();
    }
    let boxes: Vec<(f32, f32, f32, f32)> = drafts
        .iter()
        .map(|d| {
            let (lw, lh) = logical_size(d.width, d.height, d.scale);
            (d.x as f32, d.y as f32, lw as f32, lh as f32)
        })
        .collect();
    let min_x = boxes.iter().map(|b| b.0).fold(f32::INFINITY, f32::min);
    let min_y = boxes.iter().map(|b| b.1).fold(f32::INFINITY, f32::min);
    let max_x = boxes
        .iter()
        .map(|b| b.0 + b.2)
        .fold(f32::NEG_INFINITY, f32::max);
    let max_y = boxes
        .iter()
        .map(|b| b.1 + b.3)
        .fold(f32::NEG_INFINITY, f32::max);
    let span_w = (max_x - min_x).max(1.0);
    let span_h = (max_y - min_y).max(1.0);
    let avail_w = (canvas_w - 2.0 * pad).max(1.0);
    let avail_h = (canvas_h - 2.0 * pad).max(1.0);
    let factor = (avail_w / span_w).min(avail_h / span_h);
    let scaled_w = span_w * factor;
    let scaled_h = span_h * factor;
    let offset_x = (canvas_w - scaled_w) * 0.5;
    let offset_y = (canvas_h - scaled_h) * 0.5;

    boxes
        .into_iter()
        .map(|(x, y, w, h)| DiagramRect {
            x: offset_x + (x - min_x) * factor,
            y: offset_y + (y - min_y) * factor,
            width: (w * factor).max(48.0),
            height: (h * factor).max(36.0),
        })
        .collect()
}

fn socket_path() -> Result<PathBuf, DisplayError> {
    env::var_os(SOCKET_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            DisplayError::Unavailable(
                "Display controls are available on the CrowbarOS Fulcrum desktop.".into(),
            )
        })
}

fn exchange(
    path: &Path,
    request: &serde_json::Value,
    max_reply: u64,
) -> Result<serde_json::Value, DisplayError> {
    let mut stream = UnixStream::connect(path).map_err(|e| match e.kind() {
        ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::PermissionDenied => {
            DisplayError::Unavailable(
                "Display controls are currently unavailable on this session.".into(),
            )
        }
        _ => DisplayError::Failed(format!("Could not reach the display service: {e}")),
    })?;
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

    let mut payload =
        serde_json::to_vec(request).map_err(|e| DisplayError::Failed(e.to_string()))?;
    payload.push(b'\n');
    stream
        .write_all(&payload)
        .map_err(|e| DisplayError::Failed(format!("Could not send display request: {e}")))?;

    let mut line = String::new();
    let read = BufReader::new(&stream)
        .take(max_reply + 1)
        .read_line(&mut line)
        .map_err(|e| DisplayError::Failed(format!("Could not read display reply: {e}")))?;
    if read == 0 || !line.ends_with('\n') || (line.len() as u64) > max_reply {
        return Err(DisplayError::Failed(
            "Display service returned an incomplete reply.".into(),
        ));
    }
    let reply: serde_json::Value = serde_json::from_str(line.trim_end())
        .map_err(|e| DisplayError::Failed(format!("Invalid display reply: {e}")))?;
    if reply["reply"].as_str() == Some("error") {
        let message = reply["message"]
            .as_str()
            .unwrap_or("Display request was refused.");
        return Err(DisplayError::Failed(message.to_string()));
    }
    Ok(reply)
}

pub fn fetch_outputs() -> Result<Vec<OutputInfo>, DisplayError> {
    let path = socket_path()?;
    fetch_outputs_at(&path)
}

pub fn fetch_outputs_at(path: &Path) -> Result<Vec<OutputInfo>, DisplayError> {
    let reply = exchange(path, &serde_json::json!({ "op": "outputs" }), MAX_OUTPUTS_REPLY)?;
    if reply["reply"].as_str() != Some("outputs") {
        return Err(DisplayError::Failed(
            "Unexpected reply when reading displays.".into(),
        ));
    }
    #[derive(Deserialize)]
    struct OutputsReply {
        outputs: Vec<OutputInfo>,
    }
    let parsed: OutputsReply = serde_json::from_value(reply)
        .map_err(|e| DisplayError::Failed(format!("Could not parse display list: {e}")))?;
    Ok(parsed.outputs)
}

pub fn apply_changes(
    live: &[OutputInfo],
    drafts: &[OutputDraft],
) -> Result<Vec<String>, DisplayError> {
    let path = socket_path()?;
    apply_changes_at(&path, live, drafts)
}

pub fn apply_changes_at(
    path: &Path,
    live: &[OutputInfo],
    drafts: &[OutputDraft],
) -> Result<Vec<String>, DisplayError> {
    let mut applied = Vec::new();
    for (output, draft) in live.iter().zip(drafts.iter()) {
        if let Some(request) = draft.to_configure_request(output) {
            let reply = exchange(path, &request, MAX_ACK_REPLY)?;
            if reply["reply"].as_str() != Some("ok") {
                return Err(DisplayError::Failed(format!(
                    "Unexpected reply while configuring {}.",
                    output.name
                )));
            }
            applied.push(output.name.clone());
        }
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_socket_path(label: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        env::temp_dir().join(format!(
            "crowbar-studio-displays-test-{}-{}-{id}.sock",
            std::process::id(),
            label
        ))
    }

    fn sample_output(name: &str, w: i32, h: i32, x: i32, y: i32, scale: i32) -> OutputInfo {
        OutputInfo {
            name: name.into(),
            identity: format!("EDID-{name}"),
            width: w,
            height: h,
            refresh: 60000,
            x,
            y,
            scale,
            modes: vec![
                DisplayMode {
                    width: w,
                    height: h,
                    refresh: 60000,
                },
                DisplayMode {
                    width: 1920,
                    height: 1080,
                    refresh: 59951,
                },
            ],
        }
    }

    #[test]
    fn scale_and_refresh_formatting_is_human_readable() {
        assert_eq!(format_scale(120), "100%");
        assert_eq!(format_scale(150), "125%");
        assert_eq!(format_scale(180), "150%");
        assert_eq!(format_scale(240), "200%");
        assert_eq!(format_scale(360), "300%");

        assert_eq!(format_refresh(60000), "60 Hz");
        assert_eq!(format_refresh(120000), "120 Hz");
        assert_eq!(format_refresh(59951), "59.95 Hz");
        assert_eq!(format_refresh(59900), "59.9 Hz");
        assert_eq!(format_refresh(0), "Default");
    }

    #[test]
    fn logical_size_uses_120ths_scale() {
        assert_eq!(logical_size(3840, 2160, 120), (3840, 2160));
        assert_eq!(logical_size(3840, 2160, 150), (3072, 1728));
        assert_eq!(logical_size(3840, 2160, 240), (1920, 1080));
        assert_eq!(logical_size(1280, 720, 0), (1280, 720));
    }

    #[test]
    fn dedup_sorted_modes_orders_by_area_then_refresh_and_keeps_current() {
        let modes = vec![
            DisplayMode {
                width: 1280,
                height: 720,
                refresh: 60000,
            },
            DisplayMode {
                width: 1920,
                height: 1080,
                refresh: 60000,
            },
            DisplayMode {
                width: 1920,
                height: 1080,
                refresh: 120000,
            },
            DisplayMode {
                width: 1920,
                height: 1080,
                refresh: 60000,
            },
        ];
        let current = DisplayMode {
            width: 1600,
            height: 900,
            refresh: 60000,
        };
        let sorted = dedup_sorted_modes(&modes, current);
        assert_eq!(
            sorted,
            vec![
                DisplayMode {
                    width: 1920,
                    height: 1080,
                    refresh: 120000,
                },
                DisplayMode {
                    width: 1920,
                    height: 1080,
                    refresh: 60000,
                },
                DisplayMode {
                    width: 1600,
                    height: 900,
                    refresh: 60000,
                },
                DisplayMode {
                    width: 1280,
                    height: 720,
                    refresh: 60000,
                },
            ]
        );
    }

    #[test]
    fn arrange_relative_normalizes_negative_origins() {
        let out0 = sample_output("DP-1", 1920, 1080, 0, 0, 120);
        let out1 = sample_output("HDMI-A-1", 2560, 1440, 1920, 0, 240); // logical 1280x720
        let mut drafts = vec![
            OutputDraft::from_output(&out0),
            OutputDraft::from_output(&out1),
        ];

        arrange_relative(&mut drafts, 1, RelativeSide::Left);
        assert_eq!((drafts[1].x, drafts[1].y), (0, 0));
        assert_eq!((drafts[0].x, drafts[0].y), (1280, 0));

        arrange_relative(&mut drafts, 1, RelativeSide::Above);
        assert_eq!((drafts[1].x, drafts[1].y), (0, 0));
        assert_eq!((drafts[0].x, drafts[0].y), (0, 720));

        arrange_relative(&mut drafts, 1, RelativeSide::Right);
        assert_eq!((drafts[0].x, drafts[0].y), (0, 0));
        assert_eq!((drafts[1].x, drafts[1].y), (1920, 0));
    }

    #[test]
    fn project_diagram_fits_and_centers_displays() {
        let out0 = sample_output("DP-1", 1920, 1080, 0, 0, 120);
        let out1 = sample_output("HDMI-A-1", 1920, 1080, 1920, 0, 120);
        let drafts = vec![
            OutputDraft::from_output(&out0),
            OutputDraft::from_output(&out1),
        ];
        let rects = project_diagram(&drafts, 560.0, 148.0, 16.0);
        assert_eq!(rects.len(), 2);
        assert!(rects[0].x >= 15.9 && rects[0].y >= 15.9);
        assert!(rects[1].x + rects[1].width <= 560.0 - 15.9);
        assert!(rects[1].y + rects[1].height <= 148.0 - 15.9);
        assert!(rects[1].x > rects[0].x);
    }

    #[test]
    fn configure_request_only_includes_changed_fields() {
        let live = sample_output("Virtual-1", 1280, 720, 0, 0, 120);
        let mut draft = OutputDraft::from_output(&live);
        assert!(draft.to_configure_request(&live).is_none());

        draft.scale = 150;
        assert_eq!(
            draft.to_configure_request(&live),
            Some(serde_json::json!({
                "op": "configure_output",
                "name": "Virtual-1",
                "scale": 150
            }))
        );

        draft.width = 1920;
        draft.height = 1080;
        draft.refresh = 59951;
        assert_eq!(
            draft.to_configure_request(&live),
            Some(serde_json::json!({
                "op": "configure_output",
                "name": "Virtual-1",
                "width": 1920,
                "height": 1080,
                "refresh": 59951,
                "scale": 150
            }))
        );
    }

    #[test]
    fn socket_roundtrip_fetches_and_applies_outputs() {
        let path = temp_socket_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();

        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for reply in [
                r#"{"reply":"outputs","outputs":[{"name":"Virtual-1","identity":"QEMU Monitor","width":1280,"height":720,"refresh":60000,"x":0,"y":0,"scale":120,"modes":[{"width":1280,"height":720,"refresh":60000},{"width":1920,"height":1080,"refresh":60000}]}]}"#,
                r#"{"reply":"ok"}"#,
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream.try_clone().unwrap())
                    .read_line(&mut line)
                    .unwrap();
                seen.push(serde_json::from_str::<serde_json::Value>(line.trim_end()).unwrap());
                stream.write_all(format!("{reply}\n").as_bytes()).unwrap();
            }
            seen
        });

        let outputs = fetch_outputs_at(&path).unwrap();
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].name, "Virtual-1");
        assert_eq!(outputs[0].scale, 120);
        assert_eq!(outputs[0].modes.len(), 2);

        let mut drafts = vec![OutputDraft::from_output(&outputs[0])];
        drafts[0].scale = 150;
        let applied = apply_changes_at(&path, &outputs, &drafts).unwrap();
        assert_eq!(applied, vec!["Virtual-1".to_string()]);

        let seen = server.join().unwrap();
        assert_eq!(seen[0], serde_json::json!({ "op": "outputs" }));
        assert_eq!(
            seen[1],
            serde_json::json!({
                "op": "configure_output",
                "name": "Virtual-1",
                "scale": 150
            })
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_socket_reports_unavailable_not_failure() {
        let missing = temp_socket_path("missing");
        let _ = std::fs::remove_file(&missing);
        let err = fetch_outputs_at(&missing).unwrap_err();
        assert!(matches!(err, DisplayError::Unavailable(_)));
    }
}
