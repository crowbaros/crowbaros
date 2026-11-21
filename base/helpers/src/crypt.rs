//! Password hashing through libcrypt (libxcrypt), as /etc/shadow keeps it.
//!
//! The reentrant calls, `crypt_rn` and `crypt_gensalt_rn`, keep their work in
//! buffers of ours rather than libcrypt's static ones.

use std::ffi::{CStr, CString, c_char, c_int, c_ulong, c_void};

#[link(name = "crypt")]
unsafe extern "C" {
    fn crypt_rn(phrase: *const c_char, setting: *const c_char, data: *mut c_void, size: c_int) -> *mut c_char;
    fn crypt_gensalt_rn(
        prefix: *const c_char,
        count: c_ulong,
        rbytes: *const c_char,
        nrbytes: c_int,
        output: *mut c_char,
        output_size: c_int,
    ) -> *mut c_char;
}

/// Room for libxcrypt's `struct crypt_data`, which is 32 KiB; `crypt_rn`
/// refuses a buffer smaller than it and takes a larger one.
const CRYPT_DATA: usize = 64 << 10;
/// libxcrypt's CRYPT_GENSALT_OUTPUT_SIZE.
const GENSALT_OUTPUT: usize = 192;

/// The password hashed with `setting`, which is a salt or a whole stored
/// hash. None when libcrypt fails, or the password or setting holds a NUL.
pub fn crypt(password: &str, setting: &[u8]) -> Option<Vec<u8>> {
    let phrase = CString::new(password).ok()?;
    let setting = CString::new(setting).ok()?;
    let mut data = vec![0u8; CRYPT_DATA];
    // SAFETY: both strings end in NUL, and `data` is as large as we say.
    let out = unsafe {
        crypt_rn(
            phrase.as_ptr(),
            setting.as_ptr(),
            data.as_mut_ptr().cast(),
            CRYPT_DATA as c_int,
        )
    };
    if out.is_null() {
        return None;
    }
    // SAFETY: on success `out` is a NUL-ended string inside `data`.
    let hashed = unsafe { CStr::from_ptr(out) }.to_bytes().to_vec();
    data.fill(0);
    // libcrypt's failure token starts with `*`; no hash does.
    (!hashed.starts_with(b"*")).then_some(hashed)
}

/// A new SHA-512 hash of the password, with a fresh salt, as /etc/shadow
/// takes it.
pub fn hash(password: &str) -> Option<Vec<u8>> {
    let mut salt = [0u8; GENSALT_OUTPUT];
    // SAFETY: the prefix ends in NUL; with no random bytes given, libcrypt
    // draws its own; `salt` is as large as we say.
    let out = unsafe {
        crypt_gensalt_rn(
            c"$6$".as_ptr(),
            0,
            std::ptr::null(),
            0,
            salt.as_mut_ptr().cast(),
            GENSALT_OUTPUT as c_int,
        )
    };
    if out.is_null() {
        return None;
    }
    let salt = CStr::from_bytes_until_nul(&salt).ok()?.to_bytes();
    crypt(password, salt)
}

/// Whether the password hashes to `stored`, compared in constant time.
pub fn matches(password: &str, stored: &[u8]) -> bool {
    crypt(password, stored).is_some_and(|computed| same(&computed, stored))
}

/// Byte strings compared without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_matches_its_password_and_no_other() {
        let stored = hash("crowbar").unwrap();
        assert!(stored.starts_with(b"$6$"));
        assert!(matches("crowbar", &stored));
        assert!(!matches("wrong", &stored));
        assert!(!matches("", &stored));
    }
}
