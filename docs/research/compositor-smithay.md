# Compositor: Smithay, a Hyprland fork, or wlroots

Whether CrowbarOS should write its own Wayland compositor in Rust on the
Smithay library, and what stands in the way. The alternatives are a maintained
Hyprland fork and a compositor on wlroots. Researched 14 November 2026 from
the Smithay, niri, cosmic-comp, Hyprland, sway, wlroots, Zed, and void-packages
sources, the NVIDIA driver README, and the Mesa docs.

Markers:

- **[unverified]**: only secondary sources, or I could not open the primary page.
- **[docs conflict]**: the primary sources disagree with each other.
- **[inference]**: my reading of the sources, not a statement in them.

## Verdict

**A lot of work.** Not blocked. Three facts decide it:

1. **Smithay has the security hooks the vision needs, and they are opt-in.**
   Every protocol global exists only if the compositor constructs it. The
   sensitive ones take a per-client filter. Key presses pass through a filter
   that can keep a key from every client. Two shipping desktops (COSMIC and
   niri) run on it. See sections 1 and 2.
2. **The cost is real and ongoing.** Smithay's last crates.io release is 0.7.0
   from June 2025. Both production users pin a git commit. Every release breaks
   API. Smithay gives no window management, no screencopy for wlr clients, no
   output management, and no Vulkan renderer. niri took one developer about
   five and a half months to reach 0.1.0, and two years to get floating windows
   and X11 support. See sections 1 and 3.
3. **NVIDIA is a cost, not a barrier, with one sharp edge for us.** The
   proprietary driver has had GBM since 495, and niri and COSMIC run on it. But
   GPUI 0.2.2, the version we pin, renders with blade, and blade froze after
   the first frame on NVIDIA under Smithay compositors. Zed fixed it by moving
   to wgpu in February 2026. That fix is not in any published `gpui` crate. See
   sections 4 and 5.

## Takeaways for CrowbarOS

1. **Smithay fits ADR 0014 better than either alternative out of the box.**
   Stock Hyprland and sway offer virtual input, input-method, data-control,
   screencopy, and layer-shell to every unsandboxed client. With Smithay we
   leave them out or filter them to the shell. With the others we patch them
   out and carry the patch.
2. **Hyprland is the worst fit for the trust path.** Its control socket runs
   Lua inside the compositor, including `exec_cmd`. It loads C++ plugins into
   the compositor process. Its permission system is off by default. Void
   packages none of it.
3. **Budget for tracking Smithay's git master.** Plan a pinned commit, a
   regular bump, and fixes for breaking changes at each bump.
4. **Decide the GPUI renderer question with the compositor.** Staying on GPUI
   0.2.2 with blade means a known freeze on NVIDIA under a Smithay compositor.
   Moving to GPUI from Zed's git gives wgpu but drops the crates.io pin and
   our layer-shell backport must be re-checked (upstream GPUI now has
   layer-shell).
5. **The NVIDIA proprietary driver means glibc Base and the nonfree repo.**
   Void builds `nvidia` for `x86_64` only. The open path (nouveau + NVK + Zink)
   is in Void's Mesa and needs neither.
6. **Do not fork niri or cosmic-comp into the MIT tree.** Both are GPL-3.0.
   Reading them for how they use Smithay is the shortcut. Copying code is not.
7. **X11 can stay out of the compositor.** niri runs X11 apps through
   xwayland-satellite, a separate process. That pattern suits "each app gets
   its own XWayland, or none". Void packages xwayland-satellite 0.8.3.

## 1. Smithay

### Project facts

- License MIT. Repository last pushed 14 November 2026. 4,299 commits on
  `master`. https://github.com/Smithay/smithay ,
  https://api.github.com/repos/Smithay/smithay
- Releases on crates.io: 0.3.0 (July 2021), then 0.4.0 (January 2025), 0.5.0
  (February 2025), 0.6.0 (April 2025), 0.7.0 (24 June 2025). Nothing since.
  https://crates.io/api/v1/crates/smithay
- So the release cadence is irregular: a three-and-a-half-year gap, four
  releases in five months, then seventeen months of none. **[inference]**
- Users track git. niri depends on a pinned git `rev` with the crates.io
  version commented out.
  https://github.com/niri-wm/niri/blob/main/Cargo.toml
  cosmic-comp names `0.7.0` and then patches it to a git `rev`.
  https://github.com/pop-os/cosmic-comp/blob/master/Cargo.toml
- API stability: every release and the unreleased section of the changelog
  open with "Breaking changes". The current unreleased list includes a new
  mandatory decoration callback, a rework of `virtual_keyboard`, removal of
  `start_dnd`, and replacement of all `delegate_*!` macros.
  https://github.com/Smithay/smithay/blob/master/CHANGELOG.md
  I found no written stability promise.
- Maintainers. Commits to `master` in the last twelve months by author:
  Victoria Brekenfeld 108, Ian Douglas Scott 76, Brian J. Tarricone 48, Ivan
  Molodetskikh 17, Hojjat 17, PolyMeilex 15, then a long tail.
  https://api.github.com/repos/Smithay/smithay/commits?since=2025-11-14T00:00:00Z
  (counted from the GitHub API on 14 November 2026)
- Funding. The two most active authors list System76 as their company on
  GitHub. https://github.com/Drakulix , https://github.com/ids1024
  Ivan Molodetskikh is the niri author and lists no employer.
  https://github.com/YaLTeR
  The repository has no funding file. I found no statement of how Smithay is
  funded. **[unverified]** that System76 pays for Smithay work as such.
- Minimum Rust 1.88, edition 2024. System libraries by feature: libwayland,
  libxkbcommon, libudev, libinput, libgbm, libseat, xwayland, libEGL,
  libpixman, libdisplay-info.
  https://github.com/Smithay/smithay/blob/master/Cargo.toml ,
  https://github.com/Smithay/smithay
