//! Loading a C library at run time, and tables of its functions.
//!
//! EGL, GLES, and GBM are opened with `dlopen`, as Smithay opens EGL. A
//! machine without them runs Fulcrum on the CPU renderer instead, and
//! building Fulcrum needs none of their development files. A library that
//! was opened is never closed, so every function pointer taken from it stays
//! valid for the life of the process.

use std::ffi::{CStr, c_void};

/// A library opened with `dlopen`, never closed.
pub(crate) struct Library(*mut c_void);

// Safety: the handle is only passed to `dlsym`, which is thread-safe.
unsafe impl Send for Library {}
unsafe impl Sync for Library {}

impl Library {
    /// Opens the first of `names` that loads.
    pub(crate) fn open(names: &[&CStr]) -> Option<Library> {
        names.iter().find_map(|name| {
            // Safety: `name` is a NUL-terminated string.
            let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            (!handle.is_null()).then_some(Library(handle))
        })
    }

    /// The address of a symbol, or null.
    pub(crate) fn symbol(&self, name: &CStr) -> *mut c_void {
        // Safety: the handle came from `dlopen` and is never closed.
        unsafe { libc::dlsym(self.0, name.as_ptr()) }
    }
}

/// Declares a table of C functions and how to load it: each function is
/// looked up by its own name with `lookup`, and the table loads only if all
/// of them are found.
macro_rules! functions {
    ($(#[$meta:meta])* $table:ident { $( fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)?; )* }) => {
        $(#[$meta])*
        #[allow(non_snake_case)]
        pub(crate) struct $table {
            $( pub $name: unsafe extern "C" fn($($ty),*) $(-> $ret)?, )*
        }

        impl $table {
            /// Looks up every function. Fails with the name of the first
            /// that is missing.
            #[allow(non_snake_case)]
            pub(crate) fn load(lookup: impl Fn(&std::ffi::CStr) -> *mut std::ffi::c_void) -> Result<Self, &'static str> {
                $(
                    let $name = {
                        const NAME: &str = concat!(stringify!($name), "\0");
                        let name = std::ffi::CStr::from_bytes_with_nul(NAME.as_bytes()).expect("a NUL-terminated name");
                        let address = lookup(name);
                        if address.is_null() {
                            return Err(stringify!($name));
                        }
                        // Safety: the symbol of this name has this C signature,
                        // as its header declares.
                        unsafe {
                            std::mem::transmute::<*mut std::ffi::c_void, unsafe extern "C" fn($($ty),*) $(-> $ret)?>(address)
                        }
                    };
                )*
                Ok($table { $( $name, )* })
            }
        }
    };
}

pub(crate) use functions;
