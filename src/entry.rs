use std::fs::{FileType, Metadata};
use std::os::unix::fs::{FileTypeExt, MetadataExt};

use crate::args::{IndicatorStyle, Options, SortKey};

/// The semantic kind of a directory entry, used for sorting, indicators, and color.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Regular,
    Dir,
    Link,
    Broken,
    Executable,
    Fifo,
    Socket,
    Device,
}

/// The metadata fields `ls` actually displays, stored compactly.
///
/// Keeping the full libc `Metadata` would make `Entry` more than twice as large
/// and turn sorting into a memory-bandwidth exercise.
#[derive(Clone, Copy)]
pub struct EntryMeta {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u64,
    pub size: u64,
    pub ino: u64,
    pub blocks: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
}

impl EntryMeta {
    pub fn from_metadata(meta: &Metadata) -> Self {
        Self {
            mode: meta.mode(),
            uid: meta.uid(),
            gid: meta.gid(),
            nlink: meta.nlink(),
            size: meta.size(),
            ino: meta.ino(),
            blocks: meta.blocks(),
            mtime: meta.mtime(),
            mtime_nsec: meta.mtime_nsec(),
        }
    }
}

/// A single directory entry with its name, path, and (optionally loaded) metadata.
pub struct Entry {
    pub name: std::ffi::OsString,
    pub path: std::path::PathBuf,
    pub file_type: FileType,
    pub meta: Option<EntryMeta>,
    /// Kind of the symlink target, when color output required resolving it.
    pub target: Option<Kind>,
}

impl Entry {
    pub fn metadata(&self) -> &EntryMeta {
        self.meta
            .as_ref()
            .expect("metadata was not loaded for this entry")
    }
}

/// Returns `true` when the entry's metadata must be loaded.
pub fn needs_metadata(opts: &Options) -> bool {
    opts.long
        || opts.blocks
        || opts.inode
        || opts.sort == SortKey::Time
        || opts.sort == SortKey::Size
        || opts.indicator == IndicatorStyle::Classify
}

pub fn kind_of(file_type: FileType, mode: Option<u32>) -> Kind {
    if file_type.is_dir() {
        Kind::Dir
    } else if file_type.is_symlink() {
        Kind::Link
    } else if file_type.is_fifo() {
        Kind::Fifo
    } else if file_type.is_socket() {
        Kind::Socket
    } else if file_type.is_block_device() || file_type.is_char_device() {
        Kind::Device
    } else if file_type.is_file() && mode.is_some_and(|m| m & 0o111 != 0) {
        Kind::Executable
    } else {
        Kind::Regular
    }
}

pub fn entry_kind(entry: &Entry) -> Kind {
    if entry.file_type.is_symlink() {
        if entry.target.is_some() {
            Kind::Link
        } else {
            Kind::Broken
        }
    } else {
        kind_of(entry.file_type, entry.meta.as_ref().map(|m| m.mode))
    }
}
