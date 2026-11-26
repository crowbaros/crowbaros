// SPDX-License-Identifier: MIT
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuTime {
    pub busy: u64,
    pub total: u64,
}

/// Guest times already belong to user/nice; do not count them twice.
/// A reset (including decreasing iowait) starts a new measurement interval.
pub fn parse_cpu(text: &str) -> BTreeMap<String, CpuTime> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let name = words.next()?;
            if name != "cpu"
                && !name
                    .strip_prefix("cpu")
                    .is_some_and(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()))
            {
                return None;
            }
            let fields = words
                .take(8)
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            if fields.len() < 4 {
                return None;
            }
            let total = fields.iter().try_fold(0u64, |a, b| a.checked_add(*b))?;
            let idle = fields[3].checked_add(fields.get(4).copied().unwrap_or(0))?;
            Some((
                name.to_string(),
                CpuTime {
                    busy: total.checked_sub(idle)?,
                    total,
                },
            ))
        })
        .collect()
}

pub fn cpu_percent(before: CpuTime, after: CpuTime) -> Option<f64> {
    let total = after.total.checked_sub(before.total)?;
    let busy = after.busy.checked_sub(before.busy)?;
    (total > 0 && busy <= total).then(|| 100.0 * busy as f64 / total as f64)
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Memory {
    pub total: u64,
    pub used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

pub fn parse_memory(text: &str) -> Option<Memory> {
    let values: BTreeMap<_, _> = text
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            let mut words = value.split_whitespace();
            let n = words.next()?.parse::<u64>().ok()?;
            (words.next()? == "kB").then_some((key, n.checked_mul(1024)?))
        })
        .collect();
    let total = *values.get("MemTotal")?;
    let available = *values.get("MemAvailable")?;
    let swap_total = *values.get("SwapTotal")?;
    Some(Memory {
        total,
        used: total.saturating_sub(available),
        swap_total,
        swap_used: swap_total.saturating_sub(*values.get("SwapFree")?),
    })
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Counters {
    pub read: u64,
    pub write: u64,
}

pub fn parse_network(text: &str) -> BTreeMap<String, Counters> {
    text.lines()
        .filter_map(|line| {
            let (name, values) = line.split_once(':')?;
            let fields: Vec<_> = values.split_whitespace().collect();
            Some((
                name.trim().to_string(),
                Counters {
                    read: fields.first()?.parse().ok()?,
                    write: fields.get(8)?.parse().ok()?,
                },
            ))
        })
        .collect()
}

/// Linux diskstats sectors are always 512 bytes, regardless of device geometry.
pub fn parse_disks(text: &str) -> BTreeMap<String, Counters> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<_> = line.split_whitespace().collect();
            Some((
                f.get(2)?.to_string(),
                Counters {
                    read: f.get(5)?.parse::<u64>().ok()?.checked_mul(512)?,
                    write: f.get(9)?.parse::<u64>().ok()?.checked_mul(512)?,
                },
            ))
        })
        .collect()
}

pub fn rate(before: u64, after: u64, elapsed: f64) -> Option<f64> {
    if !elapsed.is_finite() || elapsed <= 0.0 {
        return None;
    }
    Some(after.checked_sub(before)? as f64 / elapsed)
}

pub fn bytes(value: f64) -> String {
    if !value.is_finite() || value < 0.0 {
        return "Unavailable".into();
    }
    let mut scaled = value;
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut unit = 0;
    while scaled >= 1024. && unit < units.len() - 1 {
        scaled /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{scaled:.0} B")
    } else {
        format!("{scaled:.1} {}", units[unit])
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Process {
    pub pid: u32,
    pub name: String,
    pub state: String,
    pub uid: u32,
    pub user: String,
    pub ticks: u64,
    pub start: u64,
    pub rss: u64,
    pub cpu: Option<f64>,
}

/// Split on the last ')' because comm may contain spaces and parentheses.
pub fn parse_process(stat: &str, status: &str, page_size: u64) -> Option<Process> {
    let (pid, rest) = stat.split_once(" (")?;
    let (name, fields) = rest.rsplit_once(") ")?;
    let f: Vec<_> = fields.split_whitespace().collect();
    let uid_line = status.lines().find_map(|s| s.strip_prefix("Uid:"))?;
    let uids = uid_line
        .split_whitespace()
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if uids.len() != 4 {
        return None;
    }
    // Any elevated/mixed identity is displayed but cannot be acted on.
    let uid = if uids.iter().all(|u| *u == uids[0]) { uids[0] } else { 0 };
    Some(Process {
        pid: pid.parse().ok()?,
        name: name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect(),
        state: f.first()?.to_string(),
        uid,
        user: uid.to_string(),
        ticks: f
            .get(11)?
            .parse::<u64>()
            .ok()?
            .checked_add(f.get(12)?.parse::<u64>().ok()?)?,
        start: f.get(19)?.parse().ok()?,
        rss: f.get(21)?.parse::<u64>().ok()?.checked_mul(page_size)?,
        cpu: None,
    })
}

pub fn can_signal(process: &Process, person_uid: u32, own_pid: u32) -> bool {
    person_uid != 0
        && process.uid == person_uid
        && process.pid > 1
        && process.pid != own_pid
        && process.state != "Z"
        && process.state != "X"
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Name,
    Pid,
    User,
    Cpu,
    Memory,
    State,
}

pub fn process_rows(processes: &[Process], query: &str, sort: Sort, descending: bool) -> Vec<usize> {
    let query = query.to_lowercase();
    let mut rows: Vec<_> = processes
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            p.name.to_lowercase().contains(&query)
                || p.pid.to_string().contains(&query)
                || p.user.to_lowercase().contains(&query)
        })
        .map(|(i, _)| i)
        .collect();
    rows.sort_by(|a, b| {
        let (a, b) = (&processes[*a], &processes[*b]);
        let order = match sort {
            Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            Sort::Pid => a.pid.cmp(&b.pid),
            Sort::User => a.user.cmp(&b.user),
            Sort::Cpu => a.cpu.unwrap_or(-1.).total_cmp(&b.cpu.unwrap_or(-1.)),
            Sort::Memory => a.rss.cmp(&b.rss),
            Sort::State => a.state.cmp(&b.state),
        };
        (if descending { order.reverse() } else { order }).then(a.pid.cmp(&b.pid))
    });
    rows
}

