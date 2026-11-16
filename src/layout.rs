use std::os::raw::c_int;

extern "C" {
    fn wcwidth(ch: u32) -> c_int;
}

/// A column layout: item `index` is in column `index / rows`, row
/// `index % rows`.
pub struct Layout {
    pub rows: usize,
    pub columns: usize,
    pub widths: Vec<usize>,
}

/// Compute the multi-column layout with the most columns that fits items of
/// display width `widths` within `max_width`, filling columns top to bottom.
/// Columns are separated by at least two spaces.
pub fn layout_columns(widths: &[usize], max_width: usize) -> Layout {
    let count = widths.len();
    if count == 0 {
        return Layout {
            rows: 0,
            columns: 0,
            widths: Vec::new(),
        };
    }

    let narrowest = widths.iter().copied().min().unwrap_or(0);
    let widest = widths.iter().copied().max().unwrap_or(0);
    let max_columns = max_width.div_ceil(3).max(1).min(count);
    let mut column_widths = Vec::with_capacity(max_columns);

    for columns in (1..=max_columns).rev() {
        let rows = count.div_ceil(columns);
        if count.div_ceil(rows) != columns {
            continue;
        }
        // Some column holds the widest item and every other column is at
        // least as wide as the narrowest one; skip counts that cannot fit.
        if columns > 1 && widest + (columns - 1) * (narrowest + 2) > max_width {
            continue;
        }

        // Measure column by column and stop as soon as the total overflows.
        column_widths.clear();
        let mut total = 0;
        let mut fits = true;
        for column in widths.chunks(rows) {
            let width = column.iter().copied().max().unwrap_or(0);
            total += width + if column_widths.is_empty() { 0 } else { 2 };
            column_widths.push(width);
            if total > max_width && columns > 1 {
                fits = false;
                break;
            }
        }
        if fits {
            return Layout {
                rows,
                columns,
                widths: column_widths,
            };
        }
    }

    // Unreachable in practice: a single column always fits.
    Layout {
        rows: count,
        columns: 1,
        widths: vec![widest],
    }
}

/// Terminal display width of `text`, as GNU ls computes it: ASCII
/// characters are one column; others use the C library's `wcwidth` for the
/// current locale (two for most CJK, zero for combining marks), counting
/// characters `wcwidth` rejects as one column. Callers pass quoted names,
/// so control characters have already been escaped.
pub fn display_width(text: &str) -> usize {
    if text.is_ascii() {
        return text.len();
    }
    text.chars()
        .map(|ch| {
            if ch.is_ascii() {
                1
            } else {
                // `wchar_t` holds Unicode scalar values on Linux and macOS.
                usize::try_from(unsafe { wcwidth(u32::from(ch)) }).unwrap_or(1)
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::os::raw::c_char;

    use super::*;

    /// The original layout algorithm: measure every column of every
    /// candidate count, most columns first.
    fn reference(widths: &[usize], max_width: usize) -> (usize, usize, Vec<usize>) {
        let count = widths.len();
        for columns in (1..=max_width.div_ceil(3).max(1).min(count)).rev() {
            let rows = count.div_ceil(columns);
            if count.div_ceil(rows) != columns {
                continue;
            }
            let mut column_widths = vec![0; columns];
            for (index, &width) in widths.iter().enumerate() {
                column_widths[index / rows] = column_widths[index / rows].max(width);
            }
            if column_widths.iter().sum::<usize>() + 2 * (columns - 1) <= max_width || columns == 1
            {
                return (rows, columns, column_widths);
            }
        }
        unreachable!()
    }

    #[test]
    fn layout_matches_reference_algorithm() {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move |bound: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % bound
        };
        for _ in 0..2000 {
            let count = 1 + next(300) as usize;
            let spread = 1 + next(60);
            let widths: Vec<usize> = (0..count).map(|_| 1 + next(spread) as usize).collect();
            let max_width = 10 + next(250) as usize;
            let layout = layout_columns(&widths, max_width);
            assert_eq!(
                (layout.rows, layout.columns, layout.widths),
                reference(&widths, max_width),
                "{widths:?} in {max_width}"
            );
        }
    }

    #[test]
    fn display_width_counts_terminal_columns() {
        extern "C" {
            fn setlocale(category: c_int, locale: *const c_char) -> *mut c_char;
        }
        const LC_CTYPE: c_int = 0;
        let utf8 = CString::new("C.UTF-8").unwrap();
        if unsafe { setlocale(LC_CTYPE, utf8.as_ptr()) }.is_null() {
            return;
        }
        assert_eq!(display_width("plain.txt"), 9);
        assert_eq!(display_width("資料"), 4);
        assert_eq!(display_width("写真_2024.jpg"), 13);
        assert_eq!(display_width("e\u{301}t"), 2); // combining acute accent
        assert_eq!(display_width("café"), 4);
        let c = CString::new("C").unwrap();
        unsafe { setlocale(LC_CTYPE, c.as_ptr()) };
    }
}
