use std::cmp::Ordering;
use std::ffi::{CString, OsStr};
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;

extern "C" {
    fn strcoll(a: *const c_char, b: *const c_char) -> c_int;
    fn strxfrm(dest: *mut c_char, src: *const c_char, n: usize) -> usize;
}

/// Locale-aware comparison of two entry names.
pub fn collate_names(a: &OsStr, b: &OsStr) -> Ordering {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    match (CString::new(a_bytes), CString::new(b_bytes)) {
        (Ok(a), Ok(b)) => unsafe { strcoll(a.as_ptr(), b.as_ptr()) }.cmp(&0),
        _ => a_bytes.cmp(b_bytes),
    }
}

/// Precompute a locale collation key for `name`.
///
/// Keys compare with ordinary byte order and produce the same ordering as
/// repeated `strcoll` calls, but without allocating for each comparison.
pub fn collate_key(name: &OsStr) -> Vec<u8> {
    let bytes = name.as_bytes();
    let Ok(c_name) = CString::new(bytes) else {
        return bytes.to_vec();
    };

    let mut buffer = [0u8; 256];
    let len = unsafe {
        strxfrm(
            buffer.as_mut_ptr() as *mut c_char,
            c_name.as_ptr(),
            buffer.len(),
        )
    };
    if len == usize::MAX {
        return bytes.to_vec();
    }

    if len < buffer.len() {
        return buffer[..len].to_vec();
    }

    let mut key = vec![0u8; len + 1];
    let written = unsafe { strxfrm(key.as_mut_ptr() as *mut c_char, c_name.as_ptr(), key.len()) };
    if written == usize::MAX || written > len {
        return bytes.to_vec();
    }
    key.truncate(written);
    key
}
