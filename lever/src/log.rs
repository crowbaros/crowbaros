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
    limit: u64,
}

impl Log {
    /// Opens or creates `path` for appending, readable by the person alone.
    pub fn open(path: PathBuf) -> io::Result<Self> {
        Self::open_with_limit(path, MAX_LOG)
    }

    fn open_with_limit(path: PathBuf, limit: u64) -> io::Result<Self> {
        let file = open_private(&path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            path,
            file,
            size,
            limit,
        })
    }

    /// Writes one UTC-timestamped line, rotating to `<path>.1` before crossing
    /// the limit.
    pub fn write(&mut self, event: Event, details: &str) -> io::Result<()> {
        let escaped = details.replace('\n', "\n    ");
        let line = format!("{} {}: {escaped}\n", utc_timestamp(), event.as_str());
        let bytes = line.as_bytes();
        if self.size > 0 && self.size + (bytes.len() as u64) > self.limit {
            self.rotate()?;
        }
        if let Err(err) = self.file.write_all(bytes) {
            let _ = self.file.set_len(self.size);
            return Err(err);
        }
        self.size += bytes.len() as u64;
        Ok(())
    }
}

impl Log {
    /// Moves the full log to `<path>.1` and starts a new one.
    fn rotate(&mut self) -> io::Result<()> {
        let mut backup = self.path.as_os_str().to_os_string();
        backup.push(".1");
        match std::fs::rename(&self.path, &backup) {
            Ok(()) => {}
            // An earlier rotation moved the log and then could not open the
            // new one. The move is done, so only the open is tried again.
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        self.file = open_private(&self.path)?;
        self.size = 0;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_log(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cl-log-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("lever.log")
    }

    fn mode(path: &PathBuf) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_full_log_moves_to_one_backup_and_both_stay_private() {
        let path = temp_log("rotate");
        let backup = PathBuf::from(format!("{}.1", path.display()));
        let mut log = Log::open_with_limit(path.clone(), 200).unwrap();

        log.write(Event::Start, &"a".repeat(120)).unwrap();
        assert!(!backup.exists(), "one line under the limit must not rotate");
        log.write(Event::Connection, &"b".repeat(120)).unwrap();

        let old = std::fs::read_to_string(&backup).unwrap();
        let new = std::fs::read_to_string(&path).unwrap();
        assert!(old.contains(&"a".repeat(120)) && !old.contains('b'), "{old}");
        assert!(new.contains(&"b".repeat(120)) && !new.contains('a'), "{new}");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&backup), 0o600);

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_rotation_that_could_not_open_the_new_log_is_finished_by_the_next_line() {
        let path = temp_log("recover");
        let backup = PathBuf::from(format!("{}.1", path.display()));
        let mut log = Log::open_with_limit(path.clone(), 200).unwrap();
        log.write(Event::Start, &"a".repeat(120)).unwrap();

        // What a rotation leaves when the move worked and the open did not:
        // the log is at its backup's name and nothing is at its own.
        std::fs::rename(&path, &backup).unwrap();

        log.write(Event::Connection, &"b".repeat(120)).unwrap();
        log.write(Event::Connection, "c").unwrap();

        let new = std::fs::read_to_string(&path).unwrap();
        assert!(new.contains(&"b".repeat(120)) && new.contains(": c\n"), "{new}");
        assert!(std::fs::read_to_string(&backup).unwrap().contains(&"a".repeat(120)));
        assert_eq!(mode(&path), 0o600);

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
