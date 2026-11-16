# The shell is offered what a compartment with the GPU is offered, and layer shell

**Status:** accepted on November 15, 2026, by Jim Martin, when slice 1 of the compositor ([issue 49](https://github.com/crowbaros/crowbaros/issues/49)) was built.

The table "What each client is offered" in [ADR 0020](0020-the-first-compositor-is-small-on-purpose.md) names the shell once: layer shell is offered to "the shell alone". It does not say what else the shell is offered. Read as written, the shell could not create a surface, because the core protocols are in the row for compartments, and the shell is not a compartment ([ADR 0016](0016-the-warden-starts-every-compartment.md)). This record fills that gap. Everything else in ADR 0020 stands.

## The decision

- **The shell is offered three rows of the table:** the row for every compartment with a Wayland listener, the row for a compartment whose needs include the GPU, and layer shell.
- **The "Nobody" row holds for the shell too.** The shell reads pixels and learns of windows through its own interface to the compositor, and not through a Wayland protocol.
- **The Warden is offered the security context manager and nothing else.** It draws nothing.

| Offered to | Rows of ADR 0020's table |
| --- | --- |
| The shell | Every compartment with a Wayland listener; a compartment whose needs include the GPU; the shell alone |
| The Warden | The Warden alone |

## Considered Options

- **Layer shell and nothing else, as the table reads.** Rejected: the shell could not draw.
- **The compartment row and layer shell, without the GPU row.** Rejected: the shell draws all of the trusted UI except frames, and it draws with GPUI, which renders on the GPU. The shell is a trusted part, so the GPU is not a need the person is asked about.
- **A row of the shell's own that lists each protocol.** Rejected: it is a second list to keep the same as the first, and a difference between them would be an accident.

## Consequences

- A protocol that is added to the compartment row or the GPU row is offered to the shell as well. Whoever adds one checks that the shell should have it.
- ADR 0020's table points here.
- `compositor/src/policy.rs` holds this, and its tests fail if the shell loses a row or the Warden gains one.
