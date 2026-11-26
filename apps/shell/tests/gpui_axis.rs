//! Exercise the exact frame selector used by our maintained Wayland patch.
use gpui::{Pixels, Point, ScrollDelta, point, px};

#[path = "../gpui-source/src/platform/linux/wayland/axis.rs"]
#[rustfmt::skip] // Generated dependency source keeps GPUI's formatting.
mod axis;
