use std::cmp::{Ordering, Reverse};
use std::ffi::OsStr;
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

/// Sort references to a directory's entries. The entries themselves stay in
/// readdir order and are never moved; sorting shuffles 8-byte references.
pub fn sort_entries(entries: &mut Vec<&Entry<'_>>, opts: &Options) {
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
fn group_directories_first(entries: &mut Vec<&Entry<'_>>) {
    let others: Vec<&Entry<'_>> = entries
        .extract_if(.., |entry| !entry.file_type.is_dir())
        .collect();
    entries.extend(others);
}

/// Sort entries by name in the locale's collation order.
///
/// Directory entry names are unique, so unstable sorts cannot change the
/// result. They also run in linear time on input that is already sorted or
/// reversed, which readdir often returns.
fn sort_names(entries: &mut [&Entry<'_>]) {
    if entries.len() < 2 {
        return;
    }

    let mode = if entries.len() >= KEYED_MIN {
        collate::key_mode()
    } else {
        collate::verified_key_mode()
    };
    // Non-ASCII names have no level-1 key, so comparisons involving them
    // mostly call strcoll. When that adds up, sort by strxfrm keys instead
    // (see `strxfrm_pays_off` and `restore_collation_order`).
    let mode = match mode {
        Some(KeyMode::Level1(_)) if strxfrm_pays_off(entries) => Some(KeyMode::Strxfrm),
        mode => mode,
    };
    // Key offsets are 32-bit; absurdly large directories use plain sorting.
    // strxfrm keys run to several times the name length, hence the margin.
    let key_bytes: usize = entries.iter().map(|entry| entry.name.len()).sum();
    let key_limit = match mode {
        Some(KeyMode::Strxfrm) => NO_KEY as usize / 32,
        _ => NO_KEY as usize,
    };
    match mode {
        Some(mode) if key_bytes < key_limit && entries.len() < NO_KEY as usize => {
            sort_keyed(entries, mode);
        }
        _ => sort_slice(entries, |a, b| collate::collate(a.name, b.name)),
    }
}

/// Whether sorting by strxfrm keys beats level-1 keys with strcoll.
///
/// With a fraction `f` of non-ASCII names, about `p = 1 - (1 - f)²` of
/// comparisons involve one and usually need strcoll, so level-1 sorting
/// costs about `p · log₂ n` strcoll calls per name. strxfrm keys plus the
/// strcoll check cost a roughly fixed amount per name, and their setup is
/// not worth it for small directories. Fitted to measurements under
/// en_US.UTF-8 (all-Japanese, half-Japanese and 5-40% mixes, 40 to 100k
/// names): strxfrm from 256 names when `p · log₂ n ≥ 3`.
fn strxfrm_pays_off(entries: &[&Entry<'_>]) -> bool {
    const MIN_NAMES: usize = 256;
    const MIN_STRCOLL_PER_NAME: f64 = 3.0;
    if entries.len() < MIN_NAMES {
        return false;
    }
    let len = entries.len() as f64;
    let non_ascii = entries
        .iter()
        .filter(|entry| !entry.name.as_bytes().is_ascii())
        .count();
    let fraction = non_ascii as f64 / len;
    let undecided = 1.0 - (1.0 - fraction) * (1.0 - fraction);
    // Integer log₂ is precise enough here, and f64::log2 would link libm,
    // whose loading costs every run ~50k instructions.
    undecided * f64::from(entries.len().ilog2()) >= MIN_STRCOLL_PER_NAME
}

fn sort_slice<T: Send>(items: &mut [T], compare: impl Fn(&T, &T) -> Ordering + Sync) {
    if items.len() >= PARALLEL_SORT_THRESHOLD {
        items.par_sort_unstable_by(compare);
    } else {
        items.sort_unstable_by(compare);
    }
}

/// An entry's position and sort key, kept compact so comparisons read the
/// key prefix without following a pointer.
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

fn sort_keyed(entries: &mut [&Entry<'_>], mode: KeyMode) {
    let (keys, mut records) = build_keys(entries, mode);

    let names = &*entries;
    let key = |record: &Keyed| &keys[record.start as usize..][..record.len as usize];
    let name = |record: &Keyed| names[record.index as usize].name;
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
    let strxfrm = matches!(mode, KeyMode::Strxfrm);
    let compare = |a: &Keyed, b: &Keyed| match shortcut(a, b) {
        Some(ordering) => {
            // glibc's strxfrm can disagree with strcoll (fixed up below).
            debug_assert!(
                strxfrm || ordering == mode.tie_break(name(a), name(b)),
                "key order disagrees with collation for {:?} and {:?}",
                name(a),
                name(b),
            );
            ordering
        }
        // With strxfrm keys, comparisons must not mix in strcoll: the two
        // may disagree, and a sort needs one consistent order.
        None if strxfrm => name(a).as_bytes().cmp(name(b).as_bytes()),
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

    if strxfrm {
        restore_collation_order(&mut records, name);
    }
    debug_assert!(
        records
            .windows(2)
            .all(|pair| mode.tie_break(name(&pair[0]), name(&pair[1])) != Ordering::Greater),
        "names are not in collation order"
    );

    let sorted: Vec<&Entry<'_>> = records
        .iter()
        .map(|record| entries[record.index as usize])
        .collect();
    entries.copy_from_slice(&sorted);
}

/// Build every entry's key into one shared buffer, plus a record per entry.
/// Large directories build chunks in parallel and then concatenate them.
fn build_keys(entries: &[&Entry<'_>], mode: KeyMode) -> (Vec<u8>, Vec<Keyed>) {
    const CHUNK: usize = 4096;

    let build = |first_index: usize, chunk: &[&Entry<'_>]| {
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

/// Make sure records sorted by strxfrm keys are in strcoll order.
///
/// POSIX promises that strxfrm keys order strings as strcoll does, but
/// glibc (2.43 at least) breaks that for some names: a randomized test found
/// about 30 disagreements per million pairs among names mixing punctuation
/// with non-ASCII letters. Checking adjacent pairs with strcoll costs `n`
/// calls (in parallel) instead of the `n log n` a strcoll sort needs, and if
/// every pair is in order, so is the whole sequence. Otherwise the records
/// are re-sorted by strcoll; the stable sort is adaptive, so input that is
/// nearly in order costs little more than the check.
fn restore_collation_order<'e>(records: &mut [Keyed], name: impl Fn(&Keyed) -> &'e OsStr + Sync) {
    let in_order =
        |pair: &[Keyed]| collate::collate(name(&pair[0]), name(&pair[1])) != Ordering::Greater;
    let sorted = if records.len() >= PARALLEL_SORT_THRESHOLD {
        records.par_windows(2).all(in_order)
    } else {
        records.windows(2).all(in_order)
    };
    if !sorted {
        records.sort_by(|a, b| collate::collate(name(a), name(b)));
    }
}

fn key_prefix(key: &[u8]) -> u64 {
    let mut bytes = [0u8; 8];
    let len = key.len().min(8);
    bytes[..len].copy_from_slice(&key[..len]);
    u64::from_be_bytes(bytes)
}

/// Sort by a descending primary key, then by name within equal-key runs.
fn sort_by_primary_then_names<K: Ord + Send>(
    entries: &mut [&Entry<'_>],
    key: impl Fn(&Entry<'_>) -> K + Sync,
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
        let primary = key(entries[start]);
        let mut end = start + 1;
        while end < entries.len() && key(entries[end]) == primary {
            end += 1;
        }
        if end - start > 1 {
            sort_names(&mut entries[start..end]);
        }
        start = end;
    }
}
