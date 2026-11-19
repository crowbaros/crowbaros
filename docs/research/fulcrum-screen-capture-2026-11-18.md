# Fulcrum screen capture options for the Agent

**Date:** November 18, 2026  
**Issue:** [crowbaros/crowbaros#232](https://github.com/crowbaros/crowbaros/issues/232) (blocking [#239](https://github.com/crowbaros/crowbaros/issues/239), under Wayfinder [#230](https://github.com/crowbaros/crowbaros/issues/230))  
**Recommendation:** **Option A — a dedicated, one-shot `capture` command on Fulcrum's UID-checked control channel returning raw pixels in a sealed `memfd` via `SCM_RIGHTS`.**

Option A is the smallest, cleanest, and safest choice for Milestone 3. Fulcrum already has offscreen GLES readback (`Renderer::draw_to_image`), CPU scene rendering (`State::draw_scene`), sealed `memfd_create`, `SO_PEERCRED` UID checks on the control socket, `SCM_RIGHTS` descriptor passing over `rustix`, and window/output addressing (`snapshot` / `outputs`). It requires no new Wayland protocol XMLs, no new runtime crates, no write path into untrusted client `wl_shm_pool` buffers, and no Wayland capture globals in `policy.rs`. Crucially, it lets Fulcrum capture an **isolated focused window** or **output minus the Agent's own overlay**, while enforcing one-shot, person-armed, visually indicated capture directly in the compositor and strictly refusing ordinary apps.

---

## 1. Ground truth in Fulcrum (`compositor/`)

Fulcrum is written from the ground up in Rust with **no Smithay crates** (`compositor/tests/dependencies.rs:11-40`), **only `rustix` as a runtime dependency** (`compositor/Cargo.toml:23-25`), and **`#![forbid(unsafe_code)]` everywhere outside `compositor/sys/`** (`compositor/README.md:191`, `compositor/src/main.rs:7`). Any capture design has to fit these existing seams cleanly.

### 1.1 Renderer architecture, dma-buf import, and readback

- **Shared scene representation:** Both renderers draw the exact same `Scene` (`compositor/src/scene.rs:39-44`): an ordered list of `Element::Surface { surface, rect }`, `Element::Solid { rect, colour }` (window frames), and `Element::Cursor { shape, rect }`, built in logical desktop coordinates by `State::logical_scene` (`compositor/src/scene.rs:59-124`) and scaled to output physical pixels by `State::in_pixels` (`compositor/src/scene.rs:128-142`).
- **GLES vs. CPU fallback on TTY:** `Tty::init` (`compositor/src/backend/tty.rs:384-399`) initializes `Gpu { gbm, renderer: Renderer }` (`compositor/src/backend/tty.rs:236-250`) unless `FULCRUM_RENDERER=cpu` is set or EGL/GBM fails to load. `Headless` (`compositor/src/backend/headless.rs:15-35`) and `Nested` (`compositor/src/backend/nested.rs:11-15`) draw with the CPU renderer (`State::render_output` / `State::draw_scene`).
- **Why CPU readback alone cannot capture GPU apps:** When `Tty` has `gpu: Some(Gpu)`, it calls `state.offer_dmabuf_on_gpu(stat.st_rdev, gpu.renderer.gles().clone())` (`compositor/src/backend/tty.rs:418-420`). Client dma-bufs (verified via `fstatfs` == `DMA_BUF_MAGIC` in `compositor/sys/src/dmabuf.rs:88-93`) are imported zero-copy via `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` + `glEGLImageTargetTexture2DOES` (`compositor/sys/src/gles.rs:703-774`, supporting both `GL_TEXTURE_2D` and `GL_TEXTURE_EXTERNAL_OES`) and stored on the surface as `Content::Gpu(GpuContent)` (`compositor/src/shm.rs:274-282`, `compositor/src/surface.rs:153-167`). For `Content::Gpu`, `Content::image()` and `Current::shown()` return `None` (`compositor/src/surface.rs:176-180, 280`), so **`State::draw_scene` silently skips GPU-imported dma-buf surfaces** (`compositor/src/scene.rs:248-252`). `Tty::draw` already notes this exact invariant when falling back to dumb buffers on a monitor while a GPU is present (`compositor/src/backend/tty.rs:765-773`).
- **Offscreen GLES readback already exists:** `Renderer::draw_to_image(&mut self, state: &State, scene: &Scene) -> io::Result<Image>` (`compositor/src/render.rs:162-167`) creates an offscreen `GL_RGBA` FBO texture (`self.gles.offscreen(w, h)`, `compositor/sys/src/gles.rs:804-846`), draws every element in the `Scene` (both `Content::Gpu` dma-buf textures and `Content::Copied` SHM textures, with viewporter `source_in_buffer()` cropping and linear filtering), executes `glFinish()` (`compositor/src/render.rs:157`, `compositor/sys/src/gles.rs:947-950`), and reads back pixels via `glReadPixels(GL_RGBA, GL_UNSIGNED_BYTE)` (`compositor/sys/src/gles.rs:922-944`), returning an `Image` of premultiplied ARGB8888 `u32` pixels.
- **No Y-flip needed:** Fulcrum's vertex shader maps pixel `y = 0` (top) to GL NDC `y = -1.0` (row 0 of framebuffer memory), so `glReadPixels` returns rows top-to-bottom without vertical inversion (`compositor/sys/src/gles.rs:11-13, 270-277`; pixel-parity verified against the CPU renderer in `compositor/tests/gles.rs:16-67`).
- **Hardware cursor plane detail:** When `Tty` uses `Buffers::Gpu` with a DRM cursor plane, the primary scanout buffer is rendered with `state.scene(output)` (without the pointer), and the compositor's own cursor sits on the separate `CursorPlane` (`compositor/src/backend/tty.rs:751-761, 775-779`). Rendering a capture from `Scene` (via `state.scene` vs. `state.scene_with_cursor`) therefore gives deterministic cursor inclusion/exclusion regardless of whether the hardware cursor plane is active, and avoids touching `Monitor.buffers[front|back]`, `DamageTracker`, or `flip_pending`.
- **No image encoder in `image.rs`:** `compositor/src/image.rs` (126 lines) is solely a raw `Image { width, height, pixels: Vec<u32> }` container with premultiplied ARGB8888 alpha-over blending (`over`), `Image::from_shm`, `Image::resized` (nearest-neighbor), `draw`, and `fill`. Fulcrum has no PNG, JPEG, or WebP encoder and links no compression crate (`compositor/Cargo.toml:23-25`).

### 1.2 Explicit sync (`wp_linux_drm_syncobj_v1`)

- Fulcrum implements explicit sync via DRM timeline syncobjs (`compositor/src/explicit_sync.rs`, `compositor/sys/src/syncobj.rs`).
- When a client commits a dma-buf whose acquire point is not yet signalled, `State::hold_commit` (`compositor/src/explicit_sync.rs:226-254`) registers a kernel `eventfd` via `DRM_IOCTL_SYNCOBJ_EVENTFD` (`compositor/sys/src/syncobj.rs:171-192`) and holds the `Pending` state in `ExplicitSync.held`. The commit is **not applied** to `surface.current` until the acquire point signals (`State::release_held` -> `State::apply_commit`, `compositor/src/explicit_sync.rs:257-269`, `compositor/src/surface.rs:664-667`).
- **Consequence for capture:** Every `Content::Gpu` visible in `surface.current` has **already passed its acquire point**. Capture does not need any extra syncobj wait. Furthermore, because `Renderer::draw` calls `glFinish()` before returning and `State::let_go` only signals `gpu.release` when newer content replaces `surface.current.content` (`compositor/src/shm.rs:297-311`), synchronous offscreen readback cannot race with client buffer reuse.
- **Event-loop stall caveat:** `glFinish()` + `glReadPixels` blocks Fulcrum's single-threaded `epoll` loop (`compositor/src/event_loop.rs`) for the duration of one offscreen render and readback (a few milliseconds at 1080p–4K; a 3840×2160 frame is 33.2 MiB uncompressed). That is fine for **one-shot, person-initiated captures**, but would hitch frame pacing under continuous 30–60 fps video streaming unless rewritten around async PBOs or dma-buf blits.

### 1.3 Output scale, fractional scale, viewporter, and transforms

- Outputs store `scale_120` in `120..=360` in steps of `30` (1.0× to 3.0× in 0.25× steps) (`compositor/src/output.rs:34, 210-217`). Logical size is `size * 120 / scale_120` rounded to nearest (`compositor/src/output.rs:48-52`).
- `State::scene(number)` builds a logical scene and scales each element's rectangle edge-by-edge via `scale_rect(rect, output.scale_120)` (`compositor/src/scene.rs:128-142, 265-272`) so adjacent elements meet cleanly at physical output resolution.
- `wp_fractional_scale_manager_v1` reports the maximum `scale_120` of outputs a window entered (`compositor/src/fractional_scale.rs:58-68`).
- `wp_viewporter` crop/destination is handled in both `Current::source_in_buffer()` (for GLES, `compositor/src/surface.rs:255-274`) and `Current::shown()` (for CPU, `compositor/src/surface.rs:279-301`).
- **Minor existing Fulcrum limitation:** `wl_output` always advertises `transform::NORMAL` (`compositor/src/output.rs:274`). `wl_surface.set_buffer_transform` is validated and stored in `surface.current.transform` (`compositor/src/surface.rs:549-555`), but neither `scene.rs` nor `render.rs` applies non-normal surface rotation/flip yet.
- **Minor alpha detail for isolated window capture:** `compositor/src/render.rs:139` currently passes `opaque = false` to `frame.draw(...)` for all surfaces. For full outputs cleared with opaque `BACKGROUND` (`0xff1a_1a1a`), this is harmless; for an isolated single-window capture cleared to transparent (`0x0000_0000`), Fulcrum should track per-surface format opacity (e.g., `XRGB8888` / `XR24` -> `opaque = true`) so opaque XRGB dma-bufs never leak undefined alpha bytes.

### 1.4 Client identity, Wayland policy table, and control channel

- **ADR 0032 client split:** Under [ADR 0032](../adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md) (proposed on `adr/0032-fulcrum-serves-the-shell-and-the-persons-apps`), compartments and the Warden are dropped in favor of separate accounts: the compositor service user, the shell service user (connecting on its UID-checked Wayland and control sockets), and the person's account (where ordinary apps connect to `WAYLAND_DISPLAY`). What Fulcrum offers the Agent is Milestone 3's ADR ([#239](https://github.com/crowbaros/crowbaros/issues/239)) to decide.
- **Default-deny Wayland registry (`policy.rs` + `globals.rs`):** `State::fill_registry` and `State::bind` (`compositor/src/globals.rs:124-160`) check `policy::offers(&identity, &global.interface)` and return the identical `wl_display.error::INVALID_OBJECT` whether a global does not exist or is denied, preventing global-ID probing. `policy::NOBODY` (`compositor/src/policy.rs:44-76`) explicitly lists all five Wayland capture globals (`ext_image_copy_capture_manager_v1`, `ext_output_image_capture_source_manager_v1`, `ext_foreign_toplevel_image_capture_source_manager_v1`, `zwlr_screencopy_manager_v1`, `zwlr_export_dmabuf_manager_v1`) so adding any of them to a client row fails unit and guest tests.
- **UID-checked control channel (`control.rs`):** `State::listen_for_control` and `State::listen_for_unlock_authority` (`compositor/src/control.rs:103-118`) bind `0o660` Unix stream sockets and verify every peer via kernel `SO_PEERCRED` (`peer_uid` via `rustix::net::sockopt::socket_peercred`, `compositor/src/connections.rs:344-346`, `compositor/src/control.rs:149-158`). It already speaks line-delimited flat JSON (`MAX_LINE = 4096`, `MAX_PENDING = 1 MiB`, `MAX_CONNECTIONS = 4`) and already exposes window IDs (`"0x2"`, `app_id`, `title`, `workspace`, `active` in `snapshot`, `compositor/src/control.rs:344-368`) and outputs (`name`, `identity`, `width`, `height`, `x`, `y`, `scale` in `outputs`, `compositor/src/control.rs:371-395`).
- **Descriptor passing and sealed `memfd` already exist in Fulcrum:** Today `control.rs` uses plain `Read`/`Write` on `UnixStream` (`compositor/src/control.rs:182, 290`) without ancillary data. However, `fulcrum-wire` already implements bounded, nonblocking `SCM_RIGHTS` descriptor passing in safe Rust via `rustix::net::sendmsg`/`recvmsg` (`compositor/wire/src/socket.rs:150-240`), and `dmabuf.rs` already creates read-only sealed memfds (`memfd_create` with `CLOEXEC | ALLOW_SEALING`, followed by `F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE | F_SEAL_SEAL`, `compositor/src/dmabuf.rs:515-538`).
- **Client SHM pools are read-only in Fulcrum today:** `BufferFile::new` (`compositor/sys/src/shm.rs:23-41`) only implements `read_pixels` using positioned `pread` (`FileExt::read_exact_at`, `compositor/sys/src/shm.rs:97-99`) to avoid `SIGBUS` when a hostile client truncates its file. Fulcrum has **no** write path into client-allocated `wl_shm` pools or client-allocated `wl_buffer` objects.

---

## 2. Primary-source Wayland capture protocol specifications

Every protocol count and requirement below is taken directly from the upstream XML specifications.

### 2.1 `ext-image-copy-capture-v1` + `ext-image-capture-source-v1` (+ `ext-foreign-toplevel-list-v1`)

Primary sources:
- [`ext-image-copy-capture-v1.xml`](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/raw/main/staging/ext-image-copy-capture/ext-image-copy-capture-v1.xml) (`wayland-protocols`, `staging/ext-image-copy-capture/`, v1)
- [`ext-image-capture-source-v1.xml`](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/raw/main/staging/ext-image-capture-source/ext-image-capture-source-v1.xml) (`wayland-protocols`, `staging/ext-image-capture-source/`, v1)
- [`ext-foreign-toplevel-list-v1.xml`](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/raw/main/staging/ext-foreign-toplevel-list/ext-foreign-toplevel-list-v1.xml) (`wayland-protocols`, `staging/ext-foreign-toplevel-list/`, v1)

| Protocol XML | Status | Interfaces | Requests | Events | Key responsibilities |
| --- | --- | ---: | ---: | ---: | --- |
| `ext-image-copy-capture-v1` | Staging (v1) | 4 | 11 | 15 | `ext_image_copy_capture_manager_v1` (`create_session`, `create_pointer_cursor_session`, `destroy`), `ext_image_copy_capture_session_v1` (`buffer_size`, `shm_format`, `dmabuf_device`, `dmabuf_format`, `done`, `stopped`, `create_frame`), `ext_image_copy_capture_frame_v1` (`attach_buffer`, `damage_buffer`, `capture`, `transform`, `damage`, `presentation_time`, `ready`, `failed`), `ext_image_copy_capture_cursor_session_v1` (`get_capture_session`, `enter`, `leave`, `position`, `hotspot`) |
| `ext-image-capture-source-v1` | Staging (v1) | 3 | 5 | 0 | Opaque `ext_image_capture_source_v1` (frozen v1), `ext_output_image_capture_source_manager_v1` (`create_source(wl_output)`), `ext_foreign_toplevel_image_capture_source_manager_v1` (`create_source(ext_foreign_toplevel_handle_v1)`) |
| `ext-foreign-toplevel-list-v1` | Staging (v1) | 2 | 3 | 7 | Required if capturing windows via `ext_foreign_toplevel_image_capture_source_manager_v1`: `ext_foreign_toplevel_list_v1` (`toplevel`, `finished`, `stop`), `ext_foreign_toplevel_handle_v1` (`title`, `app_id`, `identifier`, `done`, `closed`) |
| **Total (outputs + windows)** | | **9** | **19** | **22** | Plus writable `wl_shm` / `zwp_linux_dmabuf_v1` buffer copy-out path |

Key protocol facts from the XML:
- **Designed for continuous streaming sessions, not one-shot screenshots:** A client must bind 3–4 globals, create an `ext_image_capture_source_v1`, create an `ext_image_copy_capture_session_v1`, wait for buffer constraint events (`buffer_size`, `shm_format` / `dmabuf_device` / `dmabuf_format`, `done`), allocate a matching `wl_buffer` (`wl_shm` or `linux-dmabuf`), create an `ext_image_copy_capture_frame_v1` (at most one active per session or `duplicate_frame` error), call `attach_buffer` + `damage_buffer` + `capture`, and await `transform`, `damage`, `presentation_time`, and `ready` (or `failed` with `buffer_constraints` / `stopped` / `unknown`).
- **Compositor writes into client-owned `wl_buffer`:** The client allocates the `wl_shm` pool or dma-buf and hands a `wl_buffer` to the compositor. Fulcrum would need to add a safe write path (`pwrite` with `O_RDWR` and size checks, or GLES `target(&dmabuf)` rendering into client dma-bufs with explicit format/modifier negotiation).
- **Cursor session complexity:** `ext_image_copy_capture_manager_v1` includes `create_pointer_cursor_session` (taking a `wl_pointer` and producing a separate `ext_image_copy_capture_cursor_session_v1` with `enter`/`leave`/`position`/`hotspot` events and a nested capture session).
- **Window capture duplicates control-channel state:** Capturing a toplevel requires implementing `ext_foreign_toplevel_list_v1` (with stable non-reused 32-byte ASCII identifiers, per-handle lifecycle, `closed`/`done` events), duplicating Fulcrum's existing `control.rs` `snapshot` window table (`compositor/src/control.rs:344-368`), and still leaves no standard field telling the client which toplevel has keyboard focus (as `ext-foreign-toplevel-list-v1` explicitly omits focus/activation state).
- **Positive design hooks in the spec:** `ext_output_image_capture_source_manager_v1.create_source` explicitly permits omitting *"overlays that have been marked as transparent to capturing"*, and `ext_image_copy_capture_session_v1.stopped` explicitly covers *"if the user decides to end the image capture"*.

### 2.2 `wlr-screencopy-unstable-v1`

Primary source:
- [`wlr-screencopy-unstable-v1.xml`](https://gitlab.freedesktop.org/wlroots/wlr-protocols/-/raw/master/unstable/wlr-screencopy-unstable-v1.xml) (`wlr-protocols`, `unstable/`, v3)

| Protocol XML | Status | Interfaces | Requests | Events |
| --- | --- | ---: | ---: | ---: |
| `wlr-screencopy-unstable-v1` | **Deprecated** (v3) | 2 | 6 | 7 |

Key protocol facts from the XML:
- **Explicitly deprecated upstream:** The XML `<description>` states verbatim: *"Note! This protocol is deprecated and not intended for production use. The ext-image-copy-capture-v1 protocol should be used instead."*
- **No isolated window capture:** `zwlr_screencopy_manager_v1` only provides `capture_output` and `capture_output_region` (in output logical coordinates, citing `xdg_output.logical_size`, which Fulcrum does not implement). It **cannot** capture a single window without also capturing overlapping windows, shell panels, or the Agent's own overlay.
- **Legacy dma-buf limitations:** `zwlr_screencopy_frame_v1.linux_dmabuf` (since v3) sends only `(format, width, height)` with no `dev_t` device and no DRM format modifier list.
- **Still requires writing into client-owned `wl_buffer` objects** and implementing `copy`, `copy_with_damage`, `damage`, `flags` (`y_invert`), `ready`, `failed`, and error codes (`already_used`, `invalid_buffer`).

### 2.3 Do GPUI or CrowbarOS's own Rust clients expect Wayland capture protocols?

- **CrowbarOS repo (`apps/` and `base/`):** Zero references to `wlr-screencopy`, `ext-image-copy-capture`, or `ext-image-capture-source`. The Shell (`apps/shell/src/session.rs:216-256`) already talks to Fulcrum's line-JSON control channel.
- **GPUI (Zed's `gpui` crate, pinned at `0.2.2` in `apps/shell/Cargo.toml:9`):** GPUI does **not** use `wlr-screencopy` or `ext-image-copy-capture`. Upstream GPUI's optional `screen-capture` feature (`crates/gpui/src/platform.rs`, `scap_screen_capture` / PipeWire + `xdg-desktop-portal`, and WebRTC `DesktopCapturer` in Zed) targets desktop portals/PipeWire rather than wlroots/ext copy-capture protocols ([zed-industries/zed#28754](https://github.com/zed-industries/zed/issues/28754), [zed-industries/zed#65783](https://github.com/zed-industries/zed/issues/65783)).
- **The Agent harness:** Under `docs/VISION.md` ("The Agent"), the Agent is a system service running from boot, not an arbitrary third-party Wayland screenshot utility (`grim`, `slurp`, `wf-recorder`). If the Agent harness is not itself a Wayland client (or only uses the Shell to render its overlay), requiring a Wayland capture protocol would force the Agent (or a helper binary) to speak Wayland `wl_registry` + `wl_shm` + `wl_output` + 2–3 capture protocols just to grab one PNG for a vision model.

---

## 3. Comparison of options for Milestone 3

| Dimension | Option A: Control-channel `capture` + sealed `memfd` (`SCM_RIGHTS`) | Option B: `ext-image-copy-capture-v1` + `ext-image-capture-source-v1` (+ `ext-foreign-toplevel-list-v1`) | Option C: `wlr-screencopy-unstable-v1` |
| --- | --- | --- | --- |
| **Upstream status** | CrowbarOS internal interface (matches `snapshot`, `outputs`, `configure_output` in `control.rs`) | Staging (testing phase, v1) | **Deprecated upstream** ("not intended for production use") |
| **Fulcrum code size & complexity** | **Small (~200–300 LOC):** `Backend::capture_scene`, `State::window_scene`, `SCM_RIGHTS` reply attachment in `control.rs`, sealed `memfd` helper. 0 new XML files, 0 new Wayland objects | **Large (~900–1,400+ LOC):** 3 XMLs, 9 interfaces, 19 requests, 22 events in `codegen`/`state.rs`/`client.rs`, writable `wl_shm`/`dmabuf` buffer copy-out, constraint negotiation, per-session damage tracking, cursor sessions, foreign toplevel lifecycle | **Medium (~450–650 LOC):** 1 XML, 2 interfaces, 6 requests, 7 events, writable `wl_shm` copy-out, damage waiting (`copy_with_damage`), `xdg_output` logical coords mismatch |
| **External crates added to Fulcrum** | **None** (`rustix` already provides `memfd_create`, `fcntl_add_seals`, `sendmsg`/`SCM_RIGHTS`) | None (hand-coded via `fulcrum-codegen`), but significant state-machine surface in `compositor/` | None |
| **GLES + CPU + dma-buf + explicit sync** | Reuses existing `Renderer::draw_to_image` (GLES FBO + `glReadPixels`) and `State::draw_scene` (CPU) unchanged. Acquires already signalled; no client buffer write hazards | Needs new write path into client `wl_shm_pool` (`pwrite` + `O_RDWR` validation) and/or GLES rendering into client-supplied dma-buf targets | Needs new write path into client `wl_shm_pool` (and lacks dma-buf device/modifier negotiation) |
| **Output & isolated window capture** | **Both cleanly supported:** `target: "output"` (optionally excluding Agent overlay layer) and `target: "window"` / `"focused"` (isolated window + popups at output `scale_120`, zero overlap) | Both supported, **only** if `ext-foreign-toplevel-list-v1` is also implemented (duplicating `control.rs` `snapshot`, still missing focus state) | **Output/region only** — cannot capture an isolated window without overlapping windows or overlays |
| **Client ergonomics (Shell / Agent)** | **Trivial:** 1 JSON request on the existing control socket -> 1 JSON reply + 1 read-only `memfd` to `mmap` and encode (PNG/WebP/downscale) in the Agent/Shell process | Heavy: client must connect to Wayland, bind 3–4 globals, negotiate constraints, allocate `wl_shm_pool`/`wl_buffer`, run 3+ roundtrips per capture | Moderate: client must be a Wayland client, bind `wl_output` + `wl_shm` + manager, allocate `wl_shm_pool`/`wl_buffer` |
| **Ordinary apps blocked** | **Two independent locks:** (1) `SO_PEERCRED` UID check on control socket refuses the person's account; (2) `policy::NOBODY` keeps all Wayland capture globals nonexistent | Single Wayland policy row gating (`policy::offers`) on the Shell/Agent Wayland socket; adds capture globals to `Globals` | Single Wayland policy row gating; adds deprecated capture global to `Globals` |
| **Person-initiated privacy guarantees** | **Strong & native:** One-shot by construction (no session/stream), easily gated on compositor-observed person gesture / arming flag, refused while locked, visible compositor-drawn indicator | Session-oriented (supports indefinite silent frame capture unless Fulcrum injects custom `stopped` policy on top of the spec) | Frame-oriented, no built-in consent or revocation semantics (client can loop `capture_output` silently unless custom-gated) |
| **Third-party tool compatibility (`grim`, portals)** | None (by design for Milestone 3 — ordinary apps are refused anyway) | High if ever opened to a portal/trusted tool later | Legacy wlroots tools (`grim`, older `xdg-desktop-portal-wlr`) |

### Verdict

- **Choose Option A for Milestone 3.** It aligns directly with `docs/LESSONS.md` (*"Build what a person can touch first"*), Fulcrum's minimal zero-Smithay codebase, and `docs/VISION.md` (*"It does not pop up, interrupt, or look at the screen unless the person asks"*).
- **Reject Option C (`wlr-screencopy-unstable-v1`).** It is explicitly deprecated upstream, cannot isolate a window from overlays/other windows, and still forces Fulcrum to implement client `wl_buffer` write-back.
- **Defer Option B (`ext-image-copy-capture-v1` + `ext-image-capture-source-v1`).** It is the right Wayland standard **if and only if** a future milestone needs continuous video capture (screen recording / `xdg-desktop-portal` / PipeWire screensharing). Because Option A builds the exact renderer-agnostic `Backend::capture_scene` + `State::window_scene` primitives underneath, adding `ext-image-copy-capture-v1` later reuses the same rendering core rather than throwing work away.

---

## 4. Concrete design sketch for Option A in Fulcrum

This section maps Option A directly onto Fulcrum's existing modules so ADR #239 and the implementation PR have exact seams.

### 4.1 Renderer & scene seam (`backend/mod.rs`, `backend/tty.rs`, `scene.rs`, `render.rs`)

1. **Add `capture_scene` to `Backend`** (`compositor/src/backend/mod.rs:34-44`):
   - Default implementation (used by `Headless` and `Nested`): `Ok(state.draw_scene(scene))`.
   - `Tty` implementation (`compositor/src/backend/tty.rs:733`):
     - Refuse if `!self.session.borrow().active` (VT switched away).
     - If `self.gpu: Some(gpu)`: call `gpu.renderer.draw_to_image(state, scene)`.
     - If `self.gpu: None` (`FULCRUM_RENDERER=cpu`): call `Ok(state.draw_scene(scene))`.
2. **Add capture-oriented `Scene` builders in `compositor/src/scene.rs`**:
   - **Output capture (`capture_output_scene(output, include_cursor, exclude_namespaces)`):** Uses `logical_scene` + `in_pixels` (so output `scale_120` is honored identically to display rendering), optionally omits the pointer (`scene` vs. `scene_with_cursor`), and skips layer surfaces matching excluded namespaces (e.g. the Agent's own overlay surface, using the same namespace check `lock.rs:125-136` uses for `LOCK_NAMESPACE`).
   - **Window capture (`capture_window_scene(surface, include_popups)`):** Builds a `Scene` containing only the target toplevel window's surface tree (`push_tree`) and optionally its mapped popups (`self.popups` filtered by `toplevel_of`), translated to `(0, 0)` (no compositor window frame, no other windows, no Shell/Agent overlays), scaled by `self.preferred_scale(surface)` (`compositor/src/fractional_scale.rs:58-68`, the max `scale_120` of outputs the window occupies).
   - **XRGB opacity fix:** Pass `opaque = buffer_is_xrgb` into `frame.draw(...)` (`compositor/src/render.rs:139`) so isolated window captures over a transparent background do not inherit undefined alpha from `XR24` dma-bufs.
3. **Downscaling & encoding stay out of Fulcrum:**
   - Fulcrum writes raw pixels (`argb8888` or `rgba8888`) into a sealed `memfd` (`memfd_create("fulcrum-capture", CLOEXEC | ALLOW_SEALING)` + `F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE | F_SEAL_SEAL`, matching `compositor/src/dmabuf.rs:515-538`) and drops its own `OwnedFd` immediately after `sendmsg`.
   - **Why raw `memfd`, not PNG inside Fulcrum:** Keeps `compositor/Cargo.toml` at zero image/compression crates, avoids blocking Fulcrum's single-threaded `epoll` loop with 30–150 ms of CPU PNG compression on a 4K frame, and avoids blowing `control.rs`'s `MAX_PENDING = 1 MiB` (`compositor/src/control.rs:76`) with inline base64. The Shell or Agent harness `mmap`s the read-only sealed `memfd`, optionally downscales for the vision model, and encodes PNG/WebP asynchronously in its own process.

### 4.2 Control-channel wire format (`control.rs`)

Add `SCM_RIGHTS` reply attachment to `ControlConnection` (`compositor/src/control.rs:86-94`, mirroring `fulcrum-wire/src/socket.rs:193-239`'s `fds_out: VecDeque<(u64, OwnedFd)>` + `rustix::net::sendmsg` so a reply's `OwnedFd` always rides with the first byte of its JSON line):

```json
-> {"op":"capture","target":"focused","cursor":false}
<- {"reply":"capture","width":1920,"height":1080,"stride":7680,"format":"rgba8888","scale":150}   [+ 1 SCM_RIGHTS sealed memfd]

-> {"op":"capture","target":"window","id":"0x2"}
<- {"reply":"capture","width":1200,"height":900,"stride":4800,"format":"rgba8888","scale":120}    [+ 1 SCM_RIGHTS sealed memfd]

-> {"op":"capture","target":"output","name":"HDMI-A-1","cursor":false}
<- {"reply":"capture","width":3840,"height":2160,"stride":15360,"format":"rgba8888","scale":150}  [+ 1 SCM_RIGHTS sealed memfd]
```

*(Note: Returning `"rgba8888"` directly from `glReadPixels(GL_RGBA, GL_UNSIGNED_BYTE)` skips the RGBA->ARGB swizzle in `compositor/sys/src/gles.rs:939-943` on the GLES path, while CPU `Image` converts ARGB->RGBA in a single tight pass — and RGBA8888 is directly consumable by standard PNG/WebP encoders without another swizzle.)*

### 4.3 Privacy & trust guarantees

To uphold `docs/VISION.md` (*"It does not pop up, interrupt, or look at the screen unless the person asks"* and *"A yes comes from the person's own hand"*) and `docs/LESSONS.md` (*"No model on the trust path"*):

1. **Ordinary apps are structurally excluded:**
   - All Wayland capture protocols remain absent from `Globals` and listed in `policy::NOBODY` (`compositor/src/policy.rs:44-76`).
   - Control sockets remain `0o660` in Base-managed directories and kernel-verified via `SO_PEERCRED` (`compositor/src/control.rs:149-158`) against the trusted service UID (Shell UID or a dedicated Agent UID, never the person's account).
2. **Always refused while locked:**
   - If `self.locked()` (`compositor/src/lock.rs:60-62`), `capture` unconditionally returns `{"reply":"error","message":"capture is refused while locked"}`. The lock screen and password entry can never be read back.
3. **One-shot, person-armed capture (no silent background polling):**
   - Even if the Agent harness or model is prompt-injected, Fulcrum should not allow silent arbitrary-time screen polling.
   - Because Fulcrum owns all physical keyboard/pointer input (`compositor/src/seat.rs:348-395`) and the summon gesture (`docs/VISION.md`), Fulcrum can require a **one-shot capture arming state** set only by a genuine compositor-observed person act before `op: "capture"` succeeds (consuming the arming token on use and expiring after a short timeout, e.g. 10–30 s, just like `LAUNCH_FOCUS = 10s` in `compositor/src/control.rs:81`):
     - *Concrete Milestone 3 mechanisms for ADR #239 to pick between:* (a) arming one capture when the person presses a compositor-owned "summon with screen" / capture chord (or clicks an explicit "Attach screen / window" button on the trusted Shell/Agent surface while summoned — verifiable by Fulcrum because Fulcrum owns pointer button delivery to the Shell/Agent surface); or (b) when the Agent requests a capture un-armed, Fulcrum requires the person's hand (confirm chord or explicit click) before fulfilling it.
   - **Never arm silently on background timers or IPC alone.**
4. **Visible compositor-drawn indicator & audit log:**
   - Every successful capture logs one structured line via `log::info!` (target, dimensions, caller) and triggers a brief **compositor-drawn capture indicator** (e.g., an unmistakable border flash / corner badge rendered in `scene.rs` above all windows and layers using `Element::Solid`, which neither apps nor the Agent can hide or spoof).
5. **Least-privilege default scope:**
   - Supporting `target: "focused"` (isolated focused window) alongside `target: "output"` lets Milestone 3 default to sharing only the active window when the person asks for help with a specific app, rather than exposing every window on every monitor.

---

## 5. Primary sources

1. **Wayland `ext-image-copy-capture-v1` XML specification (v1, staging):**  
   `https://gitlab.freedesktop.org/wayland/wayland-protocols/-/raw/main/staging/ext-image-copy-capture/ext-image-copy-capture-v1.xml`
2. **Wayland `ext-image-capture-source-v1` XML specification (v1, staging):**  
   `https://gitlab.freedesktop.org/wayland/wayland-protocols/-/raw/main/staging/ext-image-capture-source/ext-image-capture-source-v1.xml`
3. **Wayland `ext-foreign-toplevel-list-v1` XML specification (v1, staging):**  
   `https://gitlab.freedesktop.org/wayland/wayland-protocols/-/raw/main/staging/ext-foreign-toplevel-list/ext-foreign-toplevel-list-v1.xml`
4. **wlroots `wlr-screencopy-unstable-v1` XML specification (v3, deprecated):**  
   `https://gitlab.freedesktop.org/wlroots/wlr-protocols/-/raw/master/unstable/wlr-screencopy-unstable-v1.xml`
5. **Zed / GPUI Wayland screen-capture tracking issues:**  
   `https://github.com/zed-industries/zed/issues/28754` and `https://github.com/zed-industries/zed/issues/65783`
6. **CrowbarOS architecture and Fulcrum source code (`main` at `593a6bb5` + ADR 0032 branch):**
   - `GLOSSARY.md`, `docs/VISION.md`, `docs/LESSONS.md`
   - `docs/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps.md` (on `origin/adr/0032-fulcrum-serves-the-shell-and-the-persons-apps`)
   - `compositor/Cargo.toml`, `compositor/tests/dependencies.rs`, `compositor/protocols/README.md`
   - `compositor/src/control.rs`, `compositor/src/connections.rs`, `compositor/src/policy.rs`, `compositor/src/globals.rs`
   - `compositor/src/scene.rs`, `compositor/src/render.rs`, `compositor/src/image.rs`, `compositor/src/surface.rs`
   - `compositor/src/dmabuf.rs`, `compositor/src/explicit_sync.rs`, `compositor/src/shm.rs`
   - `compositor/src/output.rs`, `compositor/src/fractional_scale.rs`, `compositor/src/windows.rs`, `compositor/src/lock.rs`, `compositor/src/seat.rs`
   - `compositor/src/backend/mod.rs`, `compositor/src/backend/tty.rs`, `compositor/src/backend/headless.rs`, `compositor/src/backend/nested.rs`
   - `compositor/sys/src/gles.rs`, `compositor/sys/src/dmabuf.rs`, `compositor/sys/src/shm.rs`, `compositor/sys/src/syncobj.rs`
   - `compositor/wire/src/socket.rs`, `compositor/tests/gles.rs`
   - `apps/shell/Cargo.toml`, `apps/shell/src/session.rs`
