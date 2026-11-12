use std::fmt::Write as _;

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
        let _ = write!(out, "{bytes}");
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
        let _ = write!(out, "{}", blocks / 2);
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
