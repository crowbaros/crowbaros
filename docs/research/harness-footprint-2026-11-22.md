# The Harness's footprint

**Result (November 22, 2026): under runit in the VM, the idle Harness holds
61 MiB of memory and answers on its surface socket 70 ms after its service
starts, or 143 ms with nothing in the page cache. Both are well inside the
limits [ADR 0034](../adr/0034-pi-durable-runs-the-agent-behind-a-protocol-crowbaros-owns.md)
set, so the Harness choice stands and no issue reopens it.**

This is the record
[issue 293](https://github.com/crowbaros/crowbaros/issues/293) asks for, from
the first slice of [Milestone 3](https://github.com/crowbaros/crowbaros/issues/291).
ADR 0034 chose Pi Durable with no prototype, and said the choice is reopened
if the idle Harness holds more than 150 MiB, takes more than 2 seconds from
start to answering on its socket, or loses a parked ask when it is killed and
restarted. This record measures the first two. A coding agent built the
image, booted it, and ran the measurements over SSH. The evidence is in
[`evidence/harness-footprint-2026-11-22/`](evidence/harness-footprint-2026-11-22/).

## The VM and the image

| | |
| --- | --- |
| VM | QEMU 11.1.3 with KVM, as `base/void_base.py boot` starts it: 4 CPUs, 4096 MiB, a virtio disk, UEFI. The host's CPU is passed through |
| Commit | `280adc45`, on `main` at `2f83a365` |
| Image | Base, built with `base/void_base.py build --lab-account`. `base/void_base.py check` passed 26 of 26 on the same build |
| Kernel | `6.18.64_1` |
| Node.js | v26.13.1, Node's own Linux x64 binary, at `/usr/local/lib/crowbar-agent/node` |
| Pi Durable | 1.6.0 |
| The Harness | `crowbar-agent`, as runit starts it at boot: as the person, with an empty database and nothing connected. It holds no conversation and reaches no Model yet |

## Results

| | Measured | ADR 0034's limit |
| --- | --- | --- |
| Idle memory (`Rss`) | **60.9 MiB**, the same at 15, 76, 136, and 316 seconds after it started | 150 MiB |
| From `sv up` to the shell's account being answered the version | **70 ms** median of 10, from 66 to 76 | 2 seconds |
| The same, with the page cache dropped first | **143 ms** median of 5, from 132 to 174 | 2 seconds |
| From `kill -9` to the next Harness answering | **71 ms** median of 5, from 66 to 78 | |

- **Memory.** Of the 60.9 MiB, 16.3 MiB is the process's own (`Pss_Anon`)
  and the rest is Node.js's binary and what else it maps from files, which
  the kernel can drop and read again. `Pss` is 57.9 MiB. After 25 more
  starts, the Harness then running held 61.4 MiB a minute into its life.
- **Start time** is from the moment `sv up` is run until a peer that is the
  shell's account, asking every millisecond, gets `{"version":1}` back on
  `/run/crowbar-agent/surface`. It covers runit, the service's script, `chpst`,
  `crowbar-launcher as-app`, Node.js starting, Pi Durable opening its
  database, and the answer. The Harness's own log puts its share at 53 to 61
  ms: `listening ... 56 ms after the process started`.
- **At boot** the Harness logged `listening` 415 ms after its process
  started, the slowest of all its starts, with every other service starting
  beside it.

## What else it showed

- **runit starts the Harness again at once, not a second later.** The spec
  says runit restarts it a second after any exit. What `runsv` does is start
  a service again as soon as it exits, unless that service had started less
  than a second before: then it waits a second. So a Harness that was killed
  answered again in 71 ms, and one stopped within a second of its start took
  1064 ms to answer after `sv up`, a second of which is runit's wait. A
  Harness that cannot start is tried once a second. Either way the 2 second
  limit holds.
- **The Harness asks the kernel who its peer is through `node:ffi`.** Node.js
  has no call for `SO_PEERCRED`. Node.js 26 has `node:ffi`, which it marks
  experimental, and the Harness calls the C library's `getsockopt` with it.
  It is loaded in every figure above. Base's boot check found root and the
  person refused on the socket and the shell's account answered
  (`boot-checks.txt`).
- **Node.js's own binary is larger on disk than ADR 0034 says.** The ADR's
  consequences give "one Node.js binary, about 55 MiB".
  `/usr/local/lib/crowbar-agent` is 145 MiB, nearly all of it `node`: Node's
  own binary carries its own ICU and OpenSSL, where Void's `node`, the 55 MiB
  one the research for
  [issue 231](https://github.com/crowbaros/crowbaros/issues/231) weighed,
  links the system's. The bundled Harness is 0.8 MiB. The ADR sets no limit
  on disk, so this reopens nothing, but its figure is wrong by 90 MiB.

## How it was measured

[`footprint.py`](evidence/harness-footprint-2026-11-22/footprint.py) ran in
the guest as root, and
[`measurements.txt`](evidence/harness-footprint-2026-11-22/measurements.txt)
holds what it printed.
[`harness.log`](evidence/harness-footprint-2026-11-22/harness.log) is the
Harness's own log of that boot.

```sh
base/void_base.py check    # build, boot, and check; then, for the measurements:
base/void_base.py boot
base/void_base.py ssh 'cat > footprint.py' < docs/research/evidence/harness-footprint-2026-11-22/footprint.py
base/void_base.py ssh python3 footprint.py memory
base/void_base.py ssh python3 footprint.py start
```

- `memory` reads `Rss` and `Pss` from the service's process's
  `/proc/<pid>/smaps_rollup`.
- `start` stops the service, waits for its process and its socket to be
  gone, and times `sv up` against a child process that has become the
  shell's account and connects every millisecond until it is answered. The
  child starts asking once `sv up` or the kill is done, so a Harness that
  was killed cannot answer for the next. Each Harness runs for a second and
  a half before the next stop or kill, so runit's wait is not in the
  figures, but for the last series, which shows it. For the cold figures it
  runs `sync` and writes 3 to `/proc/sys/vm/drop_caches` before each start.

## Not measured

- **A parked ask surviving a kill**, ADR 0034's third limit. The Harness
  cannot raise an ask yet. The slice that adds asks measures it.
- **Memory with a conversation going**, or with Pi Durable's image worker
  loaded. This is the idle Harness alone, as the limit is written.
- **Real hardware.** The figures are from the VM, on a fast host CPU. A slow
  disk would add to the cold start, which reads the 143 MiB binary.
