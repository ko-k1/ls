use std::cmp::{Ordering, Reverse};
use std::os::unix::ffi::OsStrExt;

use rayon::prelude::*;

use crate::args::{Options, SortKey};
use crate::collate::{self, KeyMode};
use crate::entry::Entry;

const PARALLEL_SORT_THRESHOLD: usize = 8 * 1024;

/// Smallest name sort that verifies the locale's level-1 table to use keys.
///
/// Verification costs about 190 `strcoll` calls (~60k instructions), once
/// per process. Measured under en_US.UTF-8, plain `strcoll` sorting is
/// cheaper below roughly 30 real-world names (40 synthetic mixed-case
/// names). Once any sort has paid for verification, sorts of every size use
/// keys.
const KEYED_MIN: usize = 32;

/// Marks a [`Keyed`] record whose name has no key.
const NO_KEY: u32 = u32::MAX;

pub fn sort_entries(entries: &mut Vec<Entry>, opts: &Options) {
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
        group_directories_first(entries);
    }
}

/// Move directories to the front, keeping the existing order within both
/// groups. This is a linear stable partition rather than a second sort.
fn group_directories_first(entries: &mut Vec<Entry>) {
    let others: Vec<Entry> = entries
        .extract_if(.., |entry| !entry.file_type.is_dir())
        .collect();
    entries.extend(others);
}

/// Sort entries by name in the locale's collation order.
///
/// Directory entry names are unique, so unstable sorts cannot change the
/// result. They also run in linear time on input that is already sorted or
/// reversed, which readdir often returns.
fn sort_names(entries: &mut [Entry]) {
    if entries.len() < 2 {
        return;
    }

    let mode = if entries.len() >= KEYED_MIN {
        collate::key_mode()
    } else {
        collate::verified_key_mode()
    };
    // Key offsets are 32-bit; absurdly large directories use plain sorting.
    let key_bytes: usize = entries.iter().map(|entry| entry.name.len()).sum();
    match mode {
        Some(mode) if key_bytes < NO_KEY as usize && entries.len() < NO_KEY as usize => {
            sort_keyed(entries, mode);
        }
        _ => sort_slice(entries, |a, b| collate::collate(&a.name, &b.name)),
    }
}

fn sort_slice<T: Send>(items: &mut [T], compare: impl Fn(&T, &T) -> Ordering + Sync) {
    if items.len() >= PARALLEL_SORT_THRESHOLD {
        items.par_sort_unstable_by(compare);
    } else {
        items.sort_unstable_by(compare);
    }
}

/// An entry's position and sort key. Sorting these moves 24 bytes per entry
/// instead of a whole [`Entry`].
#[derive(Clone, Copy)]
struct Keyed {
    /// The first eight key bytes, big-endian and zero-padded. Key bytes are
    /// never zero, so prefixes order keys correctly up to their eighth byte.
    prefix: u64,
    /// Location of the key in the shared key buffer; `len` is [`NO_KEY`]
    /// when the name has no key.
    start: u32,
    len: u32,
    /// Index of the entry this record stands for.
    index: u32,
}

fn sort_keyed(entries: &mut [Entry], mode: KeyMode) {
    let (keys, mut records) = build_keys(entries, mode);

    let names = &*entries;
    let key = |record: &Keyed| &keys[record.start as usize..][..record.len as usize];
    let name = |record: &Keyed| names[record.index as usize].name.as_os_str();
    let shortcut = |a: &Keyed, b: &Keyed| {
        if a.len != NO_KEY && b.len != NO_KEY {
            let ordering = a.prefix.cmp(&b.prefix).then_with(|| {
                // Equal prefixes mean the first `skip` bytes are equal.
                let skip = 8.min(a.len).min(b.len) as usize;
                key(a)[skip..].cmp(&key(b)[skip..])
            });
            (ordering != Ordering::Equal).then_some(ordering)
        } else {
            mode.first_difference(name(a).as_bytes(), name(b).as_bytes())
        }
    };
    let compare = |a: &Keyed, b: &Keyed| match shortcut(a, b) {
        Some(ordering) => {
            debug_assert_eq!(
                ordering,
                mode.tie_break(name(a), name(b)),
                "shortcut order disagrees with collation for {:?} and {:?}",
                name(a),
                name(b),
            );
            ordering
        }
        None => mode.tie_break(name(a), name(b)),
    };
    if records.len() >= PARALLEL_SORT_THRESHOLD {
        // rayon's parallel quicksort partitions the whole slice on one
        // thread before splitting; the merge sort sorts chunks and merges
        // them in parallel from the start. Records are small, so its scratch
        // buffer is cheap; stability itself does not matter here.
        records.par_sort_by(compare);
    } else {
        records.sort_unstable_by(compare);
    }

    let order: Vec<u32> = records.iter().map(|record| record.index).collect();
    apply_order(entries, &order);
}

