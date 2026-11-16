# The production compositor is built beside Milestone 2, and the parts Milestone 2 can change may be reworked

**Status:** accepted on November 15, 2026, by Jim Martin, with Jason Alexander's agreement.

[ADR 0020](0020-the-first-compositor-is-small-on-purpose.md) says the production compositor starts after the security rules are proven ([issue 17](https://github.com/crowbaros/crowbaros/issues/17)), and it rejected an earlier start. It gave two reasons: Milestone 2 can still change what the compositor must enforce, and two people cannot do both at once. This record changes when the compositor starts. Everything else in ADR 0020 stands.

## What changed

- **Both can be done at once.** Four issues of the security foundation were merged on the day ADR 0020 was accepted: [56](https://github.com/crowbaros/crowbaros/issues/56), [67](https://github.com/crowbaros/crowbaros/issues/67), [68](https://github.com/crowbaros/crowbaros/issues/68), and [75](https://github.com/crowbaros/crowbaros/issues/75). Each of us works with coding agents. The compositor is a Cargo project of its own, outside the workspace that Milestone 2 changes, so the two do not change the same files.
- **The wait holds up the design work.** Seven issues are still open before issue 17 closes. After them come slice 1, the shell protocol ([issue 71](https://github.com/crowbaros/crowbaros/issues/71)), and frames, windows, and workspaces ([issue 78](https://github.com/crowbaros/crowbaros/issues/78)). Jason's work on the shell, on Settings ([issue 81](https://github.com/crowbaros/crowbaros/issues/81)), and on Tool UI ([issue 80](https://github.com/crowbaros/crowbaros/issues/80)) waits on those.
- **Milestone 2 can change only some of the compositor.** The skeleton, the protocol table, frames, focus, windows, workspaces, the compositor's keys, displays, and input devices rest on ADRs 0014, 0016, and 0020, and on nothing Milestone 2 measures. The parts it can change are listed below.

## The decision

- **The compositor starts now, with slice 1 ([issue 49](https://github.com/crowbaros/crowbaros/issues/49)), and is built beside Milestone 2.** Issue 49 no longer waits for issue 17.
- **The slices keep ADR 0020's order**, and each still lands with the attacks that test it.
- **Where a slice needs a part of the broker that is not built, it uses a stand-in.** ADR 0020 already does this for the Warden in slice 1. The broker's end of an ask, of a paste, and of unlock are stand-ins too, until the broker has them.
- **Three parts are built as ADRs 0018 and 0020 say, and may be reworked:**

| Part | Slice | What can change it |
| --- | --- | --- |
| The ask: arming it, and what the chord does | 2 ([issue 71](https://github.com/crowbaros/crowbaros/issues/71)) | The livability review ([issue 69](https://github.com/crowbaros/crowbaros/issues/69)) decides whether the person can live with the asks as they are |
| What the compositor tells the broker about each paste | 4 ([issue 166](https://github.com/crowbaros/crowbaros/issues/166)) | The measurement of how far marks spread ([issue 58](https://github.com/crowbaros/crowbaros/issues/58)) |
| Lock: the chord on the lock screen, and the broker's word that the person authenticated | 6 ([issue 87](https://github.com/crowbaros/crowbaros/issues/87)) | The chord's table follows the ask, and the broker's end does not exist yet |

- **After the livability review, each of the three is checked against its result.** A change to one of them gets a record of its own, and the attacks of its slice are run again.

## Considered Options

- **Keep ADR 0020's order.** Rejected: ten issues would close before a frame or a window exists, for a reason that covers three parts.
- **Start slices 1, 3, and 5 only, and hold the ask, the clipboard, and lock until issue 17 closes.** Rejected: a frame learns what its window is through the shell protocol, and slice 2 builds that protocol together with the ask ([ADR 0018](0018-one-gesture-answers-one-ask.md)). Holding slice 2 holds slice 3.
- **Do the design work on the spike.** Rejected for new work: the spike's channel to the shell has no request IDs, no version, and no way to reconnect (ADR 0018), and its code is thrown away, so work against it is done twice. The bar, the dock, the launcher, and Settings still run on the spike, and visual work on them goes on meanwhile.

## Consequences

- Issue 49 loses its link to issue 17. Issues 71, 78, 79, 87, and 166 keep their links, which all lead back to issue 49.
- The rework of the three parts, if the review asks for it, is the cost of starting now.
- The livability review gains a question: do the ask, the paste message, or the chord's table change?
- Every pull request that touches the compositor runs the Void guest's compositor checks (ADR 0020). CI cannot boot a guest, so one of us runs them, now while Milestone 2 work goes on.
- ADR 0020's paragraph on where the work sits, and its rejected option, now point here.
