// SPDX-License-Identifier: MIT
//! Live hardware and OS summary for the Studio Settings About / System page.

use std::fs;
use std::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemSummary {
    pub hostname: String,
    pub os_name: String,
    pub kernel: String,
    pub cpu: String,
    pub memory: String,
    pub gpu: String,
    pub disk: String,
}

pub fn parse_os_release(text: &str) -> String {
    for line in text.lines() {
        if let Some(val) = line.strip_prefix("PRETTY_NAME=") {
            return val.trim().trim_matches('"').to_string();
        }
    }
    "Void Linux".into()
}

pub fn parse_cpuinfo(text: &str) -> String {
    let mut model = None;
    let mut cores = 0usize;
    for line in text.lines() {
        if line.starts_with("processor") {
            cores += 1;
        } else if model.is_none()
            && line.starts_with("model name")
            && let Some((_, val)) = line.split_once(':')
        {
            model = Some(val.trim().to_string());
        }
    }
    let name = model.unwrap_or_else(|| "Unknown CPU".into());
    if cores > 0 {
        format!("{name} ({cores} threads)")
    } else {
        name
    }
}

pub fn parse_meminfo(text: &str) -> String {
    let mut total_kb: Option<u64> = None;
    let mut avail_kb: Option<u64> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = rest.split_whitespace().next().and_then(|s| s.parse().ok());
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            avail_kb = rest.split_whitespace().next().and_then(|s| s.parse().ok());
        }
    }
    match (total_kb, avail_kb) {
        (Some(total), Some(avail)) => {
            let used = total.saturating_sub(avail);
            format!(
                "{} / {} used",
                format_bytes(used * 1024),
                format_bytes(total * 1024)
            )
        }
        (Some(total), None) => format_bytes(total * 1024),
        _ => "Unknown".into(),
    }
}

pub fn format_bytes(bytes: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else {
        format!("{:.0} MiB", b / MIB)
    }
}

fn detect_gpu() -> String {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return "Unknown GPU".into();
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let fname = entry.file_name().to_string_lossy().into_owned();
        if !fname.starts_with("card") || fname.contains('-') {
            continue;
        }
        let dev = entry.path().join("device");
        let driver = fs::read_to_string(dev.join("uevent"))
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|l| l.strip_prefix("DRIVER=").map(str::to_string))
            });
        let vendor = fs::read_to_string(dev.join("vendor"))
            .ok()
            .map(|s| s.trim().to_ascii_lowercase());
        let label = match (vendor.as_deref(), driver.as_deref()) {
            (Some("0x10de"), _) => "NVIDIA GPU".to_string(),
            (Some("0x1002"), _) => "AMD Radeon GPU".to_string(),
            (Some("0x8086"), _) => "Intel Graphics".to_string(),
            (_, Some("virtio-pci" | "virtio_gpu")) => "VirtIO GPU".to_string(),
            (_, Some(d)) => format!("{d} GPU"),
            _ => continue,
        };
        if !names.contains(&label) {
            names.push(label);
        }
    }
    if names.is_empty() {
        "Software / Unknown GPU".into()
    } else {
        names.join(", ")
    }
}

fn detect_root_disk() -> String {
    let Ok(out) = Command::new("df").args(["-B1", "-T", "/"]).output() else {
        return "Unknown".into();
    };
    if !out.status.success() {
        return "Unknown".into();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(line) = text.lines().nth(1) else {
        return "Unknown".into();
    };
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() >= 5
        && let (Ok(total), Ok(used)) = (cols[2].parse::<u64>(), cols[3].parse::<u64>())
    {
        let fstype = cols[1];
        return format!(
            "{} / {} ({fstype})",
            format_bytes(used),
            format_bytes(total)
        );
    }
    "Unknown".into()
}

pub fn collect() -> SystemSummary {
    let hostname = fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "crowbaros".into());
    let os_name = fs::read_to_string("/etc/os-release")
        .map(|s| parse_os_release(&s))
        .unwrap_or_else(|_| "Void Linux".into());
    let kernel = fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "Linux".into());
    let cpu = fs::read_to_string("/proc/cpuinfo")
        .map(|s| parse_cpuinfo(&s))
        .unwrap_or_else(|_| "Unknown CPU".into());
    let memory = fs::read_to_string("/proc/meminfo")
        .map(|s| parse_meminfo(&s))
        .unwrap_or_else(|_| "Unknown".into());
    SystemSummary {
        hostname,
        os_name,
        kernel,
        cpu,
        memory,
        gpu: detect_gpu(),
        disk: detect_root_disk(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_os_cpu_and_memory_cleanly() {
        assert_eq!(
            parse_os_release("NAME=\"Void\"\nPRETTY_NAME=\"Void Linux\"\n"),
            "Void Linux"
        );
        let cpu = "processor\t: 0\nmodel name\t: AMD Ryzen 7\nprocessor\t: 1\nmodel name\t: AMD Ryzen 7\n";
        assert_eq!(parse_cpuinfo(cpu), "AMD Ryzen 7 (2 threads)");
        let mem = "MemTotal:       4194304 kB\nMemAvailable:   3145728 kB\n";
        assert_eq!(parse_meminfo(mem), "1.0 GiB / 4.0 GiB used");
    }
}