- The README lists users: COSMIC, niri, Catacomb, Pinnacle, wprs, xfwl4
  (Xfce's Wayland compositor), and others. https://github.com/Smithay/smithay
- Size, as GitHub counts bytes of Rust: Smithay 4.4 MB, niri 3.6 MB,
  cosmic-comp 2.7 MB. Smithay's example compositors: `smallvil` 44 KB in 10
  files, `anvil` 362 KB in 18 files.
  https://api.github.com/repos/Smithay/smithay/languages ,
  https://api.github.com/repos/niri-wm/niri/languages ,
  https://api.github.com/repos/pop-os/cosmic-comp/languages ,
  https://github.com/Smithay/smithay/tree/master/smallvil ,
  https://github.com/Smithay/smithay/tree/master/anvil

### How a protocol gets exposed

- Each protocol helper has a `*State` struct. Its constructor inserts the
  global. The compositor implements a `*Handler` trait. Dropping the state
  removes the global. https://github.com/Smithay/smithay/blob/master/src/wayland/mod.rs
- So nothing is offered unless the compositor constructs it. This is the
  basis for "simply not offer" in section 2.

### What Smithay provides and what we write

Module paths are under https://github.com/Smithay/smithay/tree/master/src .
"Opt in" means the global exists only if we construct its state. "Filter"
means the constructor or a `can_view` hook takes a per-client predicate; I
found `can_view` in each module marked so.

| Area | Smithay module | Opt in | Per-client filter | What we write |
| --- | --- | --- | --- | --- |
| Nested backend in a window (winit) | `backend/winit` | Cargo feature | n/a | Glue. Smithay's docs say to prefer the X11 backend because winit "does not integrate into calloop too well". winit is pinned to `=0.31.0-beta.4`. |
| Nested backend as an X11 client | `backend/x11` | Cargo feature | n/a | Glue. |
| DRM/KMS, planes, direct scanout | `backend/drm`, `backend/drm/compositor` | Cargo feature | n/a | Device and output setup, the frame loop, hotplug. |
| Device discovery | `backend/udev.rs` | Cargo feature | n/a | Glue. |
| Input | `backend/libinput`, `input/` | Cargo feature | n/a | Device configuration, key bindings, focus policy. |
| Session (seatd or elogind through libseat) | `backend/session/libseat.rs` | Cargo feature | n/a | VT switch handling. |
| GLES renderer | `backend/renderer/gles`, `glow.rs` | Cargo feature | n/a | Our own shaders and render elements. |
| Software renderer | `backend/renderer/pixman` | Cargo feature | n/a | Nothing, if used as a fallback. |
| Vulkan | `backend/vulkan` | Cargo feature | n/a | **Everything past device setup.** The module "does not provide abstractions for logical devices, rendering or memory allocation". A Vulkan renderer is an open request (issues #134 from 2019 and #2165). |
| Multi-GPU rendering | `backend/renderer/multigpu` | Cargo feature | n/a | Choice of render GPU. |
| Damage tracking | `backend/renderer/damage` | Used by us | n/a | Render element lists. |
| Window space helper | `desktop/space` | Used by us | n/a | **All window management:** layout, workspaces, focus, rules, animation. |
| xdg-shell | `wayland/shell/xdg` | Yes | No | Handler: map, configure, move, resize, popups. |
| wlr-layer-shell | `wayland/shell/wlr_layer` | Yes | Yes (`new_with_filter`) | Handler and layer arrangement. |
| xdg-decoration | `wayland/shell/xdg/decoration.rs` | Yes | Yes | Policy, and drawing server-side decoration ourselves. |
| linux-dmabuf | `wayland/dmabuf` | Yes | Yes | Format lists and feedback. |
| Explicit sync (linux-drm-syncobj-v1) | `wayland/drm_syncobj` | Yes | Yes | Handler. |
| Fractional scale | `wayland/fractional_scale` | Yes | No | Scale policy. |
| presentation-time | `wayland/presentation` | Yes | No | Feedback timing. |
| Session lock (ext-session-lock) | `wayland/session_lock` | Yes | Yes | Lock state and rendering while locked. |
| Idle notify and inhibit | `wayland/idle_notify`, `wayland/idle_inhibit` | Yes | No | Idle policy. |
| Input method, text input | `wayland/input_method`, `wayland/text_input` | Yes | Input method: yes | Popup placement. |
| Virtual keyboard, virtual pointer | `wayland/virtual_keyboard`, `wayland/virtual_pointer` | Yes | Yes | Handler. On master a virtual keyboard arrives as an input device the compositor processes. |
| Data control (clipboard managers) | `wayland/selection/wlr_data_control`, `ext_data_control` | Yes | Yes | Handler. |
| ext-image-copy-capture | `wayland/image_copy_capture`, `image_capture_source` | Yes | Yes | Rendering the captured frames. |
| wlr-screencopy | **None** | n/a | n/a | The whole protocol. niri wrote its own (`src/protocols/screencopy.rs`). |
| ext-foreign-toplevel-list | `wayland/foreign_toplevel_list` | Yes | Yes | Feeding it. |
| wlr-foreign-toplevel-management, ext-workspace, wlr-output-management, wlr-gamma-control | **None** | n/a | n/a | The whole protocols, if wanted. niri wrote each (`src/protocols/`). |
| security-context-v1 | `wayland/security_context` | Yes | Yes, required | Accept the new listener, mark its clients. |
| XWayland | `xwayland/`, `wayland/xwayland_shell.rs` | Cargo feature | Yes | X11 window management on top. Or none: use xwayland-satellite. |
| libei input emulation | `backend/libei` | Cargo feature, in `default` | n/a | Leave it off. |

Sources for the table:
https://github.com/Smithay/smithay/blob/master/Cargo.toml (features),
https://github.com/Smithay/smithay/blob/master/src/backend/mod.rs (backends,
the winit remark),
https://github.com/Smithay/smithay/blob/master/src/backend/vulkan/mod.rs
(Vulkan scope), https://github.com/Smithay/smithay/issues/134 ,
https://github.com/Smithay/smithay/issues/2165 ,
https://smithay.github.io/smithay/smithay/backend/drm/compositor/index.html
(planes and scanout),
https://smithay.github.io/smithay/smithay/backend/renderer/multigpu/index.html ,
https://github.com/Smithay/smithay/tree/master/src/wayland (module list),
https://github.com/Smithay/smithay/blob/master/CHANGELOG.md (virtual keyboard
change), https://github.com/niri-wm/niri/tree/main/src/protocols (what niri
had to write).

- `DrmCompositor` assigns elements to overlay and cursor planes, tests each
  with an atomic commit, and falls back to the renderer. Direct scanout needs
  atomic modesetting.
  https://smithay.github.io/smithay/smithay/backend/drm/compositor/index.html
- The renderer docs list one hardware API: "Raw OpenGL ES 2".
  https://github.com/Smithay/smithay/blob/master/src/backend/renderer/mod.rs
- cosmic-comp's roadmap for Epoch 2 lists a Vulkan renderer. **[unverified]**:
  read through a search summary of
  https://system76.com/blog/post/cosmic-epoch-2-and-3-roadmap , not the page.

## 2. Security fit

### What Smithay lets us do

- **Hide a global from some clients.** Constructors such as
  `WlrLayerShellState::new_with_filter`, `SecurityContextState::new`,
  `VirtualKeyboardManagerState::new`, and `InputMethodManagerState::new` take a
  predicate over the client. niri passes one closure, `client_is_unrestricted`,
  to layer-shell, session-lock, security-context, input-method, virtual
  keyboard, virtual pointer, foreign-toplevel, ext-workspace, output
  management, screencopy, and data-control.
  https://github.com/niri-wm/niri/blob/main/src/niri.rs (search
  `client_is_unrestricted`)
