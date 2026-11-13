/// Number of decimal digits needed to represent `value`.
pub fn decimal_width(value: u64) -> usize {
    value
        .checked_ilog10()
        .map_or(1, |digits| digits as usize + 1)
}

/// Append a decimal integer without going through the formatting machinery.
pub fn push_u64(out: &mut String, value: u64) {
    let mut buffer = [0u8; 20];
    let mut start = buffer.len();
    let mut value = value;

    loop {
        start -= 1;
        buffer[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }

    // Every byte written above is an ASCII digit.
    out.push_str(unsafe { std::str::from_utf8_unchecked(&buffer[start..]) });
}

/// Append a decimal integer to a byte buffer (used for piped inode numbers).
pub fn push_u64_bytes(out: &mut Vec<u8>, value: u64) {
    let mut buffer = [0u8; 20];
    let mut start = buffer.len();
    let mut value = value;

    loop {
        start -= 1;
        buffer[start] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }

    out.extend_from_slice(&buffer[start..]);
}

pub fn push_padded_u64(out: &mut String, value: u64, width: usize) {
    for _ in decimal_width(value)..width {
        out.push(' ');
    }
    push_u64(out, value);
}

/// Append `text` right-aligned, using an already known display width.
pub fn push_right_aligned_n(out: &mut String, text: &str, text_width: usize, width: usize) {
    for _ in text_width..width {
        out.push(' ');
    }
    out.push_str(text);
}

/// Append `text` left-aligned, using an already known display width.
pub fn push_left_aligned_n(out: &mut String, text: &str, text_width: usize, width: usize) {
    out.push_str(text);
    for _ in text_width..width {
        out.push(' ');
    }
}