/// Build every entry's key into one shared buffer, plus a record per entry.
/// Large directories build chunks in parallel and then concatenate them.
fn build_keys(entries: &[Entry], mode: KeyMode) -> (Vec<u8>, Vec<Keyed>) {
    const CHUNK: usize = 4096;

    let build = |first_index: usize, chunk: &[Entry]| {
        let mut keys = Vec::with_capacity(chunk.iter().map(|entry| entry.name.len()).sum());
        let records: Vec<Keyed> = (first_index as u32..)
            .zip(chunk)
            .map(|(index, entry)| {
                let start = keys.len();
                let has_key = mode.push_key(entry.name.as_bytes(), &mut keys);
                Keyed {
                    prefix: key_prefix(&keys[start..]),
                    start: start as u32,
                    len: if has_key {
                        (keys.len() - start) as u32
                    } else {
                        NO_KEY
                    },
                    index,
                }
            })
            .collect();
        (keys, records)
    };

    if entries.len() < PARALLEL_SORT_THRESHOLD {
        return build(0, entries);
    }

    let chunks: Vec<(Vec<u8>, Vec<Keyed>)> = entries
        .par_chunks(CHUNK)
        .enumerate()
        .map(|(chunk_index, chunk)| build(chunk_index * CHUNK, chunk))
        .collect();
    let mut keys = Vec::with_capacity(chunks.iter().map(|(keys, _)| keys.len()).sum());
    let mut records = Vec::with_capacity(entries.len());
    for (chunk_keys, chunk_records) in chunks {
        let base = keys.len() as u32;
        keys.extend_from_slice(&chunk_keys);
        records.extend(chunk_records.into_iter().map(|record| Keyed {
            start: record.start + base,
            ..record
        }));
    }
    (keys, records)
}

fn key_prefix(key: &[u8]) -> u64 {
    let mut bytes = [0u8; 8];
    let len = key.len().min(8);
    bytes[..len].copy_from_slice(&key[..len]);
    u64::from_be_bytes(bytes)
}

/// Rearrange `items` so that position `i` holds the item previously at
/// `order[i]`.
///
/// Each item is moved exactly once into a scratch buffer, in output order,
/// and the buffer is then copied back. Large slices gather in parallel.
///
/// # Panics
///
/// Panics if `order` is not a permutation of `0..items.len()`.
pub fn apply_order<T: Send>(items: &mut [T], order: &[u32]) {
    let len = items.len();
    let mut seen = vec![false; len];
    assert!(
        order.len() == len
            && order
                .iter()
                .all(|&index| !std::mem::replace(&mut seen[index as usize], true)),
        "apply_order needs a permutation"
    );

    /// Raw pointer that may be shared across rayon workers. Accessed only
    /// through `get` so closures capture the wrapper, not the pointer field.
    struct Shared<T>(*mut T);
    unsafe impl<T: Send> Sync for Shared<T> {}
    impl<T> Shared<T> {
        fn get(&self) -> *mut T {
            self.0
        }
    }

    let mut sorted: Vec<T> = Vec::with_capacity(len);
    let source = Shared(items.as_mut_ptr());
    let target = Shared(sorted.as_mut_ptr());
    // SAFETY: `order` is a permutation (checked above), so every source slot
    // is read exactly once and every target slot written exactly once. The
    // gathered items are then copied over the originals, all of which were
    // moved out. `sorted` keeps length 0, so nothing is dropped twice, and
    // no code between the reads and the copy-back can panic.
    let gather = |(position, &index): (usize, &u32)| unsafe {
        target
            .get()
            .add(position)
            .write(source.get().add(index as usize).read());
    };
    if len >= PARALLEL_SORT_THRESHOLD {
        order.par_iter().enumerate().for_each(gather);
    } else {
        order.iter().enumerate().for_each(gather);
    }
    unsafe { std::ptr::copy_nonoverlapping(sorted.as_ptr(), items.as_mut_ptr(), len) };
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