- **Identify sandboxed clients.** `SecurityContextHandler::context_created`
  hands the compositor a listener and the context's `sandbox_engine`,
  `app_id`, and `instance_id`. The docs say the filter "*must* exclude clients
  created through a security context for the protocol to be correct and
  secure."
  https://github.com/Smithay/smithay/blob/master/src/wayland/security_context/mod.rs
- The protocol itself: sandbox engines attach a context to connections from
  inside the sandbox, and "the compositor can then restrict the features that
  the sandboxed connections can use."
  https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/staging/security-context/security-context-v1.xml
- **A stricter model than niri's is open to us.** niri marks a client
  `restricted` when it arrives on a security-context socket and trusts every
  other client. The predicate is ours to write, so it could admit only the
  shell, for example by giving the shell its own connection. **[inference]**
  from the niri code above; I did not build this.
- **Intercept a key.** `KeyboardHandle::input` takes a filter that returns
  `FilterResult::Forward` or `FilterResult::Intercept`. Intercept means "Do not
  forward".
  https://github.com/Smithay/smithay/blob/master/src/input/keyboard/mod.rs
  This is how the confirm gesture can stay out of every Tool.
- **Not offer a protocol.** Do not construct its state. See section 1.
- **Compositor-owned overlay.** The compositor builds the list of render
  elements for each frame and decides their order, so it can draw its own
  element above every client surface. **[inference]** from the renderer and
  `DrmCompositor` design; no source states it as a guarantee.
  https://smithay.github.io/smithay/smithay/backend/drm/compositor/index.html

### What a Smithay compositor exposes if written the usual way

- niri's own security page is blunt: "Niri assumes that programs running
  unsandboxed on the host are **trusted**." Any client on its socket can
  record the screen, emulate input, read the clipboard, create fullscreen
  layer surfaces "that can steal the user's input, pretend to be a password
  entry", and unlock a locked session. Its IPC socket can spawn clients. Its
  D-Bus interfaces give screencast and full input listening and emulation.
  https://github.com/niri-wm/niri/blob/main/docs/wiki/Security-Model.md
- So Smithay does not make a compositor safe. It makes a safe one possible.

### Hyprland 0.57

- **Control socket.** `$XDG_RUNTIME_DIR/hypr/[HIS]/.socket.sock` takes
  hyprctl-style requests.
  https://github.com/hyprwm/hyprland-wiki/blob/main/content/ipc/_index.md
- `hyprctl eval` runs a Lua string in the compositor. Dispatchers include
  `exec_cmd` (runs through `sh -c`) and `exec_raw`.
  https://github.com/hyprwm/hyprland-wiki/blob/main/content/configuring/core/advanced-configuration/using-hyprctl.md ,
  https://github.com/hyprwm/hyprland-wiki/blob/main/content/configuring/core/dispatchers.md
  Our apps use this today (`/eval hl.dispatch(...)` in
  [`apps/shell/src/session.rs`](../../apps/shell/src/session.rs)).
- Hyprland says it cannot protect the socket: "Hyprland itself cannot enforce
  this; consult the documentation for your application sandbox of choice."
  https://github.com/hyprwm/hyprland-wiki/blob/main/content/configuring/core/advanced-configuration/permissions.md
- **Plugins.** "Plugins are written in C++ and will run as a part of
  Hyprland." The permissions page warns that an attacker could issue
  `hyprctl plugin load /tmp/my-malicious-plugin.so`.
  https://github.com/hyprwm/hyprland-wiki/blob/main/content/hyprland-plugins/using-plugins.md ,
  permissions page above.
- **Permission system.** Five permissions: `screencopy`, `plugin`, `keyboard`
  (new keyboard devices), `cursorpos`, `input-capture`. It needs
  `hyprland-guiutils`. It is "disabled by default" until
  `ecosystem.enforce_permissions` is set. Rules match the client's binary path
  by regex. Permissions page above.
- **Sandboxed clients.** Hyprland implements security-context. A client with a
  context sees only a fixed allow-list of globals (seat, data device,
  xdg-shell, dmabuf, and similar). Everything else, including layer-shell,
  virtual keyboard, virtual pointer, input-method, data-control, and
  screencopy, is privileged.
  https://github.com/hyprwm/Hyprland/blob/main/src/managers/ProtocolManager.cpp
  (`isGlobalPrivileged`)
- Unsandboxed clients see all of them. The protocol list is at
  https://github.com/hyprwm/Hyprland/tree/main/src/protocols . Removing one
  means a source patch. **[inference]**: I found no config switch per protocol.

### sway and wlroots

- sway hides a fixed list of globals from clients that have a security
  context: input-method, data-control (both), screencopy, image-copy-capture,
  export-dmabuf, layer-shell, session-lock, virtual keyboard, virtual pointer,
  output management, foreign-toplevel, and others. The code carries
  `// TODO: add a way for users to configure an allow-list`.
  https://github.com/swaywm/sway/blob/master/sway/server.c (`is_privileged`,
  `filter_global`)
- Unsandboxed clients see everything. A stricter policy is a small patch to
  that one function. **[inference]**
- wlroots ships the same protocol set as separate C modules
  (`wlr_security_context_v1`, `wlr_layer_shell_v1`, `wlr_virtual_keyboard_v1`,
  `wlr_session_lock_v1`, `wlr_linux_drm_syncobj_v1`,
  `wlr_ext_image_copy_capture_v1`, and so on). A compositor creates the ones
  it wants.
  https://gitlab.freedesktop.org/wlroots/wlroots/-/tree/master/types

### The vision's requirements against the three options

The requirements are from [ADR 0014](../adr/0014-a-yes-comes-from-the-person.md)
and section 3 of the [vision](../VISION.md). Sources for each cell are the
subsections above. Cells about our own compositor are **[inference]**: they
say what the API permits.

