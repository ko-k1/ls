use std::cmp::Ordering;
use std::env;
use std::ffi::{CString, OsStr};
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::sync::OnceLock;

extern "C" {
    fn strcoll(a: *const c_char, b: *const c_char) -> c_int;
}

/// Largest filename component on Unix. Keeping the C string on the stack
/// avoids two heap allocations per comparison for the common case.
const STACK_NAME: usize = 256;

/// Locale-aware comparison of two entry names.
///
/// Most real-world filenames are made up of lowercase ASCII letters and digits,
/// for which byte order matches collation order in the common locales. Resolve
/// those comparisons directly and only call into libc when the first differing
/// byte falls outside that alphabet.
pub fn collate_names(a: &OsStr, b: &OsStr) -> Ordering {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    let common = a_bytes.len().min(b_bytes.len());

    let mut index = 0;
    while index < common && a_bytes[index] == b_bytes[index] {
        index += 1;
    }

    if index == common {
        if a_bytes.len() == b_bytes.len() {
            return Ordering::Equal;
        }
        // Prefix relationships are left to the locale implementation.
    } else {
        let left = a_bytes[index];
        let right = b_bytes[index];
        if is_simple(left) && is_simple(right) && simple_order_is_bytewise() {
            return left.cmp(&right);
        }
    }

    collate_slow(a, b)
}

fn is_simple(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

/// Check once that the current locale sorts `[0-9a-z]` in byte order.
fn simple_order_is_bytewise() -> bool {
    static BYTEWISE: OnceLock<bool> = OnceLock::new();
    *BYTEWISE.get_or_init(|| {
        // Locales with multi-character collation elements (for example Czech
        // "ch") do not compose per-character order. Only take the fast path
        // for the C/POSIX and English locales, where ASCII order is stable.
        if !locale_has_simple_ascii_order() {
            return false;
        }

        const SIMPLE: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        SIMPLE.windows(2).all(|pair| {
            let a = OsStr::from_bytes(&pair[..1]);
            let b = OsStr::from_bytes(&pair[1..]);
            collate_slow(a, b) == Ordering::Less
        })
    })
}

/// Return whether the locale selected by `setlocale(LC_ALL, "")` uses plain
/// ASCII order.
///
/// POSIX selects the locale from `LC_ALL`, then `LC_COLLATE`, then `LANG`, so
/// reading the same variables avoids hard-coding platform-specific
/// `LC_COLLATE` numbers. The `strcoll` probe in `simple_order_is_bytewise`
/// then verifies the actual ordering and catches locales with multi-character
/// collation elements (for example Czech "ch").
fn locale_has_simple_ascii_order() -> bool {
    let name = ["LC_ALL", "LC_COLLATE", "LANG"]
        .iter()
        .filter_map(env::var_os)
        .find(|value| !value.is_empty());

    let Some(name) = name else {
        // No locale variables are set: the C locale.
        return true;
    };
    let name = name.to_string_lossy();

    name == "C"
        || name == "POSIX"
        || name == "en"
        || name.starts_with("C.")
        || name.starts_with("en_")
        || name.starts_with("en.")
        || name.starts_with("en@")
}

fn collate_slow(a: &OsStr, b: &OsStr) -> Ordering {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    if a_bytes.contains(&0) || b_bytes.contains(&0) {
        return a_bytes.cmp(b_bytes);
    }

    if a_bytes.len() < STACK_NAME && b_bytes.len() < STACK_NAME {
        let mut a_buffer = [0u8; STACK_NAME];
        let mut b_buffer = [0u8; STACK_NAME];
        a_buffer[..a_bytes.len()].copy_from_slice(a_bytes);
        b_buffer[..b_bytes.len()].copy_from_slice(b_bytes);
        return unsafe {
            strcoll(
                a_buffer.as_ptr() as *const c_char,
                b_buffer.as_ptr() as *const c_char,
            )
        }
        .cmp(&0);
    }

    match (CString::new(a_bytes), CString::new(b_bytes)) {
        (Ok(a), Ok(b)) => unsafe { strcoll(a.as_ptr(), b.as_ptr()) }.cmp(&0),
        _ => a_bytes.cmp(b_bytes),
    }
}
