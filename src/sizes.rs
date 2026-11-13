use std::fmt::Write as _;

fn push_decimal(out: &mut String, value: u64) {
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

    out.push_str(unsafe { std::str::from_utf8_unchecked(&buffer[start..]) });
}

/// Format a byte count in a human-readable form (e.g. `1.5K`, `3.0G`).
#[allow(dead_code)] // used by the unit tests in src/main.rs
pub fn human_size(bytes: u64) -> String {
    let mut out = String::new();
    push_human_size(bytes, &mut out);
    out
}

/// Format a byte count into `out` in a human-readable form (e.g. `1.5K`, `3.0G`),
/// reusing `out`'s capacity across calls.
pub fn push_human_size(bytes: u64, out: &mut String) {
    const UNITS: [&str; 7] = ["B", "K", "M", "G", "T", "P", "E"];

    out.clear();
    if bytes < 1024 {
        push_decimal(out, bytes);
        return;
    }

    let mut value = bytes as f64;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if value < 10.0 {
        let _ = write!(out, "{value:.1}{}", UNITS[unit]);
    } else {
        let _ = write!(out, "{value:.0}{}", UNITS[unit]);
    }
}

/// Format an allocated block count into `out`, reusing `out`'s capacity.
///
/// When `human` is set, sizes are expressed in 1024-byte units; otherwise in
/// 512-byte units (the traditional `ls -s` unit).
pub fn push_format_blocks(blocks: u64, human: bool, out: &mut String) {
    out.clear();
    if human {
        push_human_size(blocks.saturating_mul(512), out);
    } else {
        push_decimal(out, blocks / 2);
    }
}

/// Format an allocated block count.
///
/// When `human` is set, sizes are expressed in 1024-byte units; otherwise in
/// 512-byte units (the traditional `ls -s` unit).
pub fn format_blocks(blocks: u64, human: bool) -> String {
    let mut out = String::new();
    push_format_blocks(blocks, human, &mut out);
    out
}