| Requirement | Our compositor on Smithay | Hyprland fork | wlroots (sway, labwc, or our own) |
| --- | --- | --- | --- |
| No principal can synthesize input: no virtual keyboard, virtual pointer, or input-method for Tools | Do not construct those states, or filter them to the shell. Leave `backend_libei` off. | All three are offered to every unsandboxed client. Remove by source patch. | sway offers them to every unsandboxed client. Change `is_privileged`, or in our own compositor do not create them. |
| Nothing but the shell reads input meant for another surface | No input-capture or data-control unless we add it. | `input-capture` and `cursorpos` permissions exist but enforcement is off by default. | No equivalent prompt. Do not create the globals. |
| Confirm gesture is never delivered to a Tool | `FilterResult::Intercept` in the keyboard filter. | Compositor key binds exist, but binds live in user Lua config, and the control socket evaluates Lua. **[inference]** that a bind can be changed that way. | Compositor handles keys before forwarding; in sway, binds are user config and reachable over its IPC. **[unverified]** for sway IPC details; I did not read them. |
| Trusted chrome a Tool cannot draw over or imitate | Layer-shell filtered to the shell. Compositor can draw its own top element. | Layer-shell open to all unsandboxed clients. Overlay drawing means compositor C++ changes. | Layer-shell open to all unsandboxed clients in sway. Same patch point. |
| No shared X11 server | Leave the `xwayland` feature off. Run xwayland-satellite per app if needed. | Built-in XWayland is shared. | wlroots XWayland is shared and optional at build time. |
| No screen or clipboard reading by Tools | Do not construct image-copy-capture or data-control, or filter to a Base service. | `screencopy` permission, off by default. Data-control open to unsandboxed clients. | Open to unsandboxed clients in sway. |
| No channel that launches programs or loads code into the compositor | We define the IPC. No plugin loader exists unless we write one. | Control socket runs Lua and `exec_cmd`. Plugin loader in the tree. Both would have to be cut out and kept out across upstream releases. | sway has an IPC socket with command execution **[unverified]**; no plugin loader. Our own compositor has neither. |
| Sandboxed Tools get a reduced protocol set | security-context plus our predicate. | Built in: fixed allow-list. | Built in to sway: fixed deny-list. |
| Trusted UI cannot be themed or rebound from the Environment | Configuration is ours; keep it in Base. | Config is user Lua, reloadable; permissions need a restart "for security reasons". | sway config is a user file. Our own compositor: ours. |

## 3. Production users of Smithay

### niri

- GPL-3.0. Repository now at `niri-wm/niri`; `YaLTeR/niri` redirects.
  https://api.github.com/repos/YaLTeR/niri
- First commit 7 August 2023, "Init from smallvil". First tag
  `v0.1.0-alpha.1` on 26 November 2023. `v0.1.0` on 27 January 2024.
  https://api.github.com/repos/niri-wm/niri/commits?until=2023-08-20T00:00:00Z ,
  https://github.com/niri-wm/niri/releases
- Floating windows came in 25.01. Xwayland integration came in 25.08.
  https://github.com/niri-wm/niri/blob/main/README.md
- Status today: "Niri is stable for day-to-day use". Latest release 26.04
  (25 April 2026); releases about every three to five months. README and
  releases page above.
- Team: in the last twelve months Ivan Molodetskikh made 392 commits; the next
  author made 26.
  https://api.github.com/repos/niri-wm/niri/commits?since=2025-11-14T00:00:00Z
- **XWayland.** niri does not integrate Xwayland. It creates the X11 sockets,
  exports `$DISPLAY`, and starts xwayland-satellite on demand. Limits: X11
  apps that position windows at screen coordinates misbehave.
  https://github.com/niri-wm/niri/blob/main/docs/wiki/Xwayland.md
  The author's reasons: "Integrating Xwayland is quite a bit of work, as the
  compositor needs to implement parts of an X11 window manager" and "You tend
  to get an endless stream of X11 bugs".
  https://github.com/niri-wm/niri/blob/main/docs/wiki/FAQ.md
- xwayland-satellite is MPL-2.0, latest 0.8.3 (September 2026).
  https://github.com/Supreeeme/xwayland-satellite
- **Screen sharing.** Through portals and PipeWire. It needs D-Bus, PipeWire,
  and `xdg-desktop-portal-gnome`. niri also serves ext-image-copy-capture and
  wlr-screencopy. It can black out chosen windows in a cast.
  https://github.com/niri-wm/niri/blob/main/docs/wiki/Screencasting.md
- **Multi-monitor.** "a core part of the design from the very start. Mixed DPI
  works." README above.
- **systemd.** niri's default features are `dbus`, `systemd`, and
  `xdp-gnome-screencast`. A `dinit` feature exists. Without a service manager
  it runs as `niri --session`.
  https://github.com/niri-wm/niri/blob/main/Cargo.toml ,
  https://github.com/niri-wm/niri/blob/main/docs/wiki/Getting-Started.md
  Void packages niri 26.04, so it builds for runit.
  https://github.com/void-linux/void-packages/blob/master/srcpkgs/niri/template

### cosmic-comp

- GPL-3.0. Repository created 15 December 2021. Latest tag `epoch-1.11.0`.
  https://api.github.com/repos/pop-os/cosmic-comp ,
  https://github.com/pop-os/cosmic-comp/tags
- COSMIC alpha 1 shipped 26 August 2024 and Epoch 1 on 11 December 2025.
  **[unverified]**: dates read from a search summary of System76's blog
  (https://blog.system76.com/post/cosmic-alpha-released-heres-what-people-are-saying/ ,
  https://blog.system76.com/post/cosmic-1-0-8-released/ ), not the pages.
  On those dates, the compositor took about two years eight months to alpha
  and four years to 1.0, with paid staff. **[inference]**
- Team: last twelve months, Victoria Brekenfeld 161 commits, Ian Douglas Scott
  81, Hojjat 41, Ilia Malanin 26.
  https://api.github.com/repos/pop-os/cosmic-comp/commits?since=2025-11-14T00:00:00Z
- **XWayland.** Built in, using Smithay's `xwayland` module (`src/xwayland.rs`,
  `xwayland_shell.rs`, `xwayland_keyboard_grab.rs`).
  https://github.com/pop-os/cosmic-comp/tree/master/src
- **Screen sharing.** Its own portal backend, xdg-desktop-portal-cosmic
  (GPL-3.0). https://github.com/pop-os/xdg-desktop-portal-cosmic
  **[unverified]** how it talks to the compositor; I did not read it.
- **systemd.** Default feature set is `systemd` (libsystemd, logind,
  journald). Cargo.toml above. Void does not package cosmic-comp.
  https://github.com/void-linux/void-packages/tree/master/srcpkgs
  (no `cosmic-comp` directory)
- **Multi-monitor.** **[unverified]**: not researched beyond the open issue
  "Two GPUs, One monitor per GPU low performance".
  https://github.com/pop-os/cosmic-comp/issues/281

### Others

- xfwl4, Xfce's Wayland compositor, is on Smithay and GPL-3.0.
  https://github.com/xfce-mirror/xfwl4 , Smithay README.
- Pinnacle (latest v0.2.4, June 2026) calls itself work in progress.
  https://github.com/pinnacle-comp/pinnacle , Smithay README.

### What this implies for two people

