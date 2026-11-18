//! libxkbcommon: keymaps and keyboard state.
//!
//! The compositor compiles one keymap, shares its text with every client
//! through a sealed memfd, and keeps the state of the keys and modifiers
//! itself. ADR 0028 allows this library; Fulcrum binds the few functions it
//! uses here, and nothing else.

use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    io,
    os::fd::OwnedFd,
    ptr::NonNull,
};

#[repr(C)]
struct RuleNames {
    rules: *const c_char,
    model: *const c_char,
    layout: *const c_char,
    variant: *const c_char,
    options: *const c_char,
}

#[allow(non_camel_case_types)]
type xkb_keycode_t = u32;
#[allow(non_camel_case_types)]
type xkb_mod_mask_t = u32;
#[allow(non_camel_case_types)]
type xkb_layout_index_t = u32;

const XKB_KEYMAP_FORMAT_TEXT_V1: c_int = 1;
const XKB_KEY_UP: c_int = 0;
const XKB_KEY_DOWN: c_int = 1;
const XKB_STATE_MODS_DEPRESSED: c_int = 1 << 0;
const XKB_STATE_MODS_LATCHED: c_int = 1 << 1;
const XKB_STATE_MODS_LOCKED: c_int = 1 << 2;
const XKB_STATE_LAYOUT_EFFECTIVE: c_int = 1 << 7;

#[link(name = "xkbcommon")]
unsafe extern "C" {
    fn xkb_context_new(flags: c_int) -> *mut c_void;
    fn xkb_context_unref(context: *mut c_void);
    fn xkb_keymap_new_from_names(context: *mut c_void, names: *const RuleNames, flags: c_int) -> *mut c_void;
    fn xkb_keymap_get_as_string(keymap: *mut c_void, format: c_int) -> *mut c_char;
    fn xkb_keymap_unref(keymap: *mut c_void);
    fn xkb_state_new(keymap: *mut c_void) -> *mut c_void;
    fn xkb_state_unref(state: *mut c_void);
    fn xkb_state_update_key(state: *mut c_void, key: xkb_keycode_t, direction: c_int) -> c_int;
    fn xkb_state_serialize_mods(state: *mut c_void, components: c_int) -> xkb_mod_mask_t;
    fn xkb_state_serialize_layout(state: *mut c_void, components: c_int) -> xkb_layout_index_t;
    fn xkb_state_key_get_one_sym(state: *mut c_void, key: xkb_keycode_t) -> u32;
}

unsafe extern "C" {
    fn free(pointer: *mut c_void);
}

/// The names that choose a keymap. Empty fields take libxkbcommon's
/// defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeymapNames {
    pub rules: String,
    pub model: String,
    pub layout: String,
    pub variant: String,
    pub options: String,
}

/// The modifiers and layout as a client is told them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub depressed: u32,
    pub latched: u32,
    pub locked: u32,
    pub group: u32,
}

/// A compiled keymap, its text in a sealed memfd, and the keyboard's state.
pub struct Keyboard {
    context: NonNull<c_void>,
    keymap: NonNull<c_void>,
    state: NonNull<c_void>,
    /// The keymap's text, NUL-terminated, sealed so no client can change or
    /// shrink it.
    keymap_file: OwnedFd,
    keymap_size: u32,
}

impl Keyboard {
    pub fn new(names: &KeymapNames) -> io::Result<Self> {
        let field = |text: &str| {
            CString::new(text).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a NUL in a keymap name"))
        };
        let (rules, model, layout, variant, options) = (
            field(&names.rules)?,
            field(&names.model)?,
            field(&names.layout)?,
            field(&names.variant)?,
            field(&names.options)?,
        );
        let pointer = |text: &CString| {
            if text.as_bytes().is_empty() {
                std::ptr::null()
            } else {
                text.as_ptr()
            }
        };
        let rule_names = RuleNames {
            rules: pointer(&rules),
            model: pointer(&model),
            layout: pointer(&layout),
            variant: pointer(&variant),
            options: pointer(&options),
        };
        // SAFETY: each call gets valid arguments or null where the library
        // allows it, and each result is checked for null before use. The
        // three objects are freed in `Drop`.
        unsafe {
            let context = NonNull::new(xkb_context_new(0)).ok_or_else(|| io::Error::other("no xkb context"))?;
            let Some(keymap) = NonNull::new(xkb_keymap_new_from_names(context.as_ptr(), &rule_names, 0)) else {
                xkb_context_unref(context.as_ptr());
                return Err(io::Error::other(format!("the keymap {names:?} did not compile")));
            };
            let Some(state) = NonNull::new(xkb_state_new(keymap.as_ptr())) else {
                xkb_keymap_unref(keymap.as_ptr());
                xkb_context_unref(context.as_ptr());
                return Err(io::Error::other("no xkb state"));
            };
            let text = xkb_keymap_get_as_string(keymap.as_ptr(), XKB_KEYMAP_FORMAT_TEXT_V1);
            if text.is_null() {
                xkb_state_unref(state.as_ptr());
                xkb_keymap_unref(keymap.as_ptr());
                xkb_context_unref(context.as_ptr());
                return Err(io::Error::other("the keymap has no text"));
            }
            let bytes = CStr::from_ptr(text).to_bytes_with_nul().to_vec();
            free(text.cast());
            let (keymap_file, keymap_size) = match sealed_file(&bytes) {
                Ok(file) => file,
                Err(error) => {
                    xkb_state_unref(state.as_ptr());
                    xkb_keymap_unref(keymap.as_ptr());
                    xkb_context_unref(context.as_ptr());
                    return Err(error);
                }
            };
            Ok(Keyboard {
                context,
                keymap,
                state,
                keymap_file,
                keymap_size,
            })
        }
    }

