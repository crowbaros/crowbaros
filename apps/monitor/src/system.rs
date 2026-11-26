// SPDX-License-Identifier: MIT
use crate::metrics::*;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

const LIMIT: u64 = 4 * 1024 * 1024;
fn read(path: impl AsRef<Path>) -> io::Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(io::Error::other("metric file exceeds the read limit"));
    }
    String::from_utf8(bytes).map_err(|_| io::Error::other("metric file is not UTF-8"))
}

/// Fixed system commands only; bounded output, deadline, and child reaping.
fn command(program: &str, args: &[&str]) -> Result<String, String> {
    let mut child = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().ok_or("No command output")?;
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut data = Vec::new();
        let result = stdout.take(65537).read_to_end(&mut data).map(|_| data);
        let _ = tx.send(result);
    });
    let result = rx.recv_timeout(Duration::from_secs(2));
    // Kill even on an output-limit result: the producer may be blocked writing.
    let _ = child.kill();
    let status = child.wait();
    let _ = reader.join();
    let data = result
        .map_err(|_| "Timed out reading system status".to_string())?
        .map_err(|e| e.to_string())?;
    if data.len() > 65536 {
        return Err("System status output exceeded the limit".into());
    }
    // sv reports nonzero for services it cannot inspect; keep that explanation.
    if data.is_empty() && !status.map_err(|e| e.to_string())?.success() {
        return Err("System command unavailable or permission denied".into());
    }
    Ok(String::from_utf8_lossy(&data).trim().to_string())
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Gpu {
    pub name: String,
    pub utilization: Option<f64>,
    pub used: Option<u64>,
    pub total: Option<u64>,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Details {
    pub services: Vec<String>,
    pub storage: String,
    pub logs: String,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct Snapshot {
    pub cpu: BTreeMap<String, Option<f64>>,
    pub memory: Memory,
    pub disk_read: Option<f64>,
    pub disk_write: Option<f64>,
    pub net_read: Option<f64>,
    pub net_write: Option<f64>,
    pub gpus: Vec<Gpu>,
    pub processes: Vec<Process>,
    pub details: Details,
    pub warnings: Vec<String>,
}

struct Raw {
    at: Instant,
    cpu: BTreeMap<String, CpuTime>,
    disks: BTreeMap<String, Counters>,
    network: BTreeMap<String, Counters>,
    processes: Vec<Process>,
}
pub struct Collector {
    previous: Option<Raw>,
    details: Details,
    details_at: Option<Instant>,
}
impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}
impl Collector {
    pub fn new() -> Self {
        Self {
            previous: None,
            details: Details::default(),
            details_at: None,
        }
    }
    pub fn sample(&mut self, include_details: bool) -> Result<Snapshot, String> {
        let mut warnings = Vec::new();
        let cpu = parse_cpu(&read("/proc/stat").map_err(|e| format!("CPU metrics: {e}"))?);
        if !cpu.contains_key("cpu") {
            return Err("CPU metrics could not be read. Waiting to retry.".into());
        }
        let memory = parse_memory(&read("/proc/meminfo").map_err(|e| format!("Memory metrics: {e}"))?)
            .ok_or("Memory metrics could not be read. Waiting to retry.")?;
        let mut disks = match read("/proc/diskstats") {
            Ok(s) => parse_disks(&s),
            Err(_) => {
                warnings.push("Disk throughput unavailable".into());
                BTreeMap::new()
            }
        };
        // Count leaf whole devices once, excluding RAM/loop and stacked volumes.
        disks.retain(|name, _| {
            let path = Path::new("/sys/block").join(name);
            path.exists()
                && !name.starts_with("loop")
                && !name.starts_with("ram")
                && fs::read_dir(path.join("slaves")).is_ok_and(|mut entries| entries.next().is_none())
        });
        let mut network = match read("/proc/net/dev") {
            Ok(s) => parse_network(&s),
            Err(_) => {
                warnings.push("Network throughput unavailable".into());
                BTreeMap::new()
            }
        };
        network.remove("lo");
        let users: BTreeMap<u32, String> = read("/etc/passwd")
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let f: Vec<_> = line.split(':').collect();
                Some((f.get(2)?.parse().ok()?, f.first()?.to_string()))
            })
            .collect();
        let mut processes = Vec::new();
        for entry in fs::read_dir("/proc")
            .map_err(|e| format!("Process list: {e}"))?
            .flatten()
        {
            if entry.file_name().to_str().and_then(|s| s.parse::<u32>().ok()).is_none() {
                continue;
            }
            if let Ok(mut p) = read_process(&entry.path()) {
                p.user = users.get(&p.uid).cloned().unwrap_or_else(|| p.uid.to_string());
                processes.push(p);
            }
        }
        processes.sort_by_key(|p| p.pid);
        let now = Instant::now();
        let mut out = Snapshot {
            memory,
            gpus: gpus(),
            warnings,
            ..Snapshot::default()
        };
        let raw = Raw {
            at: now,
            cpu,
            disks,
            network,
            processes,
        };
        for (name, time) in &raw.cpu {
            out.cpu.insert(
                name.clone(),
                self.previous
                    .as_ref()
                    .and_then(|p| p.cpu.get(name))
                    .and_then(|old| cpu_percent(*old, *time)),
            );
        }
        out.processes = raw.processes.clone();
        if let Some(old) = &self.previous {
            let elapsed = now.duration_since(old.at).as_secs_f64();
            (out.disk_read, out.disk_write) = rates(&old.disks, &raw.disks, elapsed);
            (out.net_read, out.net_write) = rates(&old.network, &raw.network, elapsed);
            // CPU% per process is in cores: one fully used core is 100%.
            let hz = clock_ticks();
            let prior: BTreeMap<_, _> = old.processes.iter().map(|p| ((p.pid, p.start), p.ticks)).collect();
            for p in &mut out.processes {
                p.cpu = prior
                    .get(&(p.pid, p.start))
                    .and_then(|ticks| rate(*ticks, p.ticks, elapsed))
                    .map(|r| 100. * r / hz as f64);
            }
        }
        if include_details
            && self
                .details_at
                .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(10))
        {
            self.details = details();
            self.details_at = Some(now);
        }
        out.details = self.details.clone();
        self.previous = Some(raw);
        Ok(out)
    }
}
fn rates(
    before: &BTreeMap<String, Counters>,
    after: &BTreeMap<String, Counters>,
    elapsed: f64,
) -> (Option<f64>, Option<f64>) {
    let mut pairs = after
        .iter()
        .filter_map(|(name, now)| {
            let old = before.get(name)?;
            Some((rate(old.read, now.read, elapsed)?, rate(old.write, now.write, elapsed)?))
        })
        .peekable();
    if pairs.peek().is_none() {
        return (None, None);
    }
    let (r, w) = pairs.fold((0., 0.), |a, b| (a.0 + b.0, a.1 + b.1));
    (Some(r), Some(w))
}
fn page_size() -> u64 {
    // SAFETY: sysconf takes a constant selector and does not access pointers.
    let n = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if n > 0 { n as u64 } else { 4096 }
}
fn clock_ticks() -> u64 {
    // SAFETY: sysconf takes a constant selector and does not access pointers.
    let n = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if n > 0 { n as u64 } else { 100 }
}
pub fn person_uid() -> u32 {
    // SAFETY: geteuid has no arguments or memory requirements.
    unsafe { libc::geteuid() }
}
fn read_process(path: &Path) -> io::Result<Process> {
    parse_process(&read(path.join("stat"))?, &read(path.join("status"))?, page_size())
        .ok_or_else(|| io::Error::other("Process exited or its status is unreadable"))
}