- niri is the closer comparison: one person, tiling only at first, no
  Xwayland, GNOME's portal for screencast. It was usable by its author within
  months and "stable for day-to-day use" for others within about a year to
  two. **[inference]** from the dates above.
- Our compositor can be smaller than niri. The shell is a separate GPUI
  client, so the compositor needs no bar, launcher, or screenshot UI. It
  needs window placement, focus, outputs, the lock path, and the trusted-input
  path. **[inference]**
- The long tail is what costs: hotplug, mixed DPI, suspend, GPU quirks, client
  bugs. niri's open issues matching "nvidia" include at least 17 with ten or
  more comments.
  https://github.com/niri-wm/niri/issues?q=is%3Aissue+is%3Aopen+nvidia+sort%3Acomments-desc

### Forking niri or cosmic-comp

- Both are GPL-3.0, so a fork is GPL-3.0. Shipping it as a separate binary
  beside MIT code is aggregation under the GPL, and the MIT code stays MIT.
  **[unverified]**: this is the standard reading of
  https://www.gnu.org/licenses/gpl-faq.html#MereAggregation ; I could not
  extract the passage this session, and it is not legal advice.
- The practical limits: the compositor could not share source files with MIT
  crates in `crates/`; any code moved from the fork into the shell or broker
  would carry GPL-3.0; and a distributed image must offer the fork's source.
  **[inference]**
- niri's security model is the opposite of ours (unsandboxed clients are
  trusted), and its default features assume D-Bus and GNOME's portal. A fork
  starts by removing things. Security page and Cargo.toml above.

## 4. NVIDIA

### Background

- **EGLStreams and GBM.** Most compositors allocate buffers through GBM. The
  NVIDIA README has a chapter "GBM and GBM-based Wayland Compositors" from
  driver 495.44 on. It is absent from the 470 branch.
  https://download.nvidia.com/XFree86/Linux-x86_64/495.44/README/gbm.html
  (present),
  https://download.nvidia.com/XFree86/Linux-x86_64/470.256.02/README/gbm.html
  (404)
- Current requirements: "DRM KMS must be enabled", `libgbm.so.1` from Mesa
  21.2, egl-wayland 1.1.8 or later.
  https://download.nvidia.com/XFree86/Linux-x86_64/595.112.07/README/gbm.html
- **[docs conflict]** The same README's KMS chapter still says "Buffer
  allocation and submission to DRM KMS using gbm is not currently supported."
  It reads as stale text.
  https://download.nvidia.com/XFree86/Linux-x86_64/595.112.07/README/kms.html
- Void's own handbook is also stale here: it says GNOME and KDE have
  EGLStreams backends and "Most other Wayland compositors require drivers that
  implement the GBM interface", pointing at Mesa.
  https://github.com/void-linux/void-docs/blob/master/src/config/graphical-session/wayland.md
- **Explicit sync.** NVIDIA's egl-wayland 1.1.14 (18 July 2024) lists
  "Implement Explicit Sync (linux-drm-syncobj-v1)". 1.1.15 added "Significant
  stability fixes for explicit sync" and the `__NV_DISABLE_EXPLICIT_SYNC`
  switch. https://github.com/NVIDIA/egl-wayland/releases
- The driver README says explicit sync needs two kernel commits "present in
  6.8.x or newer kernels".
  https://download.nvidia.com/XFree86/Linux-x86_64/595.112.07/README/wayland-issues.html
- That driver 555 was the first with explicit sync, and that its absence
  caused flicker and out-of-order frames, is **[unverified]**: I found it only
  in secondary sources (for example
  https://www.phoronix.com/news/NVIDIA-555.58-Linux-Driver ). NVIDIA's own
  driver page did not load.
- Smithay has the server side (`wayland/drm_syncobj`). cosmic-comp wires it
  (`src/wayland/handlers/drm_syncobj.rs`). Sources in section 1 and
  https://github.com/pop-os/cosmic-comp/tree/master/src/wayland/handlers
- **Open kernel modules.** "We recommend the use of open kernel modules on all
  GPUs that support it." The installer defaults to them. "Blackwell and later
  are only supported by the open kernel modules." They are dual MIT/GPLv2. The
  proprietary flavor covers Turing through Hopper.
  https://download.nvidia.com/XFree86/Linux-x86_64/595.112.07/README/kernel_open.html
- **Open Mesa path.** NVK is "a conformant Vulkan 1.4 implementation",
  supports Kepler through Ada and consumer Blackwell, and needs Linux 6.6.
  From Mesa 25.1, Turing and later get "NVK+Zink as their OpenGL
  implementation by default", and "The Nouveau GL driver is no longer
  supported on these cards." https://docs.mesa3d.org/drivers/nvk.html
  The page says nothing on performance or power use against the proprietary
  driver.

### What compositors say today

- niri: "You need a GPU and drivers recent enough to support GBM", and enable
  kernel modesetting, "usually" `nvidia-drm.modeset=1`. README: "**NVIDIA**:
  seems to work fine."
  https://github.com/niri-wm/niri/blob/main/docs/wiki/Getting-Started.md ,
  https://github.com/niri-wm/niri/blob/main/README.md
- Hyprland: recommends "the official, proprietary drivers (with the open
  source kernel modules)". The page gives no version numbers and defers to
  distro wikis.
  https://github.com/hyprwm/hyprland-wiki/blob/main/content/nvidia/_index.md
- sway: still warns "Proprietary GPU drivers are not supported by sway. Do not
  report issues." and gates it behind `--unsupported-gpu`.
  https://github.com/swaywm/sway/blob/master/sway/main.c
- cosmic-comp: I found no stated minimum. COSMIC keeps an "NVIDIA graphics
  meta-issue". https://github.com/pop-os/cosmic-epoch/issues/168

### Void Linux

- `nvidia` is version 595.112.07, in the `nonfree` repository,
  `archs="x86_64"`. It builds its DKMS module from `kernel-open/`, installs
  `libnvidia-egl-wayland` and `libnvidia-egl-gbm`, and blacklists `nouveau`,
  `nova_core`, and `nova_drm`. It ships suspend hooks for `zzz` and elogind
  that call `nvidia-sleep.sh`.
  https://github.com/void-linux/void-packages/blob/master/srcpkgs/nvidia/template ,
  https://github.com/void-linux/void-packages/tree/master/srcpkgs/nvidia/files
- `x86_64` in a Void template means glibc; musl is the separate arch
  `x86_64-musl`. So there is no proprietary driver on musl. **[inference]**
  from the template; I did not open the xbps-src manual. NVIDIA ships glibc
  binaries only.
- The handbook maps cards to packages: Turing and newer use `nvidia`,
  Maxwell to Volta `nvidia580`, Kepler `nvidia470`, Fermi `nvidia390`.
  https://docs.voidlinux.org/config/graphical-session/graphics-drivers/nvidia.html
