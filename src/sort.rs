use std::cmp::{Ordering, Reverse};
use std::env;
use std::ffi::{CString, OsStr};
use std::mem::MaybeUninit;
use std::os::raw::{c_char, c_int};
use std::os::unix::ffi::OsStrExt;
use std::ptr;
use std::sync::OnceLock;

use rayon::prelude::*;

use crate::args::{Options, SortKey};
use crate::entry::Entry;

extern "C" {
    fn strcoll(a: *const c_char, b: *const c_char) -> c_int;
}

/// Largest filename component on Unix. Keeping the C string on the stack
/// avoids two heap allocations per comparison for the common case.
const STACK_NAME: usize = 256;

const PARALLEL_SORT_THRESHOLD: usize = 8 * 1024;

/// Locale-aware comparison of two entry names.
///
/// Most real-world filenames are made up of lowercase ASCII letters and digits,
/// for which byte order matches collation order in the common locales. Resolve
/// those comparisons directly and only call into libc when the first differing
/// byte falls outside that alphabet.
pub fn collate_names(a: &OsStr, b: &OsStr) -> Ordering {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    // In the C/POSIX locales, collation is plain byte order for every name.
    if ascii_order().bytewise_all {
        return a_bytes.cmp(b_bytes);
    }

    let common = a_bytes.len().min(b_bytes.len());

    let mut index = 0;
    while index < common && a_bytes[index] == b_bytes[index] {
        index += 1;
    }

    if index == common {
        if a_bytes.len() == b_bytes.len() {
            return Ordering::Equal;
        }

        // Locale collation is not required to preserve prefix order (e.g. a
        // name sorting after its own extension). Probe the current locale once
        // for a few representative bases; if any probe fails, disable the
        // prefix shortcut entirely rather than guessing.
        let next = if a_bytes.len() == common {
            b_bytes[index]
        } else {
            a_bytes[index]
        };
        if next.is_ascii() && ascii_order().prefix_sorts_first {
            return a_bytes.len().cmp(&b_bytes.len());
        }
    } else {
        let left = a_bytes[index];
        let right = b_bytes[index];
        if is_simple(left) && is_simple(right) && ascii_order().simple_is_bytewise {
            return left.cmp(&right);
        }
    }

    collate_slow(a, b)
}

fn is_simple(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit()
}

struct AsciiOrder {
    /// The locale sorts every name by byte value.
    bytewise_all: bool,
    /// `[0-9a-z]` compare in byte order.
    simple_is_bytewise: bool,
    /// A name sorts before any ASCII extension of itself.
    prefix_sorts_first: bool,
}

fn ascii_order() -> &'static AsciiOrder {
    static ORDER: OnceLock<AsciiOrder> = OnceLock::new();
    ORDER.get_or_init(|| {
        if locale_is_bytewise() {
            return AsciiOrder {
                bytewise_all: true,
                simple_is_bytewise: true,
                prefix_sorts_first: true,
            };
        }

        // Locales with multi-character collation elements (for example Czech
        // "ch") do not compose per-character order. Only probe locales where
        // ASCII order is plausibly stable.
        if !locale_has_simple_ascii_order() {
            return AsciiOrder {
                bytewise_all: false,
                simple_is_bytewise: false,
                prefix_sorts_first: false,
            };
        }

        const SIMPLE: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let simple_is_bytewise = SIMPLE.windows(2).all(|pair| {
            let a = OsStr::from_bytes(&pair[..1]);
            let b = OsStr::from_bytes(&pair[1..]);
            collate_slow(a, b) == Ordering::Less
        });

        const PREFIX_BASES: &[&[u8]] = &[b"", b"a", b"0", b"file"];
        let mut joined = [0u8; 8];
        let prefix_sorts_first = PREFIX_BASES.iter().all(|base| {
            (0x20u8..0x7f).all(|byte| {
                joined[..base.len()].copy_from_slice(base);
                joined[base.len()] = byte;
                collate_slow(
                    OsStr::from_bytes(base),
                    OsStr::from_bytes(&joined[..=base.len()]),
                ) == Ordering::Less
            })
        });

        AsciiOrder {
            bytewise_all: false,
            simple_is_bytewise,
            prefix_sorts_first,
        }
    })
}

fn locale_name() -> Option<std::ffi::OsString> {
    ["LC_ALL", "LC_COLLATE", "LANG"]
        .iter()
        .filter_map(env::var_os)
        .find(|value| !value.is_empty())
}

/// The C and C.UTF-8 locales compare names byte by byte.
fn locale_is_bytewise() -> bool {
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
}

/// Return whether the locale selected by `setlocale(LC_ALL, "")` uses plain
/// ASCII order.
///
/// POSIX selects the locale from `LC_ALL`, then `LC_COLLATE`, then `LANG`, so
/// reading the same variables avoids hard-coding platform-specific
/// `LC_COLLATE` numbers. The `strcoll` probes above then verify the actual
/// ordering and catch locales with unusual collation rules.
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

