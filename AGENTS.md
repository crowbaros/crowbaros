# CrowbarOS

CrowbarOS is an operating system that each person shapes with their agent, on a
small Void Linux base. Two people build it, Jim Martin and Jason Alexander, each
working with coding agents. This file is the standard both sides hold.

## Ground rules

- The [vision](docs/VISION.md) is the plan of record. Read it before work that
  changes what CrowbarOS does. The [README](README.md) has the repository map.
- Write with the terms in the [glossary](GLOSSARY.md). Call the project
  CrowbarOS and the earlier plan "the Fedora blueprint".
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
  `fix(apply): a failed apply no longer leaves a half-built generation`.
  The scope is the area: `state`, `apply`, `shell`, `compositor`, `studio`,
  `labels`, `broker`, `authority`, `sandbox`, `lab`, `design`, `vision`, `adr`,
  `ci`.
- Fill in the [pull request template](.github/pull_request_template.md). Under
  Verification, say what you ran and what you saw, and name what you could not
  run. End with the model and harness that did the work.
- Merge when CI is green, then delete the branch. A change to docs alone
  starts no CI run, so it merges without one.

## Checks

- CI is [`.github/workflows/ci.yml`](.github/workflows/ci.yml). It runs on every
  pull request and on `main`, except a change to docs alone: its `paths-ignore`
  lists name what counts as docs. Run its commands before you push code.
- A bug fix comes with a test that fails without the fix.
- CI cannot boot a VM or draw a window, so these checks are yours to run and
  report in the pull request:

| Change touches | Run | Show |
| --- | --- | --- |
| How Base is built, booted, or rolled back; `lab/void/` | `lab/void/void_lab.py milestone` | The evidence log it writes |
| `compositor/fulcrum.json`, or a Fulcrum change before it is pinned | `lab/void/compositor_guest.py check`, as [`compositor/`](compositor/README.md) says | The evidence log it writes |
| `apps/` | The app's Void build and run steps in its README | Before and after captures; a short recording for motion |

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
