/// Format a byte count in a human-readable form (e.g. `1.5K`, `3.0G`).
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "K", "M", "G", "T", "P", "E"];

    if bytes < 1024 {
        return bytes.to_string();
    }

    let mut value = bytes as f64;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if value < 10.0 {
        format!("{:.1}{}", value, UNITS[unit])
    } else {
        format!("{:.0}{}", value, UNITS[unit])
    }
}

/// Format an allocated block count.
///
/// When `human` is set, sizes are expressed in 1024-byte units; otherwise in
/// 512-byte units (the traditional `ls -s` unit).
pub fn format_blocks(blocks: u64, human: bool) -> String {
    if human {
        human_size(blocks.saturating_mul(512))
    } else {
        (blocks / 2).to_string()
    }
}
