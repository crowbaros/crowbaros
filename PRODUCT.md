# CrowbarOS

<!-- impeccable:product-schema 1 -->

## Platform

Native Linux desktop, Rust/GPUI on Wayland. The current Impeccable platform enum
does not represent Linux; this is neither a web app nor an iOS/Android app.

## Users and purpose

CrowbarOS is an operating system that each person shapes into their own OS with
their agent. It ships a small base, not a finished desktop. A person says or
points at what they want, the agent builds it as a Tool in a private copy, and
the person previews and confirms it. The first users are curious, technically
inclined people, one person per machine. The [vision](docs/archive/substrate-vision/VISION.md) is the
plan of record.

The interfaces CrowbarOS owns are the shell and the trusted system surfaces. The
shell draws every Tool's interface inside system chrome that a Tool cannot
imitate. Studio Settings is the first native surface built with the design
system, and the reference for its components.

## Stack and operating context

Foundations from the vision: a Void Linux Base, Rust for owned code, Wayland and
portals, and GPUI as the candidate toolkit for the shell that draws Tool
interfaces. The compositor is our own, built on Smithay
([ADR 0015](docs/adr/0015-our-own-compositor-on-smithay.md)). The format of a Tool's
interface description is an open question: a new vocabulary drawn by GPUI, or a constrained web
subset drawn by a trusted engine.

The GPUI apps run in the [Void Alpha desktop lab](lab/void/README.md), with
separate compositor, shell, and app users under the temporary rules of ADR 0016.
The integrated smoke command uses a fresh guest and preserves development VMs.
Fedora workflow tooling is archived; use the [Void guide](docs/development/local-vms.md).
Hardware graphics evidence and broader device qualification are recorded separately.

## Brand commitments

Preserve the approved CrowbarOS identity, Nullkeel palette, Rookslash crow,
Manrope interface typography, IBM Plex Mono for technical text, and the existing
Studio Settings visual studies. The brand kit owns those decisions. Do not
reopen the identity for an implementation slice.

## Capabilities and constraints

Built: the Studio Settings slice (Appearance plus reduced motion, with real
preferences and truthful apply and error states) and the first shell slice
(workspace bar, dock, launcher, wallpaper). Third-party theme integration,
accessibility, and physical-device coverage remain separate acceptance work. A
neutral Studio interface is not display calibration.

Constraints from the vision that every interface must meet:

- **Trusted surfaces cannot be themed, rebound, or imitated.** Asks, the confirm
  gesture, previews, and the powerbox are part of Base. Themes apply to Tool
  surfaces. System chrome must look unlike anything a Tool can draw.
- **A Tool describes its interface, and the shell draws it.** The design system's
  components are the parts every Tool is made from, so every Tool is themed and
  accessible.
- **Only the shell delivers input.** An ask is answered with the confirm gesture.
  Voice never counts as confirmation.
- **An ask shows the exact content in fixed words.** People stop reading asks when
  there are too many, so each one must be short, specific, and rare.
- **A visible indicator shows whenever the OS is listening or watching.**

Not designed yet: trusted asks and the confirm gesture, preview and promote, the
outbox, the powerbox, point and say, the task view and stop switch, and the
recovery screen.

## Evidence on hand

- `docs/VISION.md`, `docs/adr/`, `GLOSSARY.md`: product decisions and terms.
- `docs/architecture/capability-model.md`: declarative UI, point and say, the
  powerbox, and what a "yes" requires.
- `docs/branding/crowbaros-brand-kit/`: approved assets, tokens and visual studies.
- `apps/shell/`, `apps/studio-settings/`: the implemented native surfaces.
- `apps/gpui-probe/`: pinned GPUI 0.2.2 API and validated rendering/input baseline.
- `docs/development/`: lab workflow, validation records, and Intel hardware Vulkan evidence.
- `docs/archive/fedora-blueprint/`: the superseded Fedora plan. Not current.

## Product principles

- Make settings discoverable with familiar controls and clear scope.
- Preview safely, save deliberately, and preserve user choices.
- Keep native interfaces unprivileged and system integration separate.
- Report tested behavior and remaining gaps accurately.
- Let the person see what will happen before it happens, and undo it after.
- Never let a Tool's surface pass for the system's.
