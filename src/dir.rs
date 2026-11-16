use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsRawFd, OwnedFd, RawFd};
use std::path::Path;

use rustix::fs::{Mode, OFlags};

use crate::entry::EntryType;

/// A directory entry as returned by the kernel, before metadata is loaded.
/// The name lives in the names buffer passed to [`DirReader::read_all`];
/// 8 bytes per entry keeps large directories compact.
pub struct RawEntry {
    start: u32,
    len: u16,
    pub file_type: EntryType,
}

impl RawEntry {
    pub fn name<'n>(&self, names: &'n [u8]) -> &'n OsStr {
        let start = self.start as usize;
        OsStr::from_bytes(&names[start..start + usize::from(self.len)])
    }
}

/// Append `name` to `names` and record it, unless the buffer has outgrown
/// 32-bit offsets.
fn push(
    names: &mut Vec<u8>,
    entries: &mut Vec<RawEntry>,
    name: &[u8],
    file_type: EntryType,
) -> io::Result<()> {
    let start =
        u32::try_from(names.len()).map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
    // Filename components are at most 255 bytes (NAME_MAX).
    let len = u16::try_from(name.len()).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    names.extend_from_slice(name);
    entries.push(RawEntry {
        start,
        len,
        file_type,
    });
    Ok(())
}

/// An open directory whose entries are read into one shared names buffer.
///
/// Every name is copied exactly once, into a buffer that holds the whole
/// directory's names, so reading allocates per directory rather than per
/// entry. (`std::fs::ReadDir` allocates twice per entry, and rustix's `Dir`
/// allocates a `CString` per entry after scanning each name twice.)
pub struct DirReader {
    fd: OwnedFd,
}

impl DirReader {
    pub fn open(path: &Path) -> io::Result<Self> {
        let fd = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Self { fd })
    }

    /// File descriptor for the open directory, usable for `statat` calls on
    /// entries while the reader is alive.
    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Read every entry except `.` and `..` (and, with `skip_hidden`, every
    /// name starting with `.`), appending names to `names` and records to
    /// `entries`. On a read error, the entries read so far are kept.
    #[cfg(target_os = "linux")]
    pub fn read_all(
        &self,
        skip_hidden: bool,
        names: &mut Vec<u8>,
        entries: &mut Vec<RawEntry>,
    ) -> io::Result<()> {
        // Large enough that a 100k-entry directory takes ~50 getdents calls.
        const BUFFER: usize = 64 * 1024;
        let mut buffer: Vec<u8> = Vec::with_capacity(BUFFER);
        let mut dir = rustix::fs::RawDir::new(&self.fd, buffer.spare_capacity_mut());
        while let Some(entry) = dir.next() {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if !keep(name, skip_hidden) {
                continue;
            }
            push(
                names,
                entries,
                name,
                EntryType::from_rustix(entry.file_type()),
            )?;
        }
        Ok(())
    }

    /// Portable fallback for [`read_all`](Self::read_all) on non-Linux Unix.
    #[cfg(not(target_os = "linux"))]
    pub fn read_all(
        &self,
        skip_hidden: bool,
        names: &mut Vec<u8>,
        entries: &mut Vec<RawEntry>,
    ) -> io::Result<()> {
        let mut dir = rustix::fs::Dir::read_from(&self.fd)?;
        while let Some(entry) = dir.read() {
            let entry = entry?;
            let name = entry.file_name().to_bytes();
            if !keep(name, skip_hidden) {
                continue;
            }
            push(
                names,
                entries,
                name,
                EntryType::from_rustix(entry.file_type()),
            )?;
        }
        Ok(())
    }
}

fn keep(name: &[u8], skip_hidden: bool) -> bool {
    match name.first() {
        Some(b'.') => !skip_hidden && name != b"." && name != b"..",
        _ => true,
    }
}
