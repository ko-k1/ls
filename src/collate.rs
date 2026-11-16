//! Locale-aware filename ordering.
//!
//! `strcoll` defines the order, but in glibc's UTF-8 locales it costs about
//! 100 ns per call and a sort makes `n log n` of them. Large sorts therefore
//! compare precomputed level-1 keys and call `strcoll` only for ties and for
//! names the keys cannot describe.
//!
//! glibc compares strings one level at a time: base characters first
//! (level 1), then accents, case and punctuation. The first level that
//! differs decides. In the locales accepted here, every printable ASCII
//! character either has a single level-1 weight or is ignored at level 1,
//! upper and lower case share a weight, and no ASCII sequence forms a
//! multi-character collation element. The level-1 sequence of an ASCII name
//! is therefore the weights of its characters with the ignored ones dropped,
//! and comparing two such keys lexicographically reproduces the level-1
//! result exactly. Equal keys mean the names differ only at later levels, so
//! those pairs go to `strcoll`.
//!
//! The weight table is not trusted blindly: before first use it is checked
//! against the running locale with about 190 `strcoll` probes, and keys are
//! used only if every probe agrees.

use std::cmp::Ordering;
use std::env;
use std::ffi::{CString, OsStr, OsString};
use std::mem::MaybeUninit;
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::ptr;
use std::sync::OnceLock;

extern "C" {
    fn strcoll(a: *const c_char, b: *const c_char) -> c_int;
    fn strxfrm(dest: *mut c_char, src: *const c_char, n: usize) -> usize;
}

/// Largest filename component on Unix. Keeping the C string on the stack
/// avoids two heap allocations per comparison for the common case.
const STACK_NAME: usize = 256;

/// Compare two names in the current locale's collation order.
pub fn collate(a: &OsStr, b: &OsStr) -> Ordering {
    if locale_is_bytewise() {
        a.as_bytes().cmp(b.as_bytes())
    } else {
        strcoll_bytes(a.as_bytes(), b.as_bytes())
    }
}

/// How sort keys are derived from names.
#[derive(Clone, Copy)]
pub enum KeyMode {
    /// The C/POSIX locales: the name bytes are the key.
    Bytes,
    /// Level-1 weights of ASCII characters (see the module documentation).
    Level1(&'static Level1Table),
    /// `strxfrm` output. Its byte order is meant to be `strcoll` order, but
    /// glibc does not always deliver that, so callers must verify the result
    /// with `strcoll` (see `sort::restore_collation_order`).
    Strxfrm,
}

impl KeyMode {
    /// Append the sort key for `name` to `out` and return `true`, or leave
    /// `out` unchanged and return `false` if the name contains characters the
    /// key cannot describe. Key bytes are never zero.
    pub fn push_key(self, name: &[u8], out: &mut Vec<u8>) -> bool {
        match self {
            Self::Bytes => {
                // Filenames cannot contain NUL, so these bytes are non-zero.
                out.extend_from_slice(name);
                true
            }
            Self::Strxfrm => push_strxfrm(name, out),
            Self::Level1(table) => {
                let start = out.len();
                for &byte in name {
                    match table.weights.get(usize::from(byte)) {
                        Some(&IGNORED) => {}
                        Some(&weight) if weight != NO_WEIGHT => out.push(weight),
                        _ => {
                            out.truncate(start);
                            return false;
                        }
                    }
                }
                true
            }
        }
    }

    /// Order two names, at least one without a key, by their first
    /// differing byte when that alone decides: the byte-identical prefix
    /// contributes identical weights to both names, so if both differing
    /// bytes are weighted ASCII characters with different weights, they
    /// decide level 1. Returns `None` when `strcoll` is needed.
    pub fn first_difference(self, a: &[u8], b: &[u8]) -> Option<Ordering> {
        let Self::Level1(table) = self else {
            return None;
        };
        let index = a.iter().zip(b).position(|(x, y)| x != y)?;
        let weight = |byte: u8| {
            let weight = *table.weights.get(usize::from(byte))?;
            (weight != IGNORED && weight != NO_WEIGHT).then_some(weight)
        };
        let (left, right) = (weight(a[index])?, weight(b[index])?);
        (left != right).then(|| left.cmp(&right))
    }

