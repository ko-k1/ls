use crate::entry::EntryType;

/// Render the 10-character Unix permission string (type + rwx + special bits) into `out`,
/// reusing `out`'s capacity across calls.
pub fn push_permissions(file_type: EntryType, mode: u32, out: &mut String) {
    out.clear();
    let kind = if file_type.is_dir() {
        'd'
    } else if file_type.is_symlink() {
        'l'
    } else if file_type.is_fifo() {
        'p'
    } else if file_type.is_socket() {
        's'
    } else if file_type.is_block_device() {
        'b'
    } else if file_type.is_char_device() {
        'c'
    } else {
        '-'
    };

    let mut buffer = [b'-'; 10];
    buffer[0] = kind as u8;

    const TRIPLETS: [&[u8; 3]; 8] = [
        b"---", b"--x", b"-w-", b"-wx", b"r--", b"r-x", b"rw-", b"rwx",
    ];

    for (index, shift) in [6, 3, 0].into_iter().enumerate() {
        let triplet = TRIPLETS[((mode >> shift) & 7) as usize];
        let start = 1 + index * 3;
        buffer[start] = triplet[0];
        buffer[start + 1] = triplet[1];
        buffer[start + 2] = triplet[2];
    }

    if mode & 0o4000 != 0 {
        buffer[3] = if buffer[3] == b'x' { b's' } else { b'S' };
    }
    if mode & 0o2000 != 0 {
        buffer[6] = if buffer[6] == b'x' { b's' } else { b'S' };
    }
    if mode & 0o1000 != 0 {
        buffer[9] = if buffer[9] == b'x' { b't' } else { b'T' };
    }

    out.push_str(unsafe { std::str::from_utf8_unchecked(&buffer) });
}
