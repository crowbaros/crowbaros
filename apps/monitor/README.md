# Monitor

Monitor shows live CPU (total and per core), memory and swap, disk throughput,
network traffic, and GPU utilization and VRAM when the driver exposes them.
It reads `/proc` and DRM sysfs directly. NVIDIA metrics use the installed
`nvidia-smi`, when available. Missing metrics say **Unavailable**; they are
not shown as zero.

The Processes table searches by name, exact numeric PID, or account and sorts by
each column.
CPU percentages use 100% for one fully used core. End Process sends SIGTERM;
Force Quit asks for confirmation before sending SIGKILL. Both are restricted
to processes owned by the person, excluding Monitor itself and exited processes.
Monitor opens a pidfd and rechecks the process's start time and ownership before
signalling, so a stale row cannot target a reused PID. It never elevates privileges.

Services & Storage is read-only: Base publishes `sv status` every five seconds
through `crowbar-monitor-status`, leaving runit’s supervision directories and
control FIFOs private. Monitor reads that root-owned report from
`/run/crowbar-monitor/services` and rejects reports older than 20 seconds.
It also shows root filesystem space from
`statvfs` (including Btrfs), and the last 30 lines of
`/var/log/socklog/everything/current`. Permission errors remain visible. Monitor
changes no service or log permissions and does not ask for root access. Base
adds the person to Void’s `socklog` group so existing system logs are readable
without elevating Monitor.

Samples run outside the UI thread, once a second while focused and once every
five seconds when inactive (including a hidden workspace). Only one sample is
in flight. Charts retain 60 samples; system details refresh at most every ten
seconds while their view is open. Disk totals count whole leaf devices once,
excluding RAM/loop devices and stacked volumes; network totals exclude loopback.
New devices, reset counters, and the initial sample do not produce rate spikes.

The interface uses the shared Manrope/IBM Plex Mono fonts and follows live
CrowbarOS theme changes. The process list renders only visible rows. Errors
retain the last sample with an explicit stale-data message and retry automatically.

| Keys | Action |
| --- | --- |
| Alt+1 / Alt+2 / Alt+3 | Overview / Processes / Services & Storage |
| Ctrl+F | Search processes |
| Escape | Leave search or cancel Force Quit |
| Up / Down | Select a process when the search field is not focused |
| Ctrl+E | End the selected owned process |
| Ctrl+K | Ask to force quit the selected owned process |
| Ctrl+Enter | Confirm Force Quit |
| Tab / Shift+Tab | Move through controls |
| Enter / Space | Activate a focused button |
| Ctrl+Q | Close Monitor |

## Build and run on Void

From this directory, with Base's GPUI build dependencies installed:

```sh
python3 ../shell/packaging/prepare-gpui.py
cargo build --release --locked
cargo test --locked
target/release/crowbaros-monitor
```

From the repository root, `lab/void/desktop_check.py` builds and tests Monitor
in Base's Void build root, installs its desktop entry, and exercises the actual
window on Fulcrum in Dark and Light. It verifies live CPU/memory samples and
process listing, searches for disposable owned children, ends them with SIGTERM
and confirmed SIGKILL, checks cancellation, and opens Services & Storage. It
saves before/after, process, confirmation, and services captures plus a six-second
recording of live chart updates in each theme. The desktop check needs `ffmpeg`
on the development machine to encode those recordings.

For quick parser and process-control tests without GPUI:

```sh
cargo test --locked --no-default-features
```

`crowbaros-monitor --sample` prints one JSON sample without opening a window.
The first sample has no CPU/rate delta. Its output contains process names,
accounts, and system logs: inspect it locally, not in public evidence from a
personal machine. The desktop check uses only the disposable lab guest.
`CROWBAR_MONITOR_CHECK=1` enables count/availability-only diagnostics for that
check; it does not log process names, account names, or log contents.

Reference semantics: [Linux procfs](https://docs.kernel.org/filesystems/proc.html)
and [pidfd signals](https://man7.org/linux/man-pages/man2/pidfd_send_signal.2.html).
