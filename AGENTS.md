# CrowbarOS

CrowbarOS is a small operating system on Void Linux with an agent built into
it, always at the person's side. Two people build it, Jim Martin and Jason
Alexander, each working with coding agents. This file is the standard both
sides hold.

## Ground rules

- The [vision](docs/VISION.md) is the plan of record. Read it before work that
  changes what CrowbarOS does. The [README](README.md) has the repository map.
- Read the [lessons](docs/LESSONS.md) before you design a part the earlier
  plans already explored.
- Write with the terms in the [glossary](GLOSSARY.md). Call the project
  CrowbarOS and the earlier plans "the Fedora blueprint" and "the substrate
  vision".
- [`docs/archive/`](docs/archive/) is history. Where it disagrees with the
  vision, the vision is right.
- A decision that is hard to reverse gets a record in [`docs/adr/`](docs/adr/),
  with the next number.
- Dated validation records and evidence logs stay as written. New results get a
  new record.

## Branches and pull requests

- `main` changes only through a pull request from a short-lived branch.
- Commit locally as you go. Push, open a pull request, or merge only when the
  person you work for asks.
- One pull request does one thing, with everything that thing needs: code,
  tests, and docs.
- **Squash merge.** The pull request title becomes the one commit on `main`, so
  write it as a conventional commit that states the outcome in plain language:
  `fix(base): a build that stopped partway no longer leaves a root the next build trusts`.
  The scope is the area: `base`, `compositor`, `shell`, `agent`, `apps`,
  `studio`, `lab`, `design`, `vision`, `adr`, `ci`.
- Fill in the [pull request template](.github/pull_request_template.md). Under
  Verification, say what you ran and what you saw, and name what you could not
  run. End with the model and harness that did the work.
- Merge when the `CI passed` check is green, then delete the branch. A branch
  need not be up to date with `main`, and nobody runs a check again because
  the other person merged ([ADR 0033](docs/adr/0033-crowbaros-is-public-and-base-boots-in-ci.md)).
- CI runs again on `main` after every merge, and catches two changes that pass
  alone but fail together. When `main` fails, fixing it comes before other
  work: the person whose merge came last opens a pull request with the fix and
  a test.

## Checks

- CI is [`.github/workflows/ci.yml`](.github/workflows/ci.yml). It runs on every
  pull request and every push to `main`. Its `changes` job decides which jobs
  a change needs, and `CI passed` passes when they all do. `CI passed` is the
  one check `main`'s ruleset requires. A change to docs alone runs only those
  two. Run the jobs' commands before you push code.
- CI boots Base. Its `Base boots` job runs `base/void_base.py check` on
  GitHub's runner, with KVM, for any change to `base/`, `compositor/`,
  `apps/shell/`, or `apps/studio-settings/`. Its logs are the run's
  `base-boot-logs` artifact. Run the check yourself when you want to see a boot
  before you push; you need not report it. GitHub does not support a VM inside
  its runners, so if the job stops working, run the check yourself, show its
  `.lab/base/boot-<time>.log`, and open an issue.
- A bug fix comes with a test that fails without the fix.
- CI cannot draw a window or run the graphical guest, so these checks are
  yours to run and report in the pull request:

| Change touches | Run | Show |
| --- | --- | --- |
| The graphical guest in `lab/void/` | `lab/void/compositor_guest.py check` | The evidence log it writes |
| The desktop: how `base/` builds or runs the compositor, the shell, or the apps, or the desktop check in `lab/void/` | `lab/void/compositor_guest.py desktop-check` | The run's `desktop.log` and `results.json` |
| How the compositor serves clients: [`compositor/`](compositor/README.md) | `lab/void/compositor_guest.py check` | The evidence log it writes |
| `apps/` | The app's Void build and run steps in its README | Before and after captures; a short recording for motion |

- When `main` moves while your check runs, rebase, but run the check again only
  if what landed touches a path that check covers. The compositor check covers
  `lab/void/` and `compositor/`. The desktop check covers `base/`, `lab/void/`,
  `compositor/`, and `apps/`. To see what landed, give the `main` commit your
  branch sat on when you ran the check:
  `git diff --stat <that commit>..origin/main -- <the paths>`. If it prints
  nothing, the evidence stands. Say in the pull request which `main` commit it
  ran on.
- After you merge a change that needed one of these checks, run that check
  once on `main`. Nothing waits on it. If it fails, fix `main` first, as above.

## The repository is public

- Anyone can read the code, its whole history, the issues, the pull requests,
  and the CI logs. Never put a secret, a key, an address on our networks, or
  anything about our homes or machines in a commit, an issue, a pull request,
  or a test's output. Each commit carries its author's Git email.
- A pull request from anyone outside the two of us runs CI only after one of
  us approves it. Read its changes first, and `.github/workflows/` most
  closely.
- CI runs only on GitHub's runners. Never connect a self-hosted runner: it
  would run strangers' code on our machines.

## Releases

Nothing is released or deployed yet, so there is no CD. Record the release
process in an ADR before the first installable image is published.

## Agent skills

### Issue tracker

Issues live in GitHub Issues on `crowbaros/crowbaros`, through the `gh` CLI. See
`docs/agents/issue-tracker.md`.

### Triage labels

The five default roles, each label named after its role: `needs-triage`,
`needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See
`docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the repo root. See
`docs/agents/domain.md`.
