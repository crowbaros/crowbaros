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
  `lab`, `design`, `vision`, `adr`, `ci`.
- Fill in the [pull request template](.github/pull_request_template.md). Under
  Verification, say what you ran and what you saw, and name what you could not
  run. End with the model and harness that did the work.
- Merge when CI is green and the other person has approved, then delete the
  branch. GitHub does not enforce this on a private repository on our plan, so
  we hold it ourselves.

## Checks

- CI is [`.github/workflows/ci.yml`](.github/workflows/ci.yml). It runs on every
  pull request and on `main`. Run its commands before you push.
- A bug fix comes with a test that fails without the fix.
- CI cannot boot a VM or draw a window, so these checks are yours to run and
  report in the pull request:

| Change touches | Run | Show |
| --- | --- | --- |
| How Base is built, booted, or rolled back; `lab/void/` | `lab/void/void_lab.py milestone` | The evidence log it writes |
| `apps/`, `lab/fedora/` | The app's build and run steps in its README | Before and after captures; a short recording for motion |

## Releases

Nothing is released or deployed yet, so there is no CD. Record the release
process in an ADR before the first installable image is published.
