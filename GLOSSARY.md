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
The name of the CrowbarOS compositor, the way Mutter or KWin names theirs. Its code is in `compositor/`.

**Shell**:
The bar, launcher, dock, and wallpaper the person works in, drawn as layer surfaces.
_Avoid_: Desktop environment

**App**:
A program the person runs on the desktop. Every app runs as the person's account, at the compositor's one socket for apps, and may do what a program may on any desktop: share the clipboard, take the keyboard when it opens, use the GPU.
_Avoid_: Compartment, principal, Tool

**Launcher service**:
The Base service that starts the apps the shell asks for, as the person. The shell's launcher is the menu the person picks an app from; the launcher service is what starts it.

**Default apps**:
The handful of apps CrowbarOS installs. Studio Settings is the first.

**Desktop**:
The compositor, the shell, and the apps running on Base: what the person sees and works in.
_Avoid_: Alpha desktop, session

**The person's account**:
The one account on the machine that belongs to the person. It has their home, its password unlocks the desktop, and apps run as it.
_Avoid_: `crowbar-person`, app user

**Account file**:
The file, kept out of git, that gives an image the person's account name and password hash when Base builds it.

**Lab account**:
The person's account an image gets with no account file, and the one the checks use. Its name and password are both `crowbar`, published in this repository, so an image with it is for the lab alone.

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

**Summon gesture**:
Super+Space. The compositor takes it before any app hears it. With the Agent's surface up and the keyboard in a window, it returns the keyboard to the surface; with the keyboard in the surface, it puts the surface away.

**Agent's surface**:
Where the person and the Agent talk: a drawer on the right of the display the pointer was on at the summon, above every window. Putting it away hides it and stops nothing the Agent is doing.
_Avoid_: Agent window, chat window, panel

**Mark**:
The small sign of what the Agent is doing: working, done, waiting for the person, or looking. It is in the bar, and over a fullscreen window while the Agent works, looks, or waits. The Agent's surface never comes up on its own; the mark is how the person knows.
_Avoid_: Notification, indicator, badge

**Desktop context**:
What the Agent is given of the desktop with each message, without a share: the title and app ID of the window with the keyboard, every window by workspace, and the displays. Never what a window or the clipboard holds.

**Share**:
The person's act, in the Agent's surface, that gives the Agent the clipboard or a capture of a window or a display. A share is for once, or it is an allowance.
_Avoid_: Attach, grant

**Allowance**:
A share that lasts for one conversation: the Agent may take the clipboard and any capture again without asking. It ends with the conversation, when the person switches it off, or when the machine restarts, and nothing is taken while the desktop is locked. Each take shows in the mark, in an outline the compositor draws around what was captured, and in the conversation.
_Avoid_: Permission, consent

**Person**:
The one who uses the machine and is in charge of it.
_Avoid_: User, owner

## History

**The Fedora blueprint**:
The first plan: a curated desktop on Fedora with five default apps and no AI.

**The substrate vision**:
The second plan: a lean base each person shaped into their own OS with Tools their agent built, under a deterministic security model. It ended at the `substrate-final` tag.
