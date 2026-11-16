//! The user IDs the Warden gives to compartments.
//!
//! Each start takes the next user ID of a range Base reserves, and no start
//! takes one that an earlier start had, until the machine restarts
//! (ADR 0016). The next ID is kept in a file in the Warden's runtime
//! directory, which is on a filesystem that is empty after a restart. So a
//! Warden that restarts carries on where the last one stopped.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

/// Why no user ID was given.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NoUserId {
    /// Every ID of the range has been used since the machine started.
    Spent,
    /// The record of the next ID could not be read or written, or names an
    /// ID outside the range.
    Record(String),
}

pub(crate) struct UserIds {
    record: PathBuf,
    range: Range<u32>,
}

impl UserIds {
    pub(crate) fn open(runtime_dir: &Path, range: Range<u32>) -> io::Result<Self> {
        let ids = UserIds {
            record: runtime_dir.join("next-user-id"),
            range,
        };
        ids.locked()?;
        Ok(ids)
    }

    /// The next user ID. The record moves on before the ID is returned, so a
    /// Warden that dies here wastes an ID and never gives one twice.
    pub(crate) fn take(&self) -> Result<u32, NoUserId> {
        let record = |e: io::Error| NoUserId::Record(format!("{}: {e}", self.record.display()));
        let mut file = self.locked().map_err(record)?;
        let mut text = String::new();
        file.read_to_string(&mut text).map_err(record)?;
        let next = match text.trim() {
            "" => self.range.start,
            text => text
                .parse::<u32>()
                .ok()
                .filter(|next| (self.range.start..=self.range.end).contains(next))
                .ok_or_else(|| {
                    NoUserId::Record(format!(
                        "{} holds {text:?}, which is not in the range {:?}",
                        self.record.display(),
                        self.range
                    ))
                })?,
        };
        if next == self.range.end {
            return Err(NoUserId::Spent);
        }
        file.set_len(0).map_err(record)?;
        file.seek(SeekFrom::Start(0)).map_err(record)?;
        file.write_all((next + 1).to_string().as_bytes()).map_err(record)?;
        file.sync_all().map_err(record)?;
        Ok(next)
    }

    /// The record, open and locked until the file is closed.
    fn locked(&self) -> io::Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.record)?;
        // SAFETY: `file` is an open descriptor for the whole call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("crowbar-uids-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn every_start_gets_a_user_id_no_start_had_before() {
        let dir = scratch("fresh");
        let ids = UserIds::open(&dir, 5000..5004).unwrap();
        assert_eq!([ids.take(), ids.take(), ids.take()], [Ok(5000), Ok(5001), Ok(5002)]);
    }

    #[test]
    fn a_warden_that_restarts_carries_on_where_the_last_one_stopped() {
        let dir = scratch("restart");
        let first = UserIds::open(&dir, 5000..5004).unwrap();
        assert_eq!([first.take(), first.take()], [Ok(5000), Ok(5001)]);
        drop(first);

        let second = UserIds::open(&dir, 5000..5004).unwrap();
        assert_eq!(second.take(), Ok(5002));
    }

    #[test]
    fn two_wardens_on_one_record_never_give_the_same_id() {
        let dir = scratch("shared");
        let givers = [(); 4].map(|()| UserIds::open(&dir, 5000..5400).unwrap());
        let mut given: Vec<u32> = std::thread::scope(|scope| {
            let threads: Vec<_> = givers
                .iter()
                .map(|ids| scope.spawn(move || (0..100).map(|_| ids.take().unwrap()).collect::<Vec<_>>()))
                .collect();
            threads.into_iter().flat_map(|thread| thread.join().unwrap()).collect()
        });
        given.sort_unstable();
        assert_eq!(given, (5000..5400).collect::<Vec<_>>());
    }

    #[test]
    fn a_spent_range_gives_nothing_more() {
        let dir = scratch("spent");
        let ids = UserIds::open(&dir, 5000..5002).unwrap();
        assert_eq!([ids.take(), ids.take()], [Ok(5000), Ok(5001)]);
        assert_eq!(ids.take(), Err(NoUserId::Spent));
        // A restart of the Warden does not start the range again.
        assert_eq!(UserIds::open(&dir, 5000..5002).unwrap().take(), Err(NoUserId::Spent));
    }

    #[test]
    fn a_record_outside_the_range_gives_nothing() {
        // The range changed under a running machine, or the record is
        // damaged. Either way an ID from it could be one already given.
        for (name, held) in [("below", "4999"), ("above", "5003"), ("text", "five thousand")] {
            let dir = scratch(name);
            std::fs::write(dir.join("next-user-id"), held).unwrap();
            let ids = UserIds::open(&dir, 5000..5002).unwrap();
            assert!(matches!(ids.take(), Err(NoUserId::Record(_))), "{name}");
        }
    }
}
