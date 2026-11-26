//! Exercise the exact payload selector used by our maintained Wayland patch.
//! GPUI's own full test harness is unnecessary for MIME and byte transport rules.
use gpui::{ClipboardEntry, ClipboardItem, Image, ImageFormat};

#[path = "../gpui-source/src/platform/linux/wayland/clipboard/data.rs"]
#[rustfmt::skip] // Generated dependency source keeps GPUI's formatting.
mod clipboard_data;