    /// Order two names whose keys are equal or missing.
    pub fn tie_break(self, a: &OsStr, b: &OsStr) -> Ordering {
        match self {
            Self::Bytes => a.as_bytes().cmp(b.as_bytes()),
            Self::Level1(_) | Self::Strxfrm => strcoll_bytes(a.as_bytes(), b.as_bytes()),
        }
    }
}

/// Append the `strxfrm` transformation of `name` to `out`. The output is a
/// C string, so key bytes are never zero.
fn push_strxfrm(name: &[u8], out: &mut Vec<u8>) -> bool {
    // Filename components are shorter than STACK_NAME (NAME_MAX is 255).
    if name.len() >= STACK_NAME {
        return false;
    }
    let mut source = [0u8; STACK_NAME];
    source[..name.len()].copy_from_slice(name);

    let start = out.len();
    let mut room = name.len() * 8 + 32;
    loop {
        out.reserve(room);
        let spare = out.spare_capacity_mut();
        // SAFETY: `source` is NUL-terminated and `spare` is writable for
        // `spare.len()` bytes. strxfrm writes at most that many bytes and
        // returns the full length; when it fits, that many bytes (plus a
        // NUL) were written, so extending the length over them is sound.
        let length = unsafe {
            strxfrm(
                spare.as_mut_ptr().cast(),
                source.as_ptr().cast(),
                spare.len(),
            )
        };
        if length < spare.len() {
            unsafe { out.set_len(start + length) };
            return true;
        }
        room = length + 1;
    }
}

/// Level-1 weight of each ASCII byte: [`IGNORED`], [`NO_WEIGHT`], or a rank
/// starting at 1.
pub struct Level1Table {
    weights: [u8; 128],
}

const IGNORED: u8 = 0;
const NO_WEIGHT: u8 = u8::MAX;

/// Printable ASCII characters that ISO 14651-based glibc locales ignore at
/// level 1. `/` cannot occur in a filename but is listed for completeness.
const IGNORED_CHARS: &[u8] = b" !\"#%&'()*+,-./:;<=>?@[\\]^_`{|}~";

/// Printable ASCII characters with a level-1 weight, in ascending order.
/// Upper-case letters share the weight of their lower-case form.
const WEIGHTED_CHARS: &[u8] = b"$0123456789abcdefghijklmnopqrstuvwxyz";

impl Level1Table {
    /// The table that ISO 14651-based glibc locales are expected to have.
    /// Control characters and DEL have no weight, so names containing them
    /// are always compared with `strcoll`.
    pub fn expected() -> Self {
        let mut weights = [NO_WEIGHT; 128];
        for &byte in IGNORED_CHARS {
            weights[usize::from(byte)] = IGNORED;
        }
        for (rank, &byte) in (1..).zip(WEIGHTED_CHARS) {
            weights[usize::from(byte)] = rank;
            weights[usize::from(byte.to_ascii_uppercase())] = rank;
        }
        Self { weights }
    }
}

static LEVEL1: OnceLock<Option<Level1Table>> = OnceLock::new();

/// The key mode for the current locale, verifying the level-1 table on
/// first use. `None` means names must be compared with [`collate`].
pub fn key_mode() -> Option<KeyMode> {
    if locale_is_bytewise() {
        return Some(KeyMode::Bytes);
    }
    LEVEL1
        .get_or_init(verified_level1_table)
        .as_ref()
        .map(KeyMode::Level1)
}

/// The level-1 key mode if an earlier [`key_mode`] call already verified
/// it. Never pays for verification.
pub fn verified_key_mode() -> Option<KeyMode> {
    LEVEL1.get()?.as_ref().map(KeyMode::Level1)
}

fn verified_level1_table() -> Option<Level1Table> {
    if !locale_has_simple_ascii_order() {
        return None;
    }
    let ignored = || IGNORED_CHARS.iter().all(|&byte| ignored_at_level1(byte));
    let ascending = || {
        WEIGHTED_CHARS
            .windows(2)
            .all(|pair| level1_order(pair[0], pair[1]) == Some(Ordering::Less))
    };
    let caseless = || {
        (b'a'..=b'z')
            .all(|byte| level1_order(byte, byte.to_ascii_uppercase()) == Some(Ordering::Equal))
    };
    (ignored() && ascending() && caseless()).then(Level1Table::expected)
}

/// `byte` is ignored at level 1 exactly when "b?c" sorts after "bb" (as "bc"
/// would) and "b?a" before it (as "ba" would). A weighted `byte` decides
/// both comparisons the same way.
fn ignored_at_level1(byte: u8) -> bool {
    strcoll_bytes(&[b'b', byte, b'c'], b"bb") == Ordering::Greater
        && strcoll_bytes(&[b'b', byte, b'a'], b"bb") == Ordering::Less
}

/// Level-1 order of two weighted characters. If they share a weight, the
/// following character decides each comparison, so "xb"/"ya" and "xa"/"yb"
/// disagree; otherwise `x` and `y` decide both the same way.
fn level1_order(x: u8, y: u8) -> Option<Ordering> {
    let first = strcoll_bytes(&[x, b'b'], &[y, b'a']);
    let second = strcoll_bytes(&[x, b'a'], &[y, b'b']);
    match (first, second) {
        (Ordering::Greater, Ordering::Less) => Some(Ordering::Equal),
        (first, second) if first == second && first != Ordering::Equal => Some(first),
        _ => None,
    }
}

fn locale_name() -> Option<OsString> {
    ["LC_ALL", "LC_COLLATE", "LANG"]
        .iter()
        .filter_map(env::var_os)
        .find(|value| !value.is_empty())
}

/// The C and C.UTF-8 locales compare names byte by byte.
fn locale_is_bytewise() -> bool {
    static BYTEWISE: OnceLock<bool> = OnceLock::new();
    *BYTEWISE.get_or_init(|| {
        let Some(name) = locale_name() else {
            // No locale variables are set: the C locale.
            return true;
        };
        let name = name.to_string_lossy();

        name == "C"
            || name == "POSIX"
            || name.eq_ignore_ascii_case("C.UTF-8")
            || name.eq_ignore_ascii_case("C.utf8")
            || name.eq_ignore_ascii_case("C.UTF8")
    })
}

/// Return whether the locale selected by `setlocale(LC_ALL, "")` is one
/// whose ASCII collation has no multi-character elements (unlike, say,
/// Czech "ch"), which the `strcoll` probes cannot detect.
///
/// POSIX selects the locale from `LC_ALL`, then `LC_COLLATE`, then `LANG`, so
/// reading the same variables avoids hard-coding platform-specific
/// `LC_COLLATE` numbers.
fn locale_has_simple_ascii_order() -> bool {
    let Some(name) = locale_name() else {
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

fn strcoll_bytes(a: &[u8], b: &[u8]) -> Ordering {
    // Filenames and argv entries cannot contain NUL, so there is no need to
    // scan both names before every libc call.
    if a.len() < STACK_NAME && b.len() < STACK_NAME {
        let mut a_buffer = MaybeUninit::<[u8; STACK_NAME]>::uninit();
        let mut b_buffer = MaybeUninit::<[u8; STACK_NAME]>::uninit();
        unsafe {
            let a_ptr = a_buffer.as_mut_ptr() as *mut u8;
            let b_ptr = b_buffer.as_mut_ptr() as *mut u8;
            ptr::copy_nonoverlapping(a.as_ptr(), a_ptr, a.len());
            ptr::copy_nonoverlapping(b.as_ptr(), b_ptr, b.len());
            a_ptr.add(a.len()).write(0);
            b_ptr.add(b.len()).write(0);
            return strcoll(a_ptr as *const c_char, b_ptr as *const c_char).cmp(&0);
        }
    }

    match (CString::new(a), CString::new(b)) {
        (Ok(a), Ok(b)) => unsafe { strcoll(a.as_ptr(), b.as_ptr()) }.cmp(&0),
        _ => a.cmp(b),
    }
}
