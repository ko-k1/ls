use crate::format::push_u64;

/// Format a byte count in a human-readable form (e.g. `1.5K`, `3.0G`).
#[allow(dead_code)] // used by the unit tests in src/main.rs
pub fn human_size(bytes: u64) -> String {
    let mut out = String::new();
    push_human_size(bytes, &mut out);
    out
}

/// Format a byte count into `out` in a human-readable form (e.g. `1.5K`, `3.0G`),
/// reusing `out`'s capacity across calls.
///
/// This follows gnulib's `human_readable` with GNU ls's options (powers of
/// 1024, round towards +infinity): values below 10 keep one decimal, larger
/// values are whole numbers, and a value that rounds up to 1024 moves to the
/// next unit (`1048575` is `1.0M`, not `1024K`). The arithmetic is exact
/// integer math, so it cannot drift from GNU the way float rounding does.
pub fn push_human_size(bytes: u64, out: &mut String) {
    const BASE: u64 = 1024;
    const UNITS: &[u8] = b"BKMGTPE";

    out.clear();
    if bytes < BASE {
        push_u64(out, bytes);
        return;
    }

    let mut amount = bytes;
    // The first discarded decimal digit, and a summary of everything below
    // it: 0 = exactly zero, 1 = below half, 2 = exactly half, 3 = above half.
    let mut tenths = 0;
    let mut rounding = 0;
    let mut exponent = 0;
    while amount >= BASE && exponent < UNITS.len() - 1 {
        let r10 = (amount % BASE) * 10 + tenths;
        let r2 = (r10 % BASE) * 2 + (rounding >> 1);
        amount /= BASE;
        tenths = r10 / BASE;
        rounding = if r2 < BASE {
            u64::from(r2 + rounding != 0)
        } else {
            2 + u64::from(BASE < r2 + rounding)
        };
        exponent += 1;
    }

    let mut decimal = None;
    if amount < 10 {
        if rounding > 0 {
            tenths += 1;
            rounding = 0;
            if tenths == 10 {
                amount += 1;
                tenths = 0;
            }
        }
        if amount < 10 {
            decimal = Some(tenths);
            tenths = 0;
            rounding = 0;
        }
    }

    if tenths + rounding > 0 {
        amount += 1;
        if amount == BASE && exponent < UNITS.len() - 1 {
            exponent += 1;
            amount = 1;
            decimal = Some(0);
        }
    }

    push_u64(out, amount);
    if let Some(digit) = decimal {
        out.push('.');
        out.push(char::from(b'0' + digit as u8));
    }
    out.push(char::from(UNITS[exponent]));
}

/// Format an allocated block count into `out`, reusing `out`'s capacity.
///
/// `blocks` is in 512-byte units. When `human` is set, the size is printed
/// like `-h` sizes; otherwise in 1024-byte units, rounded up like GNU ls.
pub fn push_format_blocks(blocks: u64, human: bool, out: &mut String) {
    out.clear();
    if human {
        push_human_size(blocks.saturating_mul(512), out);
    } else {
        push_u64(out, blocks.div_ceil(2));
    }
}

/// Format an allocated block count (see [`push_format_blocks`]).
pub fn format_blocks(blocks: u64, human: bool) -> String {
    let mut out = String::new();
    push_format_blocks(blocks, human, &mut out);
    out
}
