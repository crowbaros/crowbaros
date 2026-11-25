//! Lever's private log in the person's home (`lever.log`).
//!
//! Records starts, restarts, listening sockets, connections, stops, and errors
//! — never anything a peer sent. Always mode 0600, rotated at 4 MiB to
//! `lever.log.1`.

use crate::sys;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_LOG: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub enum Event {
    Start,
    Restart,
    Listening,
    Connection,
    Stop,
    Error,
}

impl Event {
    fn as_str(self) -> &'static str {
        match self {
            Event::Start => "start",
            Event::Restart => "restart",
            Event::Listening => "listening",
            Event::Connection => "connection",
            Event::Stop => "stop",
            Event::Error => "error",
        }
    }
}

pub struct Log {
    path: PathBuf,
    file: File,
    size: u64,
}

impl Log {
    /// Opens or creates `path` for appending, readable by the person alone.
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let file = open_private(&path)?;
        let size = file.metadata()?.len();
        Ok(Self { path, file, size })
    }

    /// Writes one UTC-timestamped line, rotating to `<path>.1` before crossing
    /// `MAX_LOG`.
    pub fn write(&mut self, event: Event, details: &str) -> io::Result<()> {
        let escaped = details.replace('\n', "\n    ");
        let line = format!("{} {}: {escaped}\n", utc_timestamp(), event.as_str());
        let bytes = line.as_bytes();
        if self.size > 0 && self.size + (bytes.len() as u64) > MAX_LOG {
            let mut backup = self.path.as_os_str().to_os_string();
            backup.push(".1");
            std::fs::rename(&self.path, &backup)?;
            self.file = open_private(&self.path)?;
            self.size = 0;
        }
        if let Err(err) = self.file.write_all(bytes) {
            let _ = self.file.set_len(self.size);
            return Err(err);
        }
        self.size += bytes.len() as u64;
        Ok(())
    }
}

fn open_private(path: &PathBuf) -> io::Result<File> {
    let file = OpenOptions::new().create(true).append(true).mode(0o600).open(path)?;
    sys::fchmod(&file, 0o600)?;
    Ok(file)
}

fn utc_timestamp() -> String {
    let dur = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let total_secs = dur.as_secs();
    let millis = dur.subsec_millis();
    let days = (total_secs / 86_400) as i64;
    let rem = total_secs % 86_400;
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Howard Hinnant's algorithm converting days since 1970-01-01 to (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}
