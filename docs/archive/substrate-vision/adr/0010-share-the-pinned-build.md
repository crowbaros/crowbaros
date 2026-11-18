# Sharing sends the pinned build with its intent and checks

A person shares a Tool as its pinned build together with its intent and checks. The recipient's OS runs the checks in rehearsal, shows what the Tool needs, and the recipient grants that fresh. Someone else's words are **untrusted input**: they are never handed to the recipient's Agent as instructions. A Tool from a stranger runs only as WASM. Rebuilding it locally from its intent stays possible: the build task that reads the stranger's words is marked, so the result gets no grant without the person's answer (ADR 0011). The author's description of why the Tool needs something is shown as the author's claim, next to the needs themselves, which the OS states in fixed words. Standing grants, standing rules, and secrets never transfer, and Data transfers only when explicitly included.

Security decided this. A shared intent is a prompt written by a stranger and fed to a principal that can edit intents, and a public gallery would make that injection at scale. A pinned build with a stated list of needs can be checked without a model reading anyone's words.

## Considered Options

- **Share intents; the recipient's Agent rebuilds them (the earlier decision).** Rejected as the default for the reason above. It was chosen so that no compiled code from someone else runs, but the recipient's sandbox and fresh grants already bound what shared code can do.
