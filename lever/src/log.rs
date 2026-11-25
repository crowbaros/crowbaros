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
    Harness,
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
            Event::Harness => "harness",
            Event::Stop => "stop",
            Event::Error => "error",
        }
    }
}

pub struct Log {
    path: PathBuf,
    /// `None` between moving a full log aside and opening the next one.
    file: Option<File>,
    size: u64,
    limit: u64,
}

impl Log {
    /// Opens or creates `path` for appending, readable by the person alone.
    pub fn open(path: PathBuf) -> io::Result<Self> {
        Self::open_with_limit(path, MAX_LOG)
    }

    fn open_with_limit(path: PathBuf, limit: u64) -> io::Result<Self> {
        let mut log = Self {
            path,
            file: None,
            size: 0,
            limit,
        };
        log.file()?;
        Ok(log)
    }

    /// Writes one UTC-timestamped line, rotating to `<path>.1` before crossing
    /// the limit.
    pub fn write(&mut self, event: Event, details: &str) -> io::Result<()> {
        let escaped = details.replace('\n', "\n    ");
        let line = format!("{} {}: {escaped}\n", utc_timestamp(), event.as_str());
        let bytes = line.as_bytes();
        // The log is open before its size is read: opening it is what learns
        // the size of a log that was not open.
        self.file()?;
        if self.size > 0 && self.size + (bytes.len() as u64) > self.limit {
            self.set_aside()?;
            self.file()?;
        }
        let size = self.size;
        let file = self.file()?;
        if let Err(err) = file.write_all(bytes) {
            let _ = file.set_len(size);
            return Err(err);
        }
        self.size += bytes.len() as u64;
        Ok(())
    }
}

impl Log {
    /// Moves the full log to `<path>.1`. The log has no open file from here
    /// until `file` opens the next one, so a line can never go to the backup.
    fn set_aside(&mut self) -> io::Result<()> {
        let mut backup = self.path.as_os_str().to_os_string();
        backup.push(".1");
        std::fs::rename(&self.path, &backup)?;
        self.file = None;
        self.size = 0;
        Ok(())
    }

    /// The open log. Opens it when there is none: at the start, after a full
    /// log is moved aside, and again on each line after an open that failed.
    fn file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            let file = open_private(&self.path)?;
            self.size = file.metadata()?.len();
            self.file = Some(file);
        }
        Ok(self.file.as_mut().expect("the log was just opened"))
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

        // A rotation whose move works and whose open does not: a directory
        // stands where the new log would go, so the line that asked for the
        // rotation is lost and the log is left with no open file.
        log.set_aside().unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(log.write(Event::Connection, "lost").is_err());
        std::fs::remove_dir(&path).unwrap();

        // The next line is far under the limit, and still opens the new log.
        log.write(Event::Connection, "c").unwrap();

        let old = std::fs::read_to_string(&backup).unwrap();
        let new = std::fs::read_to_string(&path).unwrap();
        assert!(old.contains(&"a".repeat(120)) && !old.contains(": c\n"), "{old}");
        assert!(new.contains(": c\n") && !new.contains("lost"), "{new}");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&backup), 0o600);

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_log_found_where_the_next_one_opens_is_held_to_the_limit_too() {
        let path = temp_log("found");
        let backup = PathBuf::from(format!("{}.1", path.display()));
        let mut log = Log::open_with_limit(path.clone(), 200).unwrap();
        log.write(Event::Start, &"a".repeat(120)).unwrap();

        // The full log is moved aside, and before the next line a log that is
        // nearly full already stands where the next one opens.
        log.set_aside().unwrap();
        std::fs::write(&path, "x".repeat(190)).unwrap();

        log.write(Event::Connection, "c").unwrap();

        let new = std::fs::read_to_string(&path).unwrap();
        assert!(new.ends_with(": c\n") && !new.contains('x'), "{new}");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), "x".repeat(190));

        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
