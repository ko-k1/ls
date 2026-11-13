use std::ffi::OsString;
use std::fs::Metadata;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};

use rustix::fd::BorrowedFd;
use rustix::fs::{AtFlags, Stat};

use crate::args::{IndicatorStyle, Options, SortKey};
use crate::dir::RawEntry;

/// The semantic kind of a directory entry, as far as `ls` needs to know it.
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

/// File type, derived from `d_type` when available and from `st_mode` otherwise.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EntryType {
    Regular,
    Dir,
    Symlink,
    Fifo,
    Socket,
    BlockDevice,
    CharDevice,
    Unknown,
}

impl EntryType {
    pub fn from_mode(mode: u32) -> Self {
        match mode & 0o170000 {
            0o100000 => Self::Regular,
            0o040000 => Self::Dir,
            0o120000 => Self::Symlink,
            0o010000 => Self::Fifo,
            0o140000 => Self::Socket,
            0o060000 => Self::BlockDevice,
            0o020000 => Self::CharDevice,
            _ => Self::Unknown,
        }
    }

    pub fn from_rustix(file_type: rustix::fs::FileType) -> Self {
        use rustix::fs::FileType;
        match file_type {
            FileType::RegularFile => Self::Regular,
            FileType::Directory => Self::Dir,
            FileType::Symlink => Self::Symlink,
            FileType::Fifo => Self::Fifo,
            FileType::Socket => Self::Socket,
            FileType::BlockDevice => Self::BlockDevice,
            FileType::CharacterDevice => Self::CharDevice,
            FileType::Unknown => Self::Unknown,
        }
    }

    pub fn is_dir(self) -> bool {
        self == Self::Dir
    }

    pub fn is_symlink(self) -> bool {
        self == Self::Symlink
    }

    pub fn is_file(self) -> bool {
        self == Self::Regular
    }

    pub fn is_fifo(self) -> bool {
        self == Self::Fifo
    }

    pub fn is_socket(self) -> bool {
        self == Self::Socket
    }

    pub fn is_block_device(self) -> bool {
        self == Self::BlockDevice
    }

    pub fn is_char_device(self) -> bool {
        self == Self::CharDevice
    }
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

    fn from_stat(stat: &Stat) -> Self {
        Self {
            mode: stat.st_mode,
            uid: stat.st_uid,
            gid: stat.st_gid,
            nlink: stat.st_nlink as u64,
            size: stat.st_size as u64,
            ino: stat.st_ino,
            blocks: stat.st_blocks as u64,
            mtime: stat.st_mtime,
            mtime_nsec: stat.st_mtime_nsec as i64,
        }
    }
}

/// A single directory entry with its name, path, and (optionally loaded) metadata.
pub struct Entry {
    pub name: OsString,
    pub path: PathBuf,
    pub file_type: EntryType,
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
pub fn needs_metadata(opts: &Options, color: bool) -> bool {
    opts.long
        || opts.blocks
        || opts.inode
        || opts.sort == SortKey::Time
        || opts.sort == SortKey::Size
        || opts.indicator == IndicatorStyle::Classify
        || color
}

/// Returns `true` when the entry's file type is needed even without full metadata.
pub fn needs_type(opts: &Options, color: bool) -> bool {
    needs_metadata(opts, color)
        || opts.recursive
        || opts.dirs_first
        || opts.indicator != IndicatorStyle::None
}

fn stat_at(dir_fd: RawFd, name: &OsString, flags: AtFlags) -> io::Result<EntryMeta> {
    // The directory reader stays alive for the whole metadata-loading phase,
    // so the descriptor is valid for every call.
    let dir_fd = unsafe { BorrowedFd::borrow_raw(dir_fd) };
    let stat = rustix::fs::statat(dir_fd, name, flags)?;
    Ok(EntryMeta::from_stat(&stat))
}

/// Turn a raw directory entry into an `Entry`, loading metadata when requested.
/// Returning the path with the error lets the caller report it from the main
/// thread while metadata loading happens in parallel.
pub fn build_entry(
    dir_path: &Path,
    dir_fd: RawFd,
    raw: RawEntry,
    opts: &Options,
    color: bool,
) -> Result<Entry, (PathBuf, io::Error)> {
    let RawEntry { name, file_type: dirent_file_type } = raw;

    let metadata = if needs_metadata(opts, color) {
        match stat_at(dir_fd, &name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(meta) => Some(meta),
            Err(err) => return Err((dir_path.join(&name), err)),
        }
    } else {
        None
    };

    let file_type = match &metadata {
        Some(meta) => EntryType::from_mode(meta.mode),
        None if dirent_file_type == EntryType::Unknown && needs_type(opts, color) => {
            match stat_at(dir_fd, &name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(meta) => EntryType::from_mode(meta.mode),
                Err(err) => return Err((dir_path.join(&name), err)),
            }
        }
        None => dirent_file_type,
    };

    // Building the full path costs an allocation per entry. It is only needed
    // for recursive descent into directories and for resolving symlinks in
    // long/color output.
    let needs_path =
        (opts.recursive && file_type.is_dir()) || ((opts.long || color) && file_type.is_symlink());
    let entry_path = if needs_path {
        dir_path.join(&name)
    } else {
        PathBuf::new()
    };
    let target = if color && file_type.is_symlink() {
        stat_at(dir_fd, &name, AtFlags::empty())
            .ok()
            .map(|meta| kind_of(EntryType::from_mode(meta.mode), Some(meta.mode)))
    } else {
        None
    };

    Ok(Entry {
        name,
        path: entry_path,
        file_type,
        meta: metadata,
        target,
    })
}

pub fn kind_of(file_type: EntryType, mode: Option<u32>) -> Kind {
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