fn collate_slow(a: &OsStr, b: &OsStr) -> Ordering {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();

    // Filenames and argv entries cannot contain NUL, so there is no need to
    // scan both names before every libc call.
    if a_bytes.len() < STACK_NAME && b_bytes.len() < STACK_NAME {
        let mut a_buffer = MaybeUninit::<[u8; STACK_NAME]>::uninit();
        let mut b_buffer = MaybeUninit::<[u8; STACK_NAME]>::uninit();
        unsafe {
            let a_ptr = a_buffer.as_mut_ptr() as *mut u8;
            let b_ptr = b_buffer.as_mut_ptr() as *mut u8;
            ptr::copy_nonoverlapping(a_bytes.as_ptr(), a_ptr, a_bytes.len());
            ptr::copy_nonoverlapping(b_bytes.as_ptr(), b_ptr, b_bytes.len());
            a_ptr.add(a_bytes.len()).write(0);
            b_ptr.add(b_bytes.len()).write(0);
            return strcoll(a_ptr as *const c_char, b_ptr as *const c_char).cmp(&0);
        }
    }

    match (CString::new(a_bytes), CString::new(b_bytes)) {
        (Ok(a), Ok(b)) => unsafe { strcoll(a.as_ptr(), b.as_ptr()) }.cmp(&0),
        _ => a_bytes.cmp(b_bytes),
    }
}

pub fn sort_entries(entries: &mut [Entry], opts: &Options) {
    match opts.sort {
        SortKey::None => {}
        SortKey::Name => sort_names(entries),
        SortKey::Time => {
            sort_by_primary_then_names(entries, |entry| {
                (entry.metadata().mtime, entry.metadata().mtime_nsec)
            });
        }
        SortKey::Size => {
            sort_by_primary_then_names(entries, |entry| entry.metadata().size);
        }
    }

    if opts.reverse {
        entries.reverse();
    }

    if opts.dirs_first && opts.sort != SortKey::None {
        entries.sort_by_key(|entry| !entry.file_type.is_dir());
    }
}

fn sort_names(entries: &mut [Entry]) {
    let pairs = entries.len().saturating_sub(1);
    if pairs == 0 {
        return;
    }

    // Sample adjacent pairs first. Readdir commonly returns names in nearly
    // sorted or reverse-sorted order, so avoid a full sort when a linear scan
    // can confirm the order instead.
    const SAMPLE: usize = 256;
    let probe = pairs.min(SAMPLE);
    let mut inversions = 0;
    let mut equal = 0;
    for pair in entries.windows(2).take(probe) {
        match collate_names(pair[0].name.as_os_str(), pair[1].name.as_os_str()) {
            Ordering::Greater => inversions += 1,
            Ordering::Equal => equal += 1,
            Ordering::Less => {}
        }
    }

    if probe == pairs {
        if inversions == 0 {
            // Already in collation order.
            return;
        }
        if equal == 0 && inversions == pairs {
            entries.reverse();
            return;
        }
    }

    // The sample suggests a monotonic run. Verifying the whole slice is still
    // cheaper than sorting it, and on large directories it can be done in
    // parallel.
    if equal == 0 && inversions * 4 < probe {
        if is_sorted(entries, false) {
            return;
        }
    } else if equal == 0 && inversions * 4 > probe * 3 && is_sorted(entries, true) {
        entries.reverse();
        return;
    }

    // Directory entry names are unique, so an unstable sort cannot change the
    // result. It avoids the merge buffer and copying that a stable sort needs.
    if entries.len() >= PARALLEL_SORT_THRESHOLD {
        entries.par_sort_unstable_by(|a, b| collate_names(a.name.as_os_str(), b.name.as_os_str()));
    } else {
        entries.sort_unstable_by(|a, b| collate_names(a.name.as_os_str(), b.name.as_os_str()));
    }
}

/// Check whether the entries are in ascending (or strictly descending) name
/// order, using the locale-aware comparator.
fn is_sorted(entries: &[Entry], descending: bool) -> bool {
    let pairs = entries.len().saturating_sub(1);

    if entries.len() >= PARALLEL_SORT_THRESHOLD {
        (0..pairs).into_par_iter().all(|index| {
            let ordering = collate_names(
                entries[index].name.as_os_str(),
                entries[index + 1].name.as_os_str(),
            );
            if descending {
                ordering == Ordering::Greater
            } else {
                ordering != Ordering::Greater
            }
        })
    } else {
        entries.windows(2).all(|pair| {
            let ordering = collate_names(pair[0].name.as_os_str(), pair[1].name.as_os_str());
            if descending {
                ordering == Ordering::Greater
            } else {
                ordering != Ordering::Greater
            }
        })
    }
}

/// Sort by a descending primary key, then by name within equal-key runs.
fn sort_by_primary_then_names<K: Ord + Send>(
    entries: &mut [Entry],
    key: impl Fn(&Entry) -> K + Sync,
) {
    // Equal primary keys are re-sorted by name below, so this sort does not
    // need to preserve readdir order.
    if entries.len() >= PARALLEL_SORT_THRESHOLD {
        entries.par_sort_unstable_by_key(|entry| Reverse(key(entry)));
    } else {
        entries.sort_unstable_by_key(|entry| Reverse(key(entry)));
    }

    let mut start = 0;
    while start < entries.len() {
        let primary = key(&entries[start]);
        let mut end = start + 1;
        while end < entries.len() && key(&entries[end]) == primary {
            end += 1;
        }
        if end - start > 1 {
            sort_names(&mut entries[start..end]);
        }
        start = end;
    }
}
