use std::fs::Metadata;
use std::os::unix::fs::{FileTypeExt, MetadataExt};

/// Render the 10-character Unix permission string (type + rwx + special bits) into `out`,
/// reusing `out`'s capacity across calls.
pub fn push_permissions(meta: &Metadata, out: &mut String) {
    out.clear();
    let ft = meta.file_type();
    let kind = if ft.is_dir() {
        'd'
    } else if ft.is_symlink() {
        'l'
    } else if ft.is_fifo() {
        'p'
    } else if ft.is_socket() {
        's'
    } else if ft.is_block_device() {
        'b'
    } else if ft.is_char_device() {
        'c'
    } else {
        '-'
    };

    let mode = meta.mode();
    out.push(kind);

    let bits = [
        (0o400, 'r'),
        (0o200, 'w'),
        (0o100, 'x'),
        (0o040, 'r'),
        (0o020, 'w'),
        (0o010, 'x'),
        (0o004, 'r'),
        (0o002, 'w'),
        (0o001, 'x'),
    ];

    for (index, (mask, character)) in bits.iter().enumerate() {
        let mut ch = if mode & mask != 0 { *character } else { '-' };

        match index {
            2 if mode & 0o4000 != 0 => {
                ch = if ch == 'x' { 's' } else { 'S' };
            }
            5 if mode & 0o2000 != 0 => {
                ch = if ch == 'x' { 's' } else { 'S' };
            }
            8 if mode & 0o1000 != 0 => {
                ch = if ch == 'x' { 't' } else { 'T' };
            }
            _ => {}
        }

        out.push(ch);
    }
}
