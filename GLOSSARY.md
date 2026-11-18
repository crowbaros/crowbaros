# CrowbarOS glossary

The words this project uses, and the ones it avoids. When a term is missing or
wrong, change it here first. The substrate vision's glossary is
[archived](docs/archive/substrate-vision/GLOSSARY.md).

## The system

**CrowbarOS**:
The operating system: Base, the compositor, the shell, the default apps, and the Agent.
_Avoid_: "agent-first OS"

**Base**:
The Void Linux system underneath everything else: the kernel, runit, xbps, and the packages CrowbarOS installs.
_Avoid_: Distro, host

**Compositor**:
The CrowbarOS Wayland compositor. It draws every surface, delivers all input, and owns the summon gesture.
_Avoid_: Window manager

**Fulcrum**:
The repository where the compositor is built. Base builds one commit of it.

**Shell**:
The bar, launcher, dock, and wallpaper the person works in, drawn as layer surfaces.
_Avoid_: Desktop environment

**Default apps**:
The handful of apps CrowbarOS installs. Studio Settings is the first.

## The Agent

**Agent**:
The helper built into CrowbarOS, at the person's side. It runs from boot and acts with the person's permissions.
_Avoid_: Assistant, copilot, bot

**Harness**:
The program that runs the Agent: it holds the conversation, calls a model, and carries out the Agent's acts on the OS.

**Model**:
The language model a harness calls. It is not part of what CrowbarOS trusts.

**Summon**:
Bringing the Agent up with one gesture, from anywhere. The same gesture puts it away.
_Avoid_: Launch, open

**Person**:
The one who uses the machine and is in charge of it.
_Avoid_: User, owner

## History

**The Fedora blueprint**:
The first plan: a curated desktop on Fedora with five default apps and no AI.

**The substrate vision**:
The second plan: a lean base each person shaped into their own OS with Tools their agent built, under a deterministic security model. It ended at the `substrate-final` tag.
