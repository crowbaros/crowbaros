# CrowbarOS is public, and Base boots in CI on every pull request and on main

**Status:** accepted on November 19, 2026, by Jim Martin and Jason Alexander. Proposed on November 19 by Jim Martin.

Two of us merge to `main` many times a day, and our pull requests keep running into each other. The checks that take longest boot a VM, and [AGENTS.md](../../AGENTS.md) has us run them on our own machines. By the time a run finishes, the other person has often merged to `main`, and the one who ran it starts over. On November 19, Jason merged #243 and #244 while #245 was in its checks, and #244 changed `base/`'s package list.

Our plan limits what GitHub can do about it. The `crowbaros` organization is on GitHub Free, and the repository is private. GitHub refuses branch protection and rulesets for it: "Upgrade to GitHub Pro or make this repository public." The merge queue needs a public repository or GitHub Enterprise Cloud; GitHub Team is not enough. Free includes $12 of Actions use a month for private repositories, about 2,000 minutes on the standard Linux runner, which a boot check on every pull request would use up. GitHub's docs say all this, as checked on November 19, 2026: the [merge queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue), [rulesets](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/about-rulesets), and [Actions billing](https://docs.github.com/en/billing/concepts/product-billing/github-actions).

[Omarchy](https://github.com/omacom/omarchy) shows another way. Its `test.yml` boots a real Omarchy install under QEMU on GitHub's standard Ubuntu runners, using the runner's `/dev/kvm`, and runs its whole test suite there in about ten minutes, on every pull request and every push to its main branch. It boots an image built and published beforehand, so no run builds the OS. It has no branch protection and no merge queue: a bad combination shows up in the run after the merge, and is fixed by a pull request that follows. We work the same way.

## The decision

- **The repository is public.** Its whole history, issues, pull requests, reviews, and CI logs can be read by anyone. The code is already under the MIT license. Before it is made public:
  - a full scan of the history for secrets, with `gitleaks`. A first search of all 774 commits for private keys, GitHub, Anthropic, and AWS tokens, and Tailscale keys and addresses found none;
  - each of us reads the issues and pull requests for anything about our homes, machines, or other private matters.
- **The boot check runs in CI**, on GitHub's runners with KVM, on every pull request and on every push to `main`. The built Base root is cached under a key made from the files in `base/` that build it, so most runs boot it rather than build it. The boot itself takes about 25 seconds. GitHub's [runner docs](https://docs.github.com/en/actions/concepts/runners/github-hosted-runners) say a VM inside a runner is "technically possible" but "not officially supported", and "experimental": Omarchy depends on it every day, but GitHub promises nothing. So this is tried on a branch before the repository is made public. If it cannot work on GitHub's runners, or later stops working, the boot check goes back to our machines under the rules in AGENTS.md, and the rest of this record stands.
- **A pull request merges once its own checks pass.** It does not wait for `main` to stop moving, and nobody runs a check again because the other person merged. The run on `main` after each merge catches two changes that pass alone but fail together. When `main` fails, fixing it comes first, with a pull request that brings a test.
- **One gate job is the only required check.** It always runs and decides inside itself which checks a change needs. A change to docs alone passes it at once. A required check cannot be filtered by path: a pull request that skips it waits on it forever.
- **A ruleset guards `main`.** Changes come only through a pull request, the gate job must pass, `main` cannot be force-pushed or deleted, and pull requests are squash-merged, as now. "Require branches to be up to date" stays off.
- **The desktop check stays on our machines.** It needs a GPU. It keeps its row in the AGENTS.md table, and the rules for when `main` moves during a check apply to it. Those rules are a separate pull request.
- **No self-hosted runner.** A public repository runs the workflows of anyone's pull request, so a runner on one of our machines would run strangers' code. By default GitHub holds the workflows of a first-time contributor for approval, but anyone who has had one change merged runs freely after that. We set "Require approval for all external contributors", so every pull request from outside the two of us waits for one of us to approve its run.

## Considered Options

- **Stay private, with only the AGENTS.md rules.** A check is run again only when what landed touches its paths, and `main` is checked after each merge. Rejected as the end state: it is free and helps at once, but every boot check is still run and reported by hand. It is kept as the rule for the desktop check.
- **Stay private, with a self-hosted runner at home.** One of our machines with KVM runs the boot check. Rejected: a machine must be kept up for CI, and a private repository on Free still has no rulesets.
- **Stay private, and pay for Actions minutes or GitHub Team.** Rejected: it costs money every month for what a public repository gets free.
- **Stay private, on GitHub Enterprise Cloud.** It has the merge queue. Rejected: it costs far more per seat than two people need.
- **Go public, and merge through GitHub's merge queue.** The queue tests each pull request on top of `main` and the pull requests ahead of it, and merges them in order, so a bad combination never lands. Rejected for now: every merge would wait for a run, and the workflows would need the `merge_group` event too. With two of us, a rare failure on `main` fixed by a pull request costs less than waiting on every merge. The queue is free on a public repository, so it is there if failures on `main` become common.

## Consequences

- Neither of us runs the boot check by hand or pastes its log into a pull request. The AGENTS.md table shrinks to the desktop check and the `apps/` captures.
- A merge waits only for the pull request's own checks.
- `main` can break for the time between a merge and the fix. The run on `main` shows it within one build of Base and a boot.
- Making the repository private again does not take it back. Anything public can be cloned, forked, and archived while it is public.
- Anyone can open an issue or a pull request. AGENTS.md and the [triage labels](../agents/triage-labels.md) already say how we handle them.
- GitHub's standard runners cost nothing for a public repository, and are larger: 4 CPUs and 16 GB of memory for a public repository against 2 CPUs and 8 GB for a private one.
- Omarchy's other two habits can follow later, and need no decision here: an AI reviewer on each pull request, and a check that asks for screenshots without blocking a merge.
