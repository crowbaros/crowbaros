//! The layout of displays (ADR 0020): each monitor's mode, place, and
//! scale, kept by the monitor's identity (see `edid.rs`), so that a monitor
//! keeps them on any port and after a restart.
//!
//! The layout is machine state. The compositor holds it, in a file of its
//! own that the binary names with `--layout`, and changes it only when the
//! shell asks over its control channel (see `control.rs`). The file holds
//! one flat object per monitor, as the control channel speaks, with the
//! scale in 120ths as fractional scale counts:
//!
//! ```text
//! {"identity":"DEL DELL U4025QW ABC1234","width":5120,"height":2160,"refresh":59990,"x":0,"y":0,"scale":150}
//! ```
//!
//! A monitor it does not name takes its preferred mode, the place to the
//! right of the others, and a scale picked from its density (see
//! `default_scale.rs`), which is not kept here. A line that cannot be read
//! is left out, and the rest still apply.

use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

use crate::{
    json::{Value, parse_object, quote},
    log,
};

/// The most monitors the file keeps.
const MAX_MONITORS: usize = 64;
/// The largest file read.
const MAX_FILE: u64 = 1024 * 1024;

/// What the person chose for one monitor. What is not chosen is the
/// compositor's to decide.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Placement {
    /// Width, height, and refresh in millihertz.
    pub mode: Option<(i32, i32, i32)>,
    pub position: Option<(i32, i32)>,
    /// In 120ths.
    pub scale_120: Option<u32>,
}

#[derive(Debug, Default)]
pub(crate) struct Layout {
    path: Option<PathBuf>,
    monitors: BTreeMap<String, Placement>,
}

impl Layout {
    /// The layout a file holds, kept there from now on. A file that does not
    /// exist yet is an empty layout.
    pub(crate) fn load(path: &Path) -> Layout {
        let mut layout = Layout {
            path: Some(path.to_owned()),
            monitors: BTreeMap::new(),
        };
        let text = match std::fs::metadata(path) {
            Ok(metadata) if metadata.len() > MAX_FILE => {
                log::warning!("the layout file is too large to read path={}", path.display());
                return layout;
            }
            Ok(_) => std::fs::read_to_string(path).unwrap_or_default(),
            Err(_) => return layout,
        };
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            match parse_line(line) {
                Ok((identity, placement)) if layout.monitors.len() < MAX_MONITORS => {
                    layout.monitors.insert(identity, placement);
                }
                Ok(_) => break,
                Err(error) => log::warning!("a line of the layout file was left out: {error}"),
            }
        }
        layout
    }

    pub(crate) fn get(&self, identity: &str) -> Option<&Placement> {
        self.monitors.get(identity)
    }

    /// Keeps what was chosen for a monitor, and writes the file.
    pub(crate) fn set(&mut self, identity: &str, placement: Placement) {
        if !self.monitors.contains_key(identity) && self.monitors.len() >= MAX_MONITORS {
            // The oldest by name goes: a machine sees few monitors.
            let first = self.monitors.keys().next().cloned();
            if let Some(first) = first {
                self.monitors.remove(&first);
            }
        }
        self.monitors.insert(identity.to_owned(), placement);
        if let Err(error) = self.save() {
            log::warning!("could not keep the layout: {error}");
        }
    }

    /// Writes the file whole, through a file beside it that takes its place,
    /// so a crash leaves the old layout or the new one.
    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut text = String::new();
        for (identity, placement) in &self.monitors {
            text.push_str(&line_of(identity, placement));
            text.push('\n');
        }
        let fresh = path.with_extension("new");
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&fresh)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&fresh, path)
    }
}

fn parse_line(line: &str) -> Result<(String, Placement), String> {
    let object = parse_object(line)?;
    let identity = match object.get("identity") {
        Some(Value::String(identity)) if !identity.is_empty() => identity.clone(),
        _ => return Err("a monitor names its identity".into()),
    };
    Ok((identity, placement_of(&object)?))
}

/// The fields of a placement in an object: each optional, none other.
pub(crate) fn placement_of(object: &BTreeMap<String, Value>) -> Result<Placement, String> {
    let number = |key: &str| -> Result<Option<i64>, String> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Number(value)) => Ok(Some(*value)),
            Some(_) => Err(format!("{key} is a whole number")),
        }
    };
    let fits = |value: i64| i32::try_from(value).map_err(|_| format!("{value} is out of range"));
    let mode = match (number("width")?, number("height")?, number("refresh")?) {
        (Some(width), Some(height), refresh) => Some((fits(width)?, fits(height)?, fits(refresh.unwrap_or(0))?)),
        (None, None, None) => None,
        _ => return Err("a mode names its width and height".into()),
    };
    let position = match (number("x")?, number("y")?) {
        (Some(x), Some(y)) => Some((fits(x)?, fits(y)?)),
        (None, None) => None,
        _ => return Err("a place names its x and y".into()),
    };
    let scale_120 = number("scale")?
        .map(|scale| u32::try_from(scale).map_err(|_| format!("{scale} is not a scale")))
        .transpose()?;
    Ok(Placement {
        mode,
        position,
        scale_120,
    })
}

fn line_of(identity: &str, placement: &Placement) -> String {
    let mut fields = vec![format!("\"identity\":{}", quote(identity))];
    if let Some((width, height, refresh)) = placement.mode {
        fields.push(format!("\"width\":{width},\"height\":{height},\"refresh\":{refresh}"));
    }
    if let Some((x, y)) = placement.position {
        fields.push(format!("\"x\":{x},\"y\":{y}"));
    }
    if let Some(scale) = placement.scale_120 {
        fields.push(format!("\"scale\":{scale}"));
    }
    format!("{{{}}}", fields.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fulcrum-layout-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("layout")
    }

    #[test]
    fn a_layout_kept_is_the_layout_read_back() {
        let path = file("kept");
        let mut layout = Layout::load(&path);
        let placement = Placement {
            mode: Some((2560, 1440, 59951)),
            position: Some((-2560, 0)),
            scale_120: Some(150),
        };
        layout.set("CRB Studio 27 SN0042", placement.clone());
        layout.set("CRB Pocket 1", Placement::default());

        let again = Layout::load(&path);
        assert_eq!(again.get("CRB Studio 27 SN0042"), Some(&placement));
        assert_eq!(again.get("CRB Pocket 1"), Some(&Placement::default()));
        assert_eq!(again.get("anything else"), None);
    }

    #[test]
    fn a_line_that_cannot_be_read_is_left_out_and_the_rest_apply() {
        let path = file("broken");
        std::fs::write(
            &path,
            "not json\n{\"identity\":\"A\",\"width\":800}\n{\"identity\":\"B\",\"scale\":180}\n{\"x\":1,\"y\":2}\n",
        )
        .unwrap();
        let layout = Layout::load(&path);
        assert_eq!(layout.get("A"), None, "a mode with no height");
        assert_eq!(layout.get("B").and_then(|placement| placement.scale_120), Some(180));
        assert_eq!(layout.monitors.len(), 1);
    }
}
