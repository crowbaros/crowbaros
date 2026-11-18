# The compositor is written from the ground up, with Smithay as its reference

**Status:** accepted on November 17, 2026, by Jason Alexander. Jim Martin had not reviewed it when it was accepted.

[ADR 0015](0015-our-own-compositor-on-smithay.md) made the compositor a CrowbarOS program written in Rust on the Smithay library. [ADR 0020](../archive/substrate-vision/adr/0020-the-first-compositor-is-small-on-purpose.md#the-smithay-pin) set rules for bumping the Smithay commit it pins. [ADR 0027](0027-the-compositor-is-built-in-its-own-repository.md) moved the code to its own repository, Fulcrum. This record changes what Fulcrum is built on. Everything else in those records stands: who the clients are, what each is offered, what the compositor draws, the attacks its tests must refuse, and that nothing is copied from a GPL-3.0 compositor.

## The decision

- **Fulcrum writes every layer of compositor code itself.** That covers the Wayland wire protocol and its code generation from the protocol XML, the event loop, the protocol state, shared memory, KMS modesetting through the kernel's ioctls, the seatd client, input handling, the renderers, damage tracking, and window management.
- **No crate from the Smithay organization.** That excludes `smithay`, the `wayland-rs` crates, `drm`, `gbm`, `input`, `calloop`, and `libseat`. A test in Fulcrum fails if one appears in its lockfile.
- **The only C libraries are the ones a GPU desktop cannot avoid.** These are Mesa's EGL, GLES, and GBM, libinput, and libxkbcommon, through Fulcrum's own bindings. Adding another needs a record here.
- **Smithay is the reference.** Fulcrum studies how Smithay solves each problem, and may adapt its MIT code with attribution. It reads the compositors built on Smithay for ideas, as ADR 0015 already does.
- **The Smithay-based compositor is the specification until the new one passes.** CrowbarOS stays pinned to Fulcrum `d11400f`, tagged `smithay-baseline` there. The pin moves to the new compositor only when it passes the baseline's 57 tests and the 36 checks of the Void guest.
- **ADR 0020's rules for the Smithay pin end** when the pin leaves `d11400f`. Until then the baseline is not bumped: work goes into the new compositor.

## Considered Options

- **Stay on Smithay, as ADR 0015 decided.** It gives a working desktop sooner, and the baseline already passes its checks. Rejected: CrowbarOS wants to own every layer of its most trusted program. A framework's dispatch would decide how each message enters, and its release cadence would decide the work.
- **Drop only the `smithay` crate, and keep the low-level crates of its organization.** Rejected for the same reason: the wire protocol, event loop, and device access would still be someone else's code inside the trusted part.
- **Build on libwayland, wlroots, or another C library.** Rejected: ADR 0015 already turned down a C base, and the reasons stand.

## Consequences

- Fulcrum's first two milestones rebuild what Smithay gave the baseline before any new behavior lands. The trusted shell session (issue 71) and the slices after it move later.
- Fulcrum owns the wire layer, so it can cap objects, descriptors, and shared-memory bytes for each client and each compartment. [Issue 174](https://github.com/crowbaros/crowbaros/issues/174) is solved there, where messages enter.
- The new code must earn the trust Smithay's maturity gave. The codec gets property tests, `unsafe` code is kept to one crate of bindings, malformed input closes a connection and never panics, and every protocol is checked against real libwayland clients in the Void guest.
- The recurring Smithay bump issue closes when the pin leaves `d11400f`.
- ADR 0015's title and its sentence "With Smithay a protocol exists only if our compositor creates it" now hold for Fulcrum's own registry. ADR 0015 and ADR 0020's section on the pin point here.
