use std::cmp::Ordering;
use std::ffi::{CString, OsStr};
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;

extern "C" {
    fn strcoll(a: *const c_char, b: *const c_char) -> c_int;
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