    /// A descriptor of the keymap's text, for one client, and its size.
    pub fn keymap(&self) -> io::Result<(OwnedFd, u32)> {
        Ok((self.keymap_file.try_clone()?, self.keymap_size))
    }

    /// A key went down or up. `keycode` is the XKB keycode, the evdev code
    /// plus 8. Returns whether the modifiers or layout changed.
    pub fn update(&mut self, keycode: u32, pressed: bool) -> bool {
        let direction = if pressed { XKB_KEY_DOWN } else { XKB_KEY_UP };
        // SAFETY: the state is valid for as long as `self`.
        unsafe { xkb_state_update_key(self.state.as_ptr(), keycode, direction) != 0 }
    }

    pub fn modifiers(&self) -> Modifiers {
        // SAFETY: the state is valid for as long as `self`.
        unsafe {
            let state = self.state.as_ptr();
            Modifiers {
                depressed: xkb_state_serialize_mods(state, XKB_STATE_MODS_DEPRESSED),
                latched: xkb_state_serialize_mods(state, XKB_STATE_MODS_LATCHED),
                locked: xkb_state_serialize_mods(state, XKB_STATE_MODS_LOCKED),
                group: xkb_state_serialize_layout(state, XKB_STATE_LAYOUT_EFFECTIVE),
            }
        }
    }

    /// The keysym a key gives now, for the compositor's own keys.
    pub fn keysym(&self, keycode: u32) -> u32 {
        // SAFETY: the state is valid for as long as `self`.
        unsafe { xkb_state_key_get_one_sym(self.state.as_ptr(), keycode) }
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        // SAFETY: each was made in `new`, and nothing else frees them.
        unsafe {
            xkb_state_unref(self.state.as_ptr());
            xkb_keymap_unref(self.keymap.as_ptr());
            xkb_context_unref(self.context.as_ptr());
        }
    }
}

/// A memfd holding `bytes`, sealed against writes, shrinking, and growing,
/// so that a client can map it and change nothing.
fn sealed_file(bytes: &[u8]) -> io::Result<(OwnedFd, u32)> {
    use rustix::fs::{MemfdFlags, SealFlags};
    let file = rustix::fs::memfd_create("fulcrum-keymap", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;
    let mut written = 0;
    while written < bytes.len() {
        written += rustix::io::write(&file, &bytes[written..])?;
    }
    rustix::fs::fcntl_add_seals(
        &file,
        SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL,
    )?;
    let size = u32::try_from(bytes.len()).map_err(|_| io::Error::other("a keymap over 4 GiB"))?;
    Ok((file, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT_SHIFT: u32 = 42 + 8;
    const KEY_A: u32 = 30 + 8;
    const XKB_KEY_A: u32 = 0x41;
    const XKB_KEY_LOWER_A: u32 = 0x61;

    #[test]
    fn the_default_keymap_compiles_and_its_text_is_sealed() {
        let keyboard = Keyboard::new(&KeymapNames::default()).unwrap();
        let (file, size) = keyboard.keymap().unwrap();
        assert!(size > 1000);
        let mut start = [0u8; 16];
        rustix::io::pread(&file, &mut start, 0).unwrap();
        assert!(start.starts_with(b"xkb_keymap"));
        assert!(
            rustix::io::pwrite(&file, b"x", 0).is_err(),
            "a client could change the keymap"
        );
    }

    #[test]
    fn shift_changes_the_modifiers_and_the_keysym() {
        let mut keyboard = Keyboard::new(&KeymapNames::default()).unwrap();
        assert_eq!(keyboard.keysym(KEY_A), XKB_KEY_LOWER_A);
        assert!(keyboard.update(LEFT_SHIFT, true));
        assert_ne!(keyboard.modifiers().depressed, 0);
        assert_eq!(keyboard.keysym(KEY_A), XKB_KEY_A);
        assert!(keyboard.update(LEFT_SHIFT, false));
        assert_eq!(keyboard.modifiers(), Modifiers::default());
    }

    #[test]
    fn a_name_with_a_nul_is_refused() {
        let names = KeymapNames {
            layout: "u\0s".into(),
            ..Default::default()
        };
        assert!(Keyboard::new(&names).is_err());
    }
}