#[derive(Clone, Copy, Debug)]
pub enum Signal {
    Terminate,
    Kill,
}
/// Open a stable process handle, recheck identity/ownership, then signal it.
/// No numeric-PID fallback: old kernels fail safely instead of risking PID reuse.
pub fn signal_process(selected: &Process, signal: Signal) -> Result<(), String> {
    let uid = person_uid();
    if !can_signal(selected, uid, std::process::id()) {
        return Err("Only your own running processes can be ended.".into());
    }
    // SAFETY: pidfd_open takes a positive PID and zero flags; no pointers.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, selected.pid as libc::pid_t, 0u32) };
    if fd < 0 {
        return Err(format!("Could not open the process: {}", io::Error::last_os_error()));
    }
    // SAFETY: syscall returned a new, owned descriptor. RAII closes it once.
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    let current = read_process(&Path::new("/proc").join(selected.pid.to_string())).map_err(|e| e.to_string())?;
    if current.start != selected.start || !can_signal(&current, uid, std::process::id()) {
        return Err("The process changed or is no longer yours. Select it again.".into());
    }
    let sig = match signal {
        Signal::Terminate => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: valid owned pidfd, supported signal, null siginfo requests kernel defaults.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd.as_raw_fd(),
            sig,
            std::ptr::null::<libc::siginfo_t>(),
            0u32,
        )
    };
    if result < 0 {
        Err(format!("Could not end the process: {}", io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn number(path: impl AsRef<Path>) -> Option<u64> {
    read(path).ok()?.trim().parse().ok()
}
fn gpus() -> Vec<Gpu> {
    let mut result = Vec::new();
    let mut nvidia = false;
    if let Ok(entries) = fs::read_dir("/sys/class/drm") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name
                .strip_prefix("card")
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
            {
                continue;
            }
            let dev = entry.path().join("device");
            let vendor = read(dev.join("vendor")).unwrap_or_default();
            if vendor.trim() == "0x10de" && Path::new("/usr/bin/nvidia-smi").exists() {
                nvidia = true;
                continue;
            }
            result.push(Gpu {
                name,
                utilization: number(dev.join("gpu_busy_percent"))
                    .filter(|v| *v <= 100)
                    .map(|v| v as f64),
                used: number(dev.join("mem_info_vram_used")),
                total: number(dev.join("mem_info_vram_total")),
            });
        }
    }
    if nvidia {
        if let Ok(text) = command(
            "/usr/bin/nvidia-smi",
            &[
                "--query-gpu=index,name,utilization.gpu,memory.used,memory.total",
                "--format=csv,noheader,nounits",
            ],
        ) {
            let parsed = parse_nvidia(&text);
            if parsed.is_empty() {
                result.push(Gpu {
                    name: "NVIDIA · metrics unavailable".into(),
                    ..Gpu::default()
                });
            } else {
                result.extend(parsed);
            }
        } else {
            result.push(Gpu {
                name: "NVIDIA · metrics unavailable".into(),
                ..Gpu::default()
            });
        }
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}
fn parse_nvidia(text: &str) -> Vec<Gpu> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<_> = line.split(',').map(str::trim).collect();
            if f.len() != 5 || f[0].parse::<u32>().is_err() {
                return None;
            }
            Some(Gpu {
                name: format!("{} · {}", f[0], f[1]),
                utilization: f[2]
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite() && (0. ..=100.).contains(v)),
                used: f[3].parse::<u64>().ok().and_then(|v| v.checked_mul(1024 * 1024)),
                total: f[4].parse::<u64>().ok().and_then(|v| v.checked_mul(1024 * 1024)),
            })
        })
        .collect()
}
fn details() -> Details {
    let services = match fs::read_dir("/var/service") {
        Ok(entries) => {
            let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            paths.sort();
            let args: Vec<_> = std::iter::once("status".to_string())
                .chain(paths.into_iter().take(256).map(|p| p.to_string_lossy().into_owned()))
                .collect();
            if args.len() == 1 {
                vec!["No runit services found.".into()]
            } else {
                command("/usr/bin/sv", &args.iter().map(String::as_str).collect::<Vec<_>>())
                    .unwrap_or_else(|e| format!("Service status unavailable: {e}"))
                    .lines()
                    .map(str::to_owned)
                    .collect()
            }
        }
        Err(_) => vec!["Service status unavailable. /var/service is not readable.".into()],
    };
    let mut storage = root_usage().unwrap_or_else(|e| format!("Root filesystem usage unavailable: {e}"));
    if read("/proc/mounts").is_ok_and(|s| {
        s.lines().any(|l| {
            let f: Vec<_> = l.split_whitespace().collect();
            f.get(1) == Some(&"/") && f.get(2) == Some(&"btrfs")
        })
    }) {
        storage.push_str("\n\nBtrfs\n");
        storage.push_str(
            &command("/usr/bin/btrfs", &["filesystem", "usage", "-b", "/"]).unwrap_or_else(|_| {
                "Detailed Btrfs usage is not readable by this account. Filesystem totals are shown above.".into()
            }),
        );
    }
    let logs = log_tail(Path::new("/var/log/socklog/everything/current"))
        .unwrap_or_else(|_| "Recent system logs are not readable by this account.".into());
    Details {
        services,
        storage,
        logs,
    }
}
fn root_usage() -> io::Result<String> {
    let mut out = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: '/' is NUL-terminated; out points to sufficient writable storage.
    if unsafe { libc::statvfs(c"/".as_ptr(), out.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs initialized the structure on success.
    let s = unsafe { out.assume_init() };
    let total = s.f_blocks.saturating_mul(s.f_frsize);
    let free = s.f_bfree.saturating_mul(s.f_frsize);
    let available = s.f_bavail.saturating_mul(s.f_frsize);
    Ok(format!(
        "Root filesystem /\n{} used of {} · {} available",
        bytes(total.saturating_sub(free) as f64),
        bytes(total as f64),
        bytes(available as f64)
    ))
}
fn log_tail(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(16384);
    file.seek(SeekFrom::Start(start))?;
    let mut data = Vec::new();
    file.take(16384).read_to_end(&mut data)?;
    let text = String::from_utf8_lossy(&data);
    let lines: Vec<_> = text.lines().skip(usize::from(start > 0)).collect();
    Ok(lines
        .iter()
        .skip(lines.len().saturating_sub(30))
        .copied()
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nvidia_devices_have_distinct_history_keys_and_missing_metrics_stay_unknown() {
        let cards = parse_nvidia("0, Example GPU, 25, 512, 4096\n1, Example GPU, [Not Supported], N/A, N/A");
        assert_eq!(cards.len(), 2);
        assert_ne!(cards[0].name, cards[1].name);
        assert_eq!(cards[0].utilization, Some(25.));
        assert_eq!(cards[0].used, Some(512 * 1024 * 1024));
        assert!(cards[1].utilization.is_none());
        assert!(cards[1].used.is_none());
        assert!(parse_nvidia("Failed to initialize the driver").is_empty());
        assert!(
            parse_nvidia("0, Example, NaN, 18446744073709551615, 4")[0]
                .used
                .is_none()
        );
    }
    #[test]
    fn device_changes_do_not_create_spikes() {
        let before = BTreeMap::from([("a".into(), Counters { read: 100, write: 200 })]);
        let after = BTreeMap::from([
            ("a".into(), Counters { read: 120, write: 260 }),
            (
                "b".into(),
                Counters {
                    read: 999999,
                    write: 999999,
                },
            ),
        ]);
        assert_eq!(rates(&before, &after, 2.), (Some(10.), Some(30.)));
        assert_eq!(rates(&BTreeMap::new(), &after, 2.), (None, None));
    }
    #[test]
    fn live_metrics_read_without_privileges() {
        let mut c = Collector::new();
        let sample = c.sample(false).unwrap();
        assert!(sample.memory.total > 0);
        assert!(sample.cpu.contains_key("cpu"));
        assert!(sample.processes.iter().any(|p| p.pid == std::process::id()));
        assert!(sample.cpu["cpu"].is_none());
    }
    #[test]
    fn ending_an_owned_child_and_rejecting_stale_identity() {
        if person_uid() == 0 {
            return;
        } // Base's build root is root; native check runs as the person.
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let p = read_process(&Path::new("/proc").join(child.id().to_string())).unwrap();
        let stale = Process {
            start: p.start + 1,
            ..p.clone()
        };
        assert!(signal_process(&stale, Signal::Kill).is_err());
        assert!(child.try_wait().unwrap().is_none());
        signal_process(&p, Signal::Terminate).unwrap();
        assert!(!child.wait().unwrap().success());
        assert!(signal_process(&p, Signal::Kill).is_err());
    }
    #[test]
    fn system_commands_have_output_and_time_limits() {
        assert_eq!(command("/bin/echo", &["status"]).unwrap(), "status");
        assert!(command("/bin/false", &[]).is_err());
        assert!(command("/bin/sleep", &["10"]).unwrap_err().contains("Timed out"));
        assert!(
            command("/usr/bin/head", &["-c", "70000", "/dev/zero"])
                .unwrap_err()
                .contains("limit")
        );
    }
    #[test]
    fn force_quit_ends_only_the_owned_child() {
        if person_uid() == 0 {
            return;
        }
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let p = read_process(&Path::new("/proc").join(child.id().to_string())).unwrap();
        signal_process(&p, Signal::Kill).unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGKILL));
    }
    #[test]
    fn root_and_self_are_refused() {
        let own = read_process(Path::new("/proc/self")).unwrap();
        assert!(signal_process(&own, Signal::Terminate).is_err());
        if let Ok(root) = read_process(Path::new("/proc/1")) {
            assert!(signal_process(&root, Signal::Kill).is_err());
        }
    }
}
