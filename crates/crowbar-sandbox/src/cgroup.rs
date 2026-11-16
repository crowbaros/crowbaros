//! The Warden's cgroup v2 subtree: one cgroup for each compartment.
//!
//! The cgroup carries the compartment's budget, and it is how a compartment
//! is stopped. `cgroup.kill` ends every process in it, including one that
//! forked twice and started a new session to get away (ADR 0016).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::order::Budget;

/// The controllers a compartment's budget needs.
const CONTROLLERS: [&str; 4] = ["cpu", "io", "memory", "pids"];

/// `statfs` reports this for a cgroup v2 filesystem.
const CGROUP2_MAGIC: i64 = 0x6367_7270;

/// The length of the period a CPU budget is counted over, in microseconds.
const CPU_PERIOD: u64 = 100_000;

/// The Warden's two processes that stand between it and the program. They
/// count against the cgroup, and the budget is for the program.
const OWN_PROCESSES: u64 = 2;

/// How long a stop waits for the kernel to end every process.
const STOP_WAIT: Duration = Duration::from_secs(30);

pub(crate) struct Subtree {
    root: PathBuf,
}

impl Subtree {
    /// Opens the subtree at `root`, creating it. Anything a Warden before
    /// this one left running there is stopped: this Warden cannot answer
    /// for a compartment it did not start.
    pub(crate) fn open(root: &Path) -> io::Result<Self> {
        fs::create_dir_all(root)?;
        if filesystem_magic(root)? != CGROUP2_MAGIC {
            return Err(io::Error::other(format!(
                "{} is not on a cgroup v2 filesystem",
                root.display()
            )));
        }
        let enable = CONTROLLERS.map(|controller| format!("+{controller}")).join(" ");
        for cgroup in [root.parent().unwrap_or(root), root] {
            let control = cgroup.join("cgroup.subtree_control");
            fs::write(&control, &enable).map_err(|e| {
                io::Error::other(format!(
                    "cannot give the {CONTROLLERS:?} controllers to cgroups under {}: {e}",
                    cgroup.display()
                ))
            })?;
        }
        let subtree = Subtree {
            root: root.to_path_buf(),
        };
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                let left = Cgroup { path: entry.path() };
                left.kill()?;
                left.remove()?;
            }
        }
        Ok(subtree)
    }

    /// Creates the cgroup of a new compartment, with its budget. Fails with
    /// `AlreadyExists` when a compartment with this ID is live.
    pub(crate) fn create(&self, compartment: &str, budget: &Budget) -> io::Result<Cgroup> {
        let cgroup = Cgroup {
            path: self.root.join(compartment),
        };
        fs::create_dir(&cgroup.path)?;
        cgroup.set_budget(budget).inspect_err(|_| {
            let _ = cgroup.remove();
        })?;
        Ok(cgroup)
    }
}

pub(crate) struct Cgroup {
    path: PathBuf,
}

impl Cgroup {
    fn set_budget(&self, budget: &Budget) -> io::Result<()> {
        self.set("memory.max", &budget.memory_bytes.to_string())?;
        // No swap on top of the memory budget, where the kernel counts swap.
        match self.set("memory.swap.max", "0") {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        // A compartment that reaches its memory budget is stopped whole,
        // never left running with one process missing.
        self.set("memory.oom.group", "1")?;
        self.set(
            "pids.max",
            &(budget.processes.saturating_add(OWN_PROCESSES)).to_string(),
        )?;
        let quota = (budget.cpu_millicores.saturating_mul(CPU_PERIOD) / 1000).max(1000);
        self.set("cpu.max", &format!("{quota} {CPU_PERIOD}"))?;
        let bytes = budget.io_bytes_per_second;
        for disk in disks()? {
            self.set("io.max", &format!("{disk} rbps={bytes} wbps={bytes}"))?;
        }
        Ok(())
    }

    /// Moves a process into the cgroup.
    pub(crate) fn add(&self, pid: u32) -> io::Result<()> {
        self.set("cgroup.procs", &pid.to_string())
    }

    /// Ends every process in the cgroup, and returns when none is left.
    pub(crate) fn kill(&self) -> io::Result<()> {
        self.set("cgroup.kill", "1")?;
        let deadline = Instant::now() + STOP_WAIT;
        while self.count("cgroup.events", "populated")? != 0 {
            if Instant::now() > deadline {
                return Err(io::Error::other(format!(
                    "{} still holds processes",
                    self.path.display()
                )));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    pub(crate) fn remove(&self) -> io::Result<()> {
        fs::remove_dir(&self.path)
    }

    /// How many times the kernel ended the compartment for memory.
    pub(crate) fn memory_stops(&self) -> io::Result<u64> {
        self.count("memory.events", "oom_kill")
    }

    /// How many times a new process was refused for the process budget.
    pub(crate) fn processes_refused(&self) -> io::Result<u64> {
        self.count("pids.events", "max")
    }

    fn set(&self, file: &str, value: &str) -> io::Result<()> {
        let path = self.path.join(file);
        fs::write(&path, value).map_err(|e| io::Error::new(e.kind(), format!("{} = {value:?}: {e}", path.display())))
    }

    /// The number after `key` in a file of `key number` lines.
    fn count(&self, file: &str, key: &str) -> io::Result<u64> {
        let path = self.path.join(file);
        let text = fs::read_to_string(&path)?;
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.trim().parse().ok())
            .ok_or_else(|| io::Error::other(format!("{} has no {key} line", path.display())))
    }
}

fn filesystem_magic(path: &Path) -> io::Result<i64> {
    let path = crate::sys::cstring(path)?;
    // SAFETY: an all-zero `statfs` is valid, `path` is a C string, and the
    // call writes only inside `stat`.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    crate::sys::checked(unsafe { libc::statfs(path.as_ptr(), &mut stat) })?;
    Ok(stat.f_type as i64)
}

/// The `major:minor` of every disk the machine has now. A disk that is
/// plugged in later has no limit in a compartment that is already running.
fn disks() -> io::Result<Vec<String>> {
    let mut disks = Vec::new();
    for entry in fs::read_dir("/sys/block")? {
        let entry = entry?;
        // A device with no medium, such as an empty loop device or optical
        // drive, has no size, and the kernel takes no limit for it.
        let size = fs::read_to_string(entry.path().join("size"))?;
        if size.trim() != "0" {
            disks.push(fs::read_to_string(entry.path().join("dev"))?.trim().to_string());
        }
    }
    disks.sort();
    Ok(disks)
}
