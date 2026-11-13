use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::path::Path;

use rustix::fs::{Dir, Mode, OFlags};

use crate::entry::EntryType;

/// A directory entry as returned by the kernel, before metadata is loaded.
pub struct RawEntry {
    pub name: OsString,
    pub file_type: EntryType,
}

/// Directory iterator backed by `getdents`, with one name allocation per entry.
///
/// `std::fs::ReadDir` keeps a second heap allocation (plus a shared reference
/// count) alive in every `DirEntry`. On large directories that allocator traffic
/// outweighs the actual directory read, so this reader copies each name once
/// and otherwise reuses a single kernel buffer.
pub struct DirReader {
    dir: Dir,
}

impl DirReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        let fd = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Self { dir: Dir::new(fd)? })
    }

    /// File descriptor for the open directory, usable for `statat` calls on
    /// entries while the reader is alive.
    pub fn fd(&self) -> RawFd {
        self.dir
            .fd()
            .expect("directory descriptor is valid while the reader is open")
            .as_raw_fd()
    }

    /// Return the next entry, skipping the `.` and `..` entries that `readdir`
    /// includes for every directory.
    pub fn next_entry(&mut self) -> Option<io::Result<RawEntry>> {
        loop {
            let entry = match self.dir.read() {
                Some(Ok(entry)) => entry,
                Some(Err(err)) => return Some(Err(err.into())),
                None => return None,
            };

            let name = entry.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }

            return Some(Ok(RawEntry {
                name: OsString::from_vec(name.to_vec()),
                file_type: EntryType::from_rustix(entry.file_type()),
            }));
        }
    }
}
