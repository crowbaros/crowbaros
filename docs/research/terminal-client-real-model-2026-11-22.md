# The person talks to the Agent from foot, against a real Model

**Result (November 22, 2026): on real hardware, in Base built from the branch
for [issue 325](https://github.com/crowbaros/crowbaros/issues/325), a person
opened foot, signed in to a Model with `crowbar-agent login`, and asked the
Agent one question. The client opened Chromium for the sign-in, the Harness
kept the credential, and a real Model answered in 3 seconds.**

This is the record issue 325 asks for: a real turn from foot in Base, against
a real Model signed in with `crowbar-agent login`. Until now the only Model
the Harness and `crowbar-agent` had met was the stub Model of their tests.
Jim Martin unlocked the machine, signed in with his own account, and typed
the request, at the machine's keyboard. A coding agent built the image, wrote
it to the machine's disk, and read the logs and the client's output over SSH.
The evidence is in
[`evidence/terminal-client-real-model-2026-11-22/`](evidence/terminal-client-real-model-2026-11-22/).

## The machine and the image

| | |
| --- | --- |
| Machine | Ryzen 5 7500X3D, 16 GB, UEFI, booted from a USB SSD |
| GPUs | The Ryzen's own Radeon, and a GeForce RTX 5060 with NVIDIA's driver 595.112.07 loaded |
| Monitor | One, on the Radeon's HDMI-A-1, 3840x2160 at 59.982 Hz. Fulcrum chose `renderer=gles` on the Radeon |
| Commit | `544db32e`, on `main` at `1896df4e` |
| Image | Base, built with `base/void_base.py build` and an account file. `lab/void/desktop_check.py` passed 113 of 113 on the same commit, in the VM |
| Kernel | `6.18.64_1` |
| Terminal | `foot-1.28.0_1` |
| Browser | `chromium-158.0.8127.36_1`, the default browser. `xdg-open` (`xdg-utils-1.2.1_1`) and `gio` are both on `PATH` |
| The Harness | As runit starts it at boot: Pi Durable 1.6.0, Node.js v26.13.1 |
| The client | `crowbar-agent 0.1.0`, at `/usr/local/bin/crowbar-agent` |

## What was done

1. The machine booted Base. The Harness logged that it listened on the person
   socket, in the person's runtime directory, 223 ms after its process
   started, before anyone had unlocked the desktop.
2. Jim unlocked the desktop and opened foot.
3. He ran `crowbar-agent login`, picked OpenAI Codex from the providers the
   Harness listed, and signed in by OAuth. Chromium opened by itself with the
   provider's page, and he finished the sign-in there.
4. He ran `crowbar-agent "in one sentence, what is Void Linux?"`, and the
   answer came on the terminal.

## Results

| | |
| --- | --- |
| `crowbar-agent login` ran from foot | The client's parent was `bash`, and its parent `foot`, all three as the person ([`process-tree.txt`](evidence/terminal-client-real-model-2026-11-22/process-tree.txt)) |
| The sign-in | By OAuth, in the Chromium the client opened. The Harness logged `signed in to openai-codex` 3 minutes 26 seconds after the client connected, which is the time the person took in the browser |
| The Model the Agent uses | GPT-6.2 Sol (`gpt-6.2-sol`), from OpenAI Codex, as `crowbar-agent status` has it |
| The turn | Turn 1 of a new conversation began and ended `done` **2.96 seconds** later, by the Harness's log |
| The answer | "Void Linux is an independent, rolling-release Linux distribution that uses the XBPS package manager and runit instead of systemd." |
| The conversation afterwards | `--list` shows it, with its one turn and its title, and `--resume` shows the message and the answer. Both still do after the Harness was restarted twice |
| The credential | In `credentials.json` in the Harness's state, mode 0600, the person's, in a directory of mode 0700. The database, the log, and the settings are 0600 too |
| The person socket | Mode 0600, the person's, in a directory of mode 0700 in their runtime directory |
| The log | 13 lines for all of it: the connections, the sign-in by the provider's name, the conversation and the turn by their IDs. Nothing either side said, and nothing of the credential |

[`client.txt`](evidence/terminal-client-real-model-2026-11-22/client.txt)
holds what the client printed and the modes, and
[`harness.log`](evidence/terminal-client-real-model-2026-11-22/harness.log)
is the Harness's own log of the boot.

## What else it showed

- **The Harness holds more memory than the footprint record measured.** The
  [footprint record](harness-footprint-2026-11-22.md) found 61 MiB idle, on
  `main` before this branch, when the Harness reached no Model. With this
  branch it holds 102 MiB idle, signed in and with nothing connected since it
  started, and 124 MiB after the sign-in and the turn
  ([`memory.txt`](evidence/terminal-client-real-model-2026-11-22/memory.txt)).
  The desktop check's VM, with no Model signed in, has it at 100 MiB. So the
  40 MiB came with the branch, not with the sign-in. What the branch loads
  when the Harness starts is pi-ai's providers and their catalogs of Models,
  which is the likely cause; it was not measured apart.
  [ADR 0034](../adr/0034-pi-durable-runs-the-agent-behind-a-protocol-crowbaros-owns.md)'s
  limit is 150 MiB for the idle Harness, and Base's boot check holds every
  build to it. It holds here, with about half the room the footprint record
  had: 48 MiB, where that record had 89.
- **`xdg-open` is in Base, and the client used it.** The client tries
  `xdg-open` and then `gio open`. Both are in the image, so the first one
  opened Chromium.
- **The desktop was drawn by the Radeon.** The monitor was on the
  motherboard's HDMI output this time, so Fulcrum rendered on the Ryzen's
  Radeon with Mesa, and NVIDIA's module was loaded and drove no display.
  Nothing here depends on which GPU draws.

## How it was gathered

A script on another machine asked the machine under test, over SSH every
four seconds, for each `crowbar-agent` process and its parents, until the
Harness's log showed a turn had ended. It caught the `login`, which ran for
minutes. The one-shot turn ran for three seconds and fell between two looks,
so the tree of that process is not in the evidence; it was typed in the same
foot.

The rest was read afterwards, as root over SSH:

```sh
cat ~person/.local/state/crowbaros/agent/harness.log
su -s /bin/sh person -c 'XDG_RUNTIME_DIR=/run/user/1003 crowbar-agent status'
su -s /bin/sh person -c 'XDG_RUNTIME_DIR=/run/user/1003 crowbar-agent --list'
su -s /bin/sh person -c 'XDG_RUNTIME_DIR=/run/user/1003 crowbar-agent --resume wfxzy8vd </dev/null'
grep -E '^(Rss|Pss|Pss_Anon):' /proc/<the Harness's pid>/smaps_rollup
```

Those clients are the connections in the log after 03:19:29. The times in
the log are the machine's own clock, in UTC. The two restarts in it are
`sv restart crowbar-agent`, for the memory figures.
[`machine.txt`](evidence/terminal-client-real-model-2026-11-22/machine.txt)
holds the kernel, the packages, the GPUs, and Fulcrum's lines for the renderer
and the output.

## Not shown

- **A capture of the screen.** The record rests on the logs, the process
  tree, and what the client printed.
- **A conversation a line at a time, Ctrl-C, or a second client**, against a
  real Model. One request, one answer. The tests cover the rest against the
  stub Model.
- **Signing in with an API key**, and any provider but this one.
- **A real Model that fails**: a refused key, a rate limit, a dropped
  connection, an answer cut short. Whether a provider's own errors read well
  on the terminal is not known yet.
- **A sign-in with no browser**: on a console or over SSH, with the address
  opened on another machine and the code pasted back.
- **The Agent's surface.** There is none yet. The drawer is
  [issue 295](https://github.com/crowbaros/crowbaros/issues/295), and
  [issue 309](https://github.com/crowbaros/crowbaros/issues/309) asks for the
  record of the whole of Milestone 3 against a real Model.
