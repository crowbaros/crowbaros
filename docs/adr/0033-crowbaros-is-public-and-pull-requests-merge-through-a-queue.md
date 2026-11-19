# CrowbarOS is public, and pull requests merge through a queue that boots Base

**Status:** proposed on November 19, 2026, by Jim Martin, for Jason Alexander's review.

Two of us merge to `main` many times a day, and our pull requests keep running into each other. The checks that take longest boot a VM, and [AGENTS.md](../../AGENTS.md) has us run them on our own machines. By the time a run finishes, the other person has often merged to `main`, and the one who ran it starts over. On November 19, Jason merged #243 and #244 while #245 was in its checks, and #244 changed `base/`'s package list.

GitHub can do this ordering for us, but not on our plan. The `crowbaros` organization is on GitHub Free, and the repository is private. GitHub refuses branch protection and rulesets for it: "Upgrade to GitHub Pro or make this repository public." The merge queue needs a public repository or GitHub Enterprise Cloud; GitHub Team is not enough. Free also gives a private repository 2,000 Actions minutes a month, which a boot check on every pull request would use up.

[Omarchy](https://github.com/omacom/omarchy) shows another way. Its `test.yml` boots a real Omarchy install under QEMU on GitHub's standard Ubuntu runners, which offer KVM, and runs its whole test suite there in about ten minutes, on every pull request and every push to its main branch. It boots an image built and published beforehand, so no run builds the OS. It has no branch protection and no merge queue: a bad combination shows up in the run after the merge, and is fixed by a pull request that follows.

## The decision

- **The repository is public.** Its whole history, issues, pull requests, reviews, and CI logs can be read by anyone. The code is already under the MIT license. Before it is made public:
  - a full scan of the history for secrets, with `gitleaks`. A first search of all 774 commits for private keys, GitHub, Anthropic, and AWS tokens, and Tailscale keys and addresses found none;
  - each of us reads the issues and pull requests for anything about our homes, machines, or other private matters;
  - both of us agree to it.
- **The boot check runs in CI**, on GitHub's runners with KVM, on every pull request, in the queue, and on `main`. The built Base root is cached under a key made from the files in `base/` that build it, so most runs boot it rather than build it. The boot itself takes about 25 seconds. This is tried on a branch before the repository is made public. If it cannot work on GitHub's runners, this record is not accepted as written.
- **One gate job is the only required check.** It always runs and decides inside itself which checks a change needs. A change to docs alone passes it at once. A required check cannot be filtered by path: a pull request that skips it waits on it forever.
- **A ruleset guards `main`.** Changes come only through a pull request, the gate job must pass, `main` cannot be force-pushed or deleted, and pull requests are squash-merged, as now.
- **Pull requests merge through GitHub's merge queue.** The queue tests each pull request on top of `main` and the pull requests ahead of it, and merges them in order. The workflows also run on the `merge_group` event. "Require branches to be up to date" stays off, because the queue does that work.
- **The desktop check stays on our machines.** It needs a GPU. It keeps its row in the AGENTS.md table, and the rules for when `main` moves during a check apply to it. Those rules are a separate pull request.
- **No self-hosted runner.** A public repository runs the workflows of anyone's pull request, so a runner on one of our machines would run strangers' code. A first-time contributor's workflows wait for one of us to approve them.

## Considered Options

- **Stay private, with only the AGENTS.md rules.** A check is run again only when what landed touches its paths, and `main` is checked after each merge. Rejected as the end state: it is free and helps at once, but every boot check is still run and reported by hand. It is kept as the rule for the desktop check.
- **Stay private, with a self-hosted runner at home.** One of our machines with KVM runs the boot check. Rejected: a machine must be kept up for CI, and a private repository on Free or Team still has no merge queue.
- **Stay private, and pay for Actions minutes or GitHub Team.** Rejected: Team adds rulesets but no merge queue, and paid minutes still leave the order of merges to us.
- **Stay private, on GitHub Enterprise Cloud.** It has the merge queue. Rejected: it costs far more per seat than two people need.
- **Go public without a merge queue, as Omarchy does.** The boot check runs in CI, and a bad combination is fixed after it lands. This is the fallback if the queue proves slow: it is simpler, and it works for Omarchy with many more contributors than us.

## Consequences

- Neither of us runs the boot check by hand or pastes its log into a pull request. The AGENTS.md table shrinks to the desktop check and the `apps/` captures.
- A merge waits for the queue's run, at most a build of Base and a boot. With two people the queue is rarely more than one deep.
- Making the repository private again does not take it back. Anything public can be cloned, forked, and archived while it is public.
- Anyone can open an issue or a pull request. AGENTS.md and the [triage labels](../agents/triage-labels.md) already say how we handle them.
- Actions minutes on GitHub's standard runners cost nothing for a public repository.
- Omarchy's other two habits can follow later, and need no decision here: an AI reviewer on each pull request, and a check that asks for screenshots without blocking a merge.

## Questions for Jason

- Are you willing to make the repository public, and is there anything in its history, issues, or pull requests you would want removed first?
- Do you want the merge queue, or Omarchy's way of checking after the merge?
- Does the desktop check staying on our own machines work for you, for the shell and apps you build?