#[derive(Clone, Debug)]
pub struct History {
    values: VecDeque<Option<f64>>,
    capacity: usize,
}
impl History {
    pub fn new(capacity: usize) -> Self {
        Self {
            values: VecDeque::new(),
            capacity,
        }
    }
    pub fn push(&mut self, value: Option<f64>) {
        if self.capacity == 0 {
            return;
        }
        if self.values.len() == self.capacity {
            self.values.pop_front();
        }
        self.values.push_back(value.filter(|x| x.is_finite() && *x >= 0.));
    }
    pub fn values(&self) -> Vec<Option<f64>> {
        self.values.iter().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cpu_ignores_guest_and_handles_reset() {
        let p = parse_cpu("cpu 10 2 3 80 5 0 0 0 9 2\ncpu0 10 2 3 80 5 0 0 0\nintr 100");
        assert_eq!(p.len(), 2);
        assert_eq!(p["cpu"], CpuTime { busy: 15, total: 100 });
        assert_eq!(cpu_percent(p["cpu"], CpuTime { busy: 25, total: 120 }), Some(50.));
        assert_eq!(cpu_percent(p["cpu"], p["cpu"]), None);
        assert_eq!(cpu_percent(p["cpu"], CpuTime::default()), None);
        assert!(parse_cpu("cpu 18446744073709551615 1 2 3\ncpuX 1 2 3 4").is_empty());
    }
    #[test]
    fn memory_uses_available_not_free() {
        let m = parse_memory(
            "MemTotal: 1000 kB\nMemFree: 20 kB\nMemAvailable: 400 kB\nSwapTotal: 200 kB\nSwapFree: 150 kB",
        )
        .unwrap();
        assert_eq!(m.used, 600 * 1024);
        assert_eq!(m.swap_used, 50 * 1024);
        assert!(parse_memory("MemTotal: nope kB").is_none());
    }
    #[test]
    fn traffic_parsers_and_counter_reset() {
        assert_eq!(
            parse_network("header\n eth0: 100 2 3 4 5 6 7 8 200 10 11 12")["eth0"],
            Counters { read: 100, write: 200 }
        );
        assert_eq!(
            parse_disks("8 0 sda 1 0 4 0 2 0 8 0 0 0 0")["sda"],
            Counters {
                read: 2048,
                write: 4096
            }
        );
        assert_eq!(rate(100, 160, 2.), Some(30.));
        assert_eq!(rate(100, 10, 2.), None);
        assert_eq!(rate(0, 10, 0.), None);
        assert_eq!(rate(0, 10, f64::NAN), None);
    }
    fn process() -> Process {
        parse_process(
            "42 (a name ) inside) S 1 0 0 0 0 0 0 0 0 0 10 20 0 0 0 0 1 0 999 4096 3",
            "Uid: 1000 1000 1000 1000",
            4096,
        )
        .unwrap()
    }
    #[test]
    fn process_fields_and_ownership() {
        let p = process();
        assert_eq!(p.name, "a name ) inside");
        assert_eq!(p.ticks, 30);
        assert_eq!(p.start, 999);
        assert_eq!(p.rss, 12288);
        assert!(can_signal(&p, 1000, 99));
        assert!(!can_signal(&p, 0, 99));
        assert!(!can_signal(&p, 1001, 99));
        assert!(!can_signal(&p, 1000, 42));
        assert!(!can_signal(&Process { uid: 0, ..p.clone() }, 1000, 99));
        assert!(!can_signal(&Process { state: "Z".into(), ..p }, 1000, 99));
        assert!(parse_process("bad", "Uid: 1000", 4096).is_none());
    }
    #[test]
    fn sorting_search_and_history_are_bounded() {
        let a = process();
        let b = Process {
            pid: 7,
            name: "other".into(),
            rss: 99999,
            ..a.clone()
        };
        assert_eq!(
            process_rows(&[a.clone(), b.clone()], "other", Sort::Memory, true),
            vec![1]
        );
        assert_eq!(process_rows(&[a, b], "", Sort::Memory, true), vec![1, 0]);
        let mut h = History::new(2);
        h.push(Some(1.));
        h.push(Some(2.));
        h.push(None);
        assert_eq!(h.values(), vec![Some(2.), None]);
        h.push(Some(f64::INFINITY));
        assert_eq!(h.values(), vec![None, None]);
        let mut h = History::new(0);
        h.push(Some(3.));
        assert!(h.values().is_empty());
    }
    #[test]
    fn byte_units() {
        assert_eq!(bytes(1024.), "1.0 KiB");
        assert_eq!(bytes(1024. * 1024.), "1.0 MiB");
        assert_eq!(bytes(0.), "0 B");
        assert_eq!(bytes(-1.), "Unavailable");
    }
}
