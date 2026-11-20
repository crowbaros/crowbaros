# Protocols

The XML of every protocol Fulcrum speaks. Each file carries its own license
in its `<copyright>` element. `fulcrum-codegen` turns them into
`wire/src/protocol/`.

| File | Source |
| --- | --- |
| `wayland.xml` | [Wayland](https://gitlab.freedesktop.org/wayland/wayland) 1.26.0, `protocol/wayland.xml` |
| `xdg-shell.xml` | [wayland-protocols](https://gitlab.freedesktop.org/wayland/wayland-protocols) 1.49, `stable/xdg-shell/` |
| `viewporter.xml` | wayland-protocols 1.49, `stable/viewporter/` |
| `linux-dmabuf-v1.xml` | wayland-protocols 1.49, `stable/linux-dmabuf/` |
| `fractional-scale-v1.xml` | wayland-protocols 1.49, `staging/fractional-scale/` |
| `linux-drm-syncobj-v1.xml` | wayland-protocols 1.49, `staging/linux-drm-syncobj/` |
| `presentation-time.xml` | wayland-protocols 1.49, `stable/presentation-time/` |
| `idle-inhibit-unstable-v1.xml` | wayland-protocols 1.49, `unstable/idle-inhibit/` |
| `primary-selection-unstable-v1.xml` | wayland-protocols 1.49, `unstable/primary-selection/` |
| `cursor-shape-v1.xml` | wayland-protocols 1.49, `staging/cursor-shape/`. Its tablet request names `zwp_tablet_tool_v2`, which Fulcrum does not speak: no client is offered tablets, so none holds a tool to name |
| `wlr-layer-shell-unstable-v1.xml` | [wlr-protocols](https://gitlab.freedesktop.org/wlroots/wlr-protocols) `1106a4a1c57562ddd438c547da4c0d6dc150d624`, `unstable/` |

Speaking a protocol is not offering it. CrowbarOS's ADR 0032 decides which
client is offered which global, and a global with no row is offered to
nobody. Add a file here only with the change that offers the protocol.
