# Lever's footprint

**Result (November 25, 2026): under runit in the VM, the idle Lever holds
4.27 MiB of memory (`Rss`) and answers version 1 and `status` on its surface
socket 9 ms after `sv up`, or 20 ms with the page cache dropped first. Both
are far below the starting limits of 50 MiB and 1 second.**

This is the record
[issue 373](https://github.com/crowbaros/crowbaros/issues/373) asks for, from
the second slice of [Milestone 3](https://github.com/crowbaros/crowbaros/issues/371)
([ADR 0036](../adr/0036-lever-drives-the-harnesses-a-person-has-and-crowbaros-builds-none.md)).
A coding agent built Base's image, ran the full desktop check, booted the
image in the VM, and ran the measurements over SSH. The evidence is in
[`evidence/lever-footprint-2026-11-25/`](evidence/lever-footprint-2026-11-25/).

## The VM and the image

| | |
| --- | --- |
| VM | QEMU 11.1.3 with KVM, as `base/void_base.py boot` starts it: 4 CPUs, 4096 MiB, a virtio disk, UEFI. The host's CPU is passed through |
| Commit | Branch commit `b894140f`, based on `main` at `e85074e0` |
| Image | Base, built with `base/void_base.py build --lab-account`. `lab/void/desktop_check.py` passed 140 of 140 checks on the same build |
| Kernel | `6.18.65_1` |
| Binary | `/usr/local/bin/crowbar-lever`, 2.7 MiB (2,755,328 bytes), built in release mode inside Base's Void build root with `rusqlite` bundled SQLite |
| Service | `crowbar-lever`, as runit starts it at boot: as the person, in the launcher's environment for apps, with its SQLite database in WAL mode and no harness started yet |

## Results

| | Measured | Starting limit |
| --- | --- | --- |
| Idle memory (`Rss`) | **4.27 MiB**, unchanged at 13 s and 73 s after boot | 50 MiB |
| From `sv up` to the shell's account answered version 1 and `status` | **9 ms** median of 10, from 9 to 10 | 1 second |
| The same, with the page cache dropped first | **20 ms** median of 5, from 19 to 25 | 1 second |
| From `kill -9` to the next Lever answering | **9 ms** median of 5, from 8 to 10 | |

- **Memory.** Of the 4.27 MiB `Rss`, `Pss` is 2.47–2.48 MiB, `Pss_Anon` is
  **0.25 MiB** (256 KiB of process heap/stack), and `Pss_File` is 2.22 MiB
  (mapped binary and shared libraries). After 25 restarts, the running Lever
  held 4.23 MiB `Rss` (2.55 MiB `Pss`, 0.25 MiB `Pss_Anon`).
- **Start time** covers `sv up`, runit invoking `/etc/sv/crowbar-lever/run`,
  `chpst`, `crowbar-launcher as-app`, `crowbar-lever` opening its log and
  SQLite WAL database, binding both sockets, and answering the shell's account
  with `{"version":1}` and `{"reply":"status",...}` on
  `/run/crowbar-lever/surface`. Inside `crowbar-lever` itself, `lever.log`
  records `0 ms` (< 1 ms) from process start to listening on both sockets for
  every warm, cold, and post-kill restart.
- **At boot**, creating `lever.sqlite` for the first time while the rest of
  Base's services started beside it, Lever logged `listening` **6 ms** after
  process start.
- **runit pacing.** As seen with the retired Harness, `runsv` restarts a
  service immediately unless it had started less than a second before it
  exited. Stopping Lever within a second of its start gave a median of 998 ms
  (997–999 ms) across four runs, almost all of it runit's 1-second pace.

## Comparison with the retired Pi Durable Harness

For reference against the historical
[Harness footprint record](harness-footprint-2026-11-22.md) (Node.js + Pi
Durable):

| | Retired Harness (Nov 22) | Lever (Nov 25) |
| --- | --- | --- |
| Disk footprint | 145 MiB | **2.7 MiB** |
| Idle `Rss` | 61.0 MiB (`Pss_Anon` 16.4 MiB) | **4.27 MiB** (`Pss_Anon` 0.25 MiB) |
| Warm start to answered | 63 ms | **9 ms** |
| Cold start (cache dropped) | 125 ms | **20 ms** |

## How it was measured

[`footprint.py`](evidence/lever-footprint-2026-11-25/footprint.py) ran in the
guest as root, [`measurements.txt`](evidence/lever-footprint-2026-11-25/measurements.txt)
holds its output, and [`lever.log`](evidence/lever-footprint-2026-11-25/lever.log)
holds Lever's private log from that boot:

```sh
base/void_base.py boot
base/void_base.py ssh 'cat > /root/footprint.py' < docs/research/evidence/lever-footprint-2026-11-25/footprint.py
base/void_base.py ssh 'python3 /root/footprint.py memory'
base/void_base.py ssh 'python3 /root/footprint.py start'
base/void_base.py stop
```

- `memory` reads `Rss`, `Pss`, `Pss_Anon`, and `Pss_File` from
  `/proc/<pid>/smaps_rollup` and process age from `/proc/<pid>/stat` and
  `/proc/uptime`.
- `start` stops the service (`sv down`), waits for both process and socket to
  disappear, and times `sv up` (or `kill -9`) until a forked child running as
  `crowbar-shell` connects to `/run/crowbar-lever/surface`, completes the
  version-1 handshake, and receives a `status` reply reporting
  `harness_started: false`. For cold starts, it runs `sync` and writes `3` to
  `/proc/sys/vm/drop_caches` before `sv up`.

## Not measured

- **OpenCode's memory when working.** Base does not bundle OpenCode yet; the
  slice that bundles OpenCode (#375) adds that measurement.
- **Real hardware.** Figures are from the VM on a fast host CPU and cached
  virtio disk.
