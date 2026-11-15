# Compositor

The CrowbarOS Wayland compositor, written in Rust on
[Smithay](https://github.com/Smithay/smithay). It is part of Base and part of
what we trust. The decision and its reasons are in
[ADR 0015](../docs/adr/0015-our-own-compositor-on-smithay.md), and the evidence
is in the [compositor research](../docs/research/compositor-smithay.md).

**Not started.** The first work is the
[spike](https://github.com/crowbaros/crowbaros/issues/3), which tests the
decision. Spike code is throwaway and does not have to land here.

What the compositor does that a borrowed one would not
([ADR 0014](../docs/adr/0014-a-yes-comes-from-the-person.md)):

- It delivers input only as the shell directs. It offers no Tool or app a
  virtual keyboard, virtual pointer, input-method, or input-emulation interface.
- It takes the confirm gesture itself and forwards it to no client.
- It decides what is drawn above what, so system chrome cannot be covered or
  imitated.
- It gates screen capture and the clipboard per principal.

## Layout

This will be its own Cargo project with a pinned `Cargo.lock`, like the apps in
[`apps/`](../apps/README.md). It is not a member of the root workspace, because
it pins Smithay to a git commit and needs system libraries (DRM, libinput,
libseat, udev) that CI does not install. It can depend on crates in
[`crates/`](../crates/README.md) by path.

Do not copy code from niri or cosmic-comp. Both are GPL-3.0, and CrowbarOS is
MIT. Smithay and its example compositors are MIT.
