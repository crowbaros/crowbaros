# Labs

Each lab runs CrowbarOS work in virtual machines or against stand-ins, so nothing
here touches the host system or a real account.

| Lab | Purpose | State |
| --- | --- | --- |
| [`void/`](void/README.md) | Base: a Void Linux VM on Btrfs, driven by the apply engine. Also the graphical guest that runs the compositor on a TTY | In use. Ran Milestone 1 and the compositor spike |
| [`adversarial/`](adversarial/README.md) | Milestone 2: fixture services, the red-team harness, attack and task runs | Not started |
| [`fedora/`](fedora/README.md) | Builds and runs the GPUI apps under Hyprland | In use for interface work only. To be removed |

Lab state lives outside git: `.lab/` for the Void lab and the compositor checks,
and `.local/` for the Fedora lab.