- NVIDIA's newest driver directory is 620.70.06, so Void is behind the newest
  branch. https://download.nvidia.com/XFree86/Linux-x86_64/
- Void packages `mesa-vulkan-nouveau` for the NVK path. Handbook page above.

### Issue table

| Issue | Cause | Current status | What it costs us |
| --- | --- | --- | --- |
| Compositor will not start on old drivers | No GBM before driver 495 | Solved upstream. Void's `nvidia` is 595. | Nothing on Turing and newer. Older cards use legacy packages or nouveau. |
| Black screen, flicker, or corruption at start | simpledrm and nvidia-drm both create DRM nodes. NVIDIA: "set the fbdev=1 module parameter for the nvidia-drm kernel module." | Workaround documented by NVIDIA. The default value of `fbdev` in 595 is **[unverified]**. | A modprobe option in Base, set when the driver is present. |
| Kernel modesetting off | nvidia-drm needs KMS for GBM | README 595: KMS "is enabled by default". niri still tells users to set `nvidia-drm.modeset=1`. **[docs conflict]** | Set it explicitly in Base; cheap. |
| Flicker and frame-order glitches without explicit sync | Driver has no implicit sync **[unverified]** | Solved upstream on both sides: egl-wayland 1.1.14 and later, Smithay `drm_syncobj`. Needs kernel 6.8 or newer. | Construct the syncobj global. Test it. |
| Flicker in a corner of the screen at high refresh rates | Smithay's swapchain reuses the most recently released buffer; "The nvidia driver appears to keep reading the buffer for a short while." NVIDIA tracks it as a driver bug. | **Open.** Affects niri and COSMIC, not Hyprland. Smithay PR #2151 works around it and is not merged. | Carry the PR as a patch, or wait. Our own item to watch. |
| High VRAM use by the compositor | Driver heap does not return freed buffers | Workaround: an NVIDIA application profile setting `GLVidHeapReuseRatio` to 0 for the compositor's process name. niri: "it is unlikely that the underlying heuristic will see a proper fix." | Ship a profile file in Base naming our binary. |
| VRAM leak when windows close | Reported on both NVIDIA and AMD under Smithay | **Open** (Smithay #1562). Not NVIDIA-specific. | Watch. |
| Hybrid laptops: render on one GPU, scan out on the other | Cross-device buffer import can fail; Smithay then copies | Works in general through `multigpu`. **Open** bug: 10-bit outputs on the Intel GPU go blank when NVIDIA renders (Smithay #2119). COSMIC has an open low-performance issue (#281). | Choose the render GPU with care. Test on a real hybrid laptop. Expect to carry fixes. |
| Display mux on laptops | "no Wayland compositors currently support this functionality" | Open, by NVIDIA's account | None unless we want it. |
| RTD3 power-off of the discrete GPU | "no established public API through which Wayland compositors can power off video memory via RTD3" | Open | Battery life on hybrid laptops. |
| VRR / G-Sync | Driver "forces all VRR capable displays into VRR mode by default"; `conceal_vrr_caps` hides it. The 555 README limited Wayland VRR to "Volta or newer GPUs". | Works upstream on recent cards. cosmic-comp has open VRR issues (#1170, #1348). How well Smithay's VRR path works on NVIDIA is **[unverified]**. | Optional feature. Test before promising it. |
| Suspend and resume corrupts or crashes clients | Video memory is lost unless preserved | With open modules it "is handled automatically if `NVreg_UseKernelSuspendNotifiers=1` is enabled". Otherwise the system must write to `/proc/driver/nvidia/suspend` around sleep. Void ships `zzz` and elogind hooks. Default of the notifier parameter is **[unverified]**. | Make sure Base's suspend path runs the hook. Test on hardware. |
| Colour pipeline gaps | "the COLOR_ENCODING and COLOR_RANGE plane properties are unsupported" | Open | Matters only for HDR and colour management. |
| No overlay planes | nvidia-drm "does not yet register an overlay plane" (KMS chapter; may be stale) | **[unverified]** whether still true in 595 | Less direct scanout. Performance only. |
| GPUI 0.2.2 window freezes after the first frame | blade did not recover from an out-of-date swapchain. Seen on NVIDIA under niri and Smithay's anvil, not on AMD or Intel, not under GNOME or KDE. | Fixed in Zed 0.225.13 after the move to wgpu (PRs #46758, #50640). **Not** in the `gpui` crate: 0.2.2 from October 2025 is the latest. | Move our apps to GPUI from Zed's git, or backport the fix, or accept no NVIDIA support for now. See section 5. |
| Zed hangs on driver 580 before 580.82.07 | Driver bug | Fixed by NVIDIA in 580.82.07 | Void's `nvidia580` is 580.196.07, past the fix. |
| musl Base | NVIDIA publishes glibc binaries; Void builds `x86_64` only | Not solvable by us | Base must be glibc to support the proprietary driver. |

Sources for the table, in row order:
https://download.nvidia.com/XFree86/Linux-x86_64/495.44/README/gbm.html ;
https://download.nvidia.com/XFree86/Linux-x86_64/595.112.07/README/wayland-issues.html ;
https://download.nvidia.com/XFree86/Linux-x86_64/595.112.07/README/kms.html ;
https://github.com/NVIDIA/egl-wayland/releases ;
https://github.com/Smithay/smithay/issues/1862 ,
https://github.com/Smithay/smithay/pull/2151 ,
https://github.com/niri-wm/niri/issues/2597 ,
https://github.com/NVIDIA/open-gpu-kernel-modules/issues/801 ;
https://github.com/niri-wm/niri/blob/main/docs/wiki/Nvidia.md ,
https://github.com/NVIDIA/egl-wayland/issues/126 ;
https://github.com/Smithay/smithay/issues/1562 ;
https://github.com/Smithay/smithay/issues/2119 ,
https://github.com/pop-os/cosmic-comp/issues/281 ;
wayland-issues page above (mux, RTD3, colour) ;
kms page above (VRR, overlay planes),
https://download.nvidia.com/XFree86/Linux-x86_64/555.42.02/README/wayland-issues.html ,
https://github.com/pop-os/cosmic-comp/issues/1170 ,
https://github.com/pop-os/cosmic-comp/issues/1348 ;
https://download.nvidia.com/XFree86/Linux-x86_64/595.112.07/README/powermanagement.html ;
https://github.com/zed-industries/zed/issues/50734 ,
https://github.com/zed-industries/zed/pull/46758 ,
https://github.com/zed-industries/zed/pull/50640 ,
https://crates.io/api/v1/crates/gpui ;
https://github.com/zed-industries/zed/issues/35948 ;
Void template above.

- None of these rows is special to writing our own compositor except the two
  Smithay ones (corner flicker, hybrid 10-bit). The rest hit any compositor on
  the proprietary driver. **[inference]**
- A wlroots or Hyprland base does not escape NVIDIA either. sway refuses
  support outright. Hyprland says issues "could arise ... that we have no
  control over". Sources above.

## 5. Clients: GPUI on Smithay compositors

- Zed runs on niri and COSMIC. The evidence is a long bug history that is now
  closed, not a support statement.
  https://github.com/niri-wm/niri/issues/2335 (closed March 2026),
  https://github.com/zed-industries/zed/issues/50574 (closed)
- **The blade freeze.** With blade (Zed 0.224 and earlier), the first frame
  drew and then nothing updated; the log flooded with "Acquire failed because
  the surface is out of date". A resize recovered it. Reported on NVIDIA under
  niri and Smithay's anvil. https://github.com/zed-industries/zed/issues/50734
- The reporter attributes the trigger to "a serial mismatch between
  `send_pending_configure` and `commit` in Smithay". **[unverified]**: that is
  a user's analysis in the issue, not a maintainer's finding. I found no
  matching Smithay issue.
- Zed replaced blade with wgpu ("gpui: Remove blade, reimplement linux
  renderer with wgpu", merged 13 February 2026) and fixed a regression in
  0.225.13 ("Fix handling of `surface.configure` on Linux").
  https://github.com/zed-industries/zed/pull/46758 ,
  https://github.com/zed-industries/zed/pull/50640
- **Our apps are on the blade side of that line.** `apps/shell` pins
  `gpui = "=0.2.2"` and its lockfile has `blade-graphics 0.7.1`
  ([`apps/shell/Cargo.toml`](../../apps/shell/Cargo.toml),
  [`apps/shell/Cargo.lock`](../../apps/shell/Cargo.lock)). crates.io has no
  newer `gpui`. https://crates.io/api/v1/crates/gpui
  A user asked Zed in March 2026 to publish a crate with the fix; the comment
  says the workaround is "to use gpui from git".
  https://github.com/zed-industries/zed/issues/50574
- Whether the freeze also hits Intel or AMD under our own Smithay compositor
  is not known. The reports say it does not. **[unverified]** for our build.
- **Protocols GPUI binds.** All optional (`.ok()`): xdg-activation,
  cursor-shape, primary-selection, wp-viewporter, fractional-scale,
  xdg-decoration, wlr-layer-shell (versions 1 to 5), KDE blur,
  ext-background-effect, text-input-v3, pointer-gestures, xdg-dialog,
  xdg-system-bell. It also uses linux-dmabuf.
  https://github.com/zed-industries/zed/blob/main/crates/gpui_linux/src/linux/wayland/client.rs
- Smithay has a module for each of those except KDE blur, which is optional.
  Section 1 and https://github.com/Smithay/smithay/tree/master/src/wayland
- Upstream GPUI now has `layer_shell.rs`, so our backport
  ([`apps/shell/patches/README.md`](../../apps/shell/patches/README.md)) may
  not be needed on a newer GPUI.
  https://github.com/zed-industries/zed/tree/main/crates/gpui_linux/src/linux/wayland
- The shell uses layer surfaces on the Background, Top, and Overlay layers,
  with exclusive zones and exclusive keyboard interactivity
  ([`apps/shell/src/main.rs`](../../apps/shell/src/main.rs)). Smithay's
  `wlr_layer` module covers these; the compositor arranges them.

## 6. The alternatives

### A Hyprland fork

- BSD-3-Clause, C++. https://github.com/hyprwm/Hyprland
- It no longer uses wlroots. Its backend is aquamarine, "a very light linux
  rendering backend library" that is "C++-only".
  https://github.com/hyprwm/aquamarine
- Releases: 0.56.0 on 20 July 2026, 0.56.1 and 0.56.2 within sixteen days,
  0.57.0 on 28 October 2026, 0.57.1 and 0.57.2 within twelve days.
  https://github.com/hyprwm/Hyprland/releases
- Breakage: configuration and control are now Lua (`hl.config`, `hl.bind`,
  `hyprctl eval`); `reload full-reset` exists for "switching to/from
  Lua/hyprlang". Our apps' IPC already follows the Lua form.
  https://github.com/hyprwm/hyprland-wiki/blob/main/content/configuring/core/advanced-configuration/using-hyprctl.md
- What we would package for Void, from its build file: aquamarine ≥ 0.15.0,
  hyprcursor ≥ 0.1.7, hyprutils ≥ 0.14.3, hyprgraphics ≥ 0.5.1,
  hyprwayland-scanner ≥ 0.3.10, hyprland-protocols ≥ 0.8.0, plus Lua 5.5,
  glslang, udis86, libinput ≥ 1.29, wayland-protocols ≥ 1.49, xkbcommon ≥
  1.11. Also xdg-desktop-portal-hyprland for screen sharing and
  hyprland-guiutils for permission prompts.
  https://github.com/hyprwm/Hyprland/blob/main/CMakeLists.txt , permissions
  page in section 2.
- Void has none of the hypr* packages (already checked; no `hyprland`
  template in https://github.com/void-linux/void-packages/tree/master/srcpkgs ).
- Size: 7.8 MB of C++ by GitHub's count, about twice niri.
  https://api.github.com/repos/hyprwm/Hyprland/languages
- The fork's standing job would be to keep the control socket, the plugin
  loader, and the privileged protocols removed while upstream ships a minor
  release every three months. **[inference]**

### wlroots

- MIT, C. Latest 0.20.2 (7 July 2026); 0.20.0 was 26 March 2026.
  https://gitlab.freedesktop.org/wlroots/wlroots ,
  https://gitlab.freedesktop.org/wlroots/wlroots/-/releases
- It describes itself as "about 60,000 lines of code you were going to write
  anyway", with KMS/DRM, libinput, Wayland, X11, and headless backends, an
  Xwayland abstraction, and GLES2, Vulkan, and pixman renderers.
  https://gitlab.freedesktop.org/wlroots/wlroots/-/blob/master/README.md ,
  https://gitlab.freedesktop.org/wlroots/wlroots/-/tree/master/render
- Against Smithay: wlroots has a Vulkan renderer and wlr-screencopy; Smithay
  has neither. wlroots makes numbered releases that distributions package
  (Void has 0.18, 0.19, 0.20); Smithay is tracked from git. wlroots is C, so
  the compositor is C or goes through bindings, and it would be the one
  component on the trust path not in Rust. **[inference]** on the last point.
- **sway** as the base: MIT, C, 1.12 released 25 May 2026, about 1.6 MB of C.
  It already filters privileged globals for sandboxed clients. It refuses
  support for proprietary drivers. It is a tiling window manager with i3
  semantics; whether that suits the shell's design is Jason's call.
  https://github.com/swaywm/sway ,
  https://api.github.com/repos/swaywm/sway/languages , section 2 and 4.
- **labwc** as the base: GPL-2.0, C, 0.20.3 (30 October 2026), stacking,
  builds on wlroots 0.20. GPL-2.0 raises the same question as a niri fork.
  https://github.com/labwc/labwc ,
  https://github.com/labwc/labwc/blob/master/meson.build
- Screen sharing on this path is xdg-desktop-portal-wlr, which Void packages.
  It implements only Screenshot and ScreenCast, and has both wlr-screencopy
  and ext-image-copy-capture sources.
  https://github.com/emersion/xdg-desktop-portal-wlr ,
  https://github.com/emersion/xdg-desktop-portal-wlr/tree/master/src/screencast
- That matters for Smithay too: a Smithay compositor that offers
  ext-image-copy-capture to a Base portal service could use
  xdg-desktop-portal-wlr without writing wlr-screencopy. **[inference]**; not
  tested.

## Risks

1. **Smithay moves under us.** No release in seventeen months, breaking
   changes on master, and two of the three most active authors work for one
   company whose priority is COSMIC. If System76 changes course, the library
   slows. Section 1.
2. **The NVIDIA long tail lands on two people.** niri and COSMIC absorb these
   bugs with large user bases. We would meet them with one or two test
   machines. Section 4.
3. **GPUI and the compositor interact.** The blade freeze shows that a
   client-side renderer bug can look like a compositor bug and take months to
   close. We own both ends, on a GPUI version upstream has left behind.
   Section 5.
4. **No Vulkan renderer in Smithay.** The compositor would render with GLES
   while the apps render with Vulkan. That works today on every Smithay
   compositor, but effects that need Vulkan are ours to build. Section 1.
5. **"Safe by construction" is only as good as our predicate.** niri shows the
   default outcome of Smithay is a compositor that trusts everything. The
   filter and the client identity scheme are new security code that needs
   adversarial tests. Section 2.
6. **Scope creep toward a desktop.** Screen sharing, output configuration,
   session lock, idle, input methods for non-Latin text, and accessibility
   each arrive as "one more protocol". ADR 0014 leaves the accessibility
   route open, and text input for CJK needs input-method, which is on the
   list of routes to close. Section 1 and
   [ADR 0014](../adr/0014-a-yes-comes-from-the-person.md).
7. **X11 apps.** xwayland-satellite covers most, not all. Whether CrowbarOS
   needs X11 at all is undecided. Section 3.
8. **Licence drift.** Reading GPL-3.0 compositors while writing an MIT one
   needs care about what is copied. Section 3.

## What a time-boxed spike should prove

1. A compositor started from `smallvil` runs nested (X11 or winit backend) on
   the development host and shows the shell's bar, dock, launcher, and
   wallpaper as layer surfaces, with correct exclusive zones.
2. Studio Settings opens as an xdg-shell window and renders through GPUI's
   Vulkan path under that compositor, on the Intel VirtIO setup in
   [`gpu-vulkan.md`](../development/gpu-vulkan.md).
3. One key chord is intercepted with `FilterResult::Intercept`, and a test
   client with keyboard focus logs that it never saw the press or release.
4. Layer-shell is visible to the shell and invisible to a second client. The
   test names how the compositor tells the two apart.
5. Virtual keyboard, virtual pointer, input-method, data-control, and
   image-copy-capture globals are absent from `wayland-info` for an ordinary
   client.
6. A client started through security-context sees the reduced set.
7. The same binary runs on a TTY in a Void guest: udev, libinput, libseat on
   seatd, DRM, no systemd. Record what Void packages it needs.
8. The apps' two uses of the Hyprland socket (focus a workspace, focus a
   window) are replaced by something we define, and the choice is written
   down.
9. On a machine with an NVIDIA card and Void's `nvidia` package: the
   compositor starts, the GPUI 0.2.2 apps are checked for the first-frame
   freeze, and suspend and resume are tried once. If no such machine is to
   hand, say so in the record.
10. Two outputs with different scales, and one hotplug.
11. A count: lines we wrote, Smithay commit pinned, and hours spent, so the
    full cost can be estimated from a measured start.

## Not verified

- That NVIDIA driver 555 was the first with explicit sync, and what failed
  without it. Secondary sources only. NVIDIA's driver page
  (https://www.nvidia.com/Download/driverResults.aspx/228215/en-us/) did not
  render. The primary evidence I have is the egl-wayland 1.1.14 release note
  and the kernel requirement in the 595 README.
- The default values of `nvidia-drm.fbdev` and
  `NVreg_UseKernelSuspendNotifiers` in driver 595. The README pages I read do
  not state them.
- Whether nvidia-drm still lacks overlay planes and still cannot take GBM
  buffers through KMS, as the KMS chapter says. The GBM chapter contradicts
  the second.
- Minimum NVIDIA driver versions recommended by cosmic-comp, Hyprland, and
  sway. Hyprland's page gives none; sway does not support the driver; I found
  no cosmic-comp statement. niri says only "recent enough to support GBM".
- How well VRR works on NVIDIA under Smithay.
- How Smithay is funded, and whether System76 employs its two top committers
  to work on Smithay itself. Their GitHub profiles list the company; nothing
  more.
- Any written API stability policy for Smithay.
- COSMIC's alpha and Epoch 1 dates and the Epoch 2 Vulkan renderer plan. Read
  from search summaries of System76's blog, not the pages.
- How xdg-desktop-portal-cosmic gets frames from cosmic-comp, and how
  cosmic-comp handles multi-monitor.
- The root cause of the GPUI freeze on NVIDIA under Smithay. The "serial
  mismatch in Smithay" explanation is a user's.
- Whether GPUI 0.2.2 with blade freezes under a Smithay compositor on Intel
  or AMD. Reports say no; we have not run it.
- sway's IPC command surface. I read only its global filter and GPU check.
- That `archs="x86_64"` excludes musl. This is how Void templates work as I
  understand them; I did not open the xbps-src manual.
- The GPL "mere aggregation" passage. I cite the FAQ URL but could not
  extract the text this session. The licence consequences in section 3 are my
  reading and not legal advice.
- Whether a per-protocol off switch exists in Hyprland's configuration. I
  found none in the pages I read; I did not search every option.
- Everything marked **[inference]** in the requirements table: it says what
  the Smithay API permits, and none of it has been built.
- Hyprland's wiki at https://wiki.hypr.land/Nvidia/ was too large to read
  through the fetch tool; I read the same page from the wiki's source
  repository instead.
