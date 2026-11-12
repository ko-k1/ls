pub struct DisplayItem {
    pub text: String,
    pub width: usize,
}

pub struct Layout {
    pub rows: Vec<Vec<(usize, usize)>>,
    pub widths: Vec<usize>,
}

/// Compute a multi-column layout that fits `items` within `max_width`.
pub fn layout_columns(items: &[DisplayItem], max_width: usize) -> Layout {
    let count = items.len();
    if count == 0 {
        return Layout {
            rows: Vec::new(),
            widths: Vec::new(),
        };
    }

    let max_columns = max_width.div_ceil(3).max(1).min(count);

    for columns in (1..=max_columns).rev() {
        let rows = count.div_ceil(columns);
        if count.div_ceil(rows) != columns {
            continue;
        }

        let mut widths = vec![0usize; columns];
        for (index, item) in items.iter().enumerate() {
            let column = index / rows;
            widths[column] = widths[column].max(item.width);
        }

        let total: usize = widths.iter().sum::<usize>() + 2 * (columns - 1);
        if total <= max_width || columns == 1 {
            let mut result = vec![Vec::new(); rows];
            for (row, cells) in result.iter_mut().enumerate() {
                for column in 0..widths.len() {
                    let index = column * rows + row;
                    if index < count {
                        cells.push((index, column));
                    }
                }
            }
            return Layout {
                rows: result,
                widths,
            };
        }
    }

    Layout {
        rows: (0..count).map(|index| vec![(index, 0)]).collect(),
        widths: vec![items.iter().map(|item| item.width).max().unwrap_or(1)],
    }
}

pub fn max_width<'a>(values: impl Iterator<Item = &'a str>) -> usize {
    values.map(|value| value.chars().count()).max().unwrap_or(1)
}

pub fn pad_right(value: &str, width: usize) -> String {
    let length = value.chars().count();
    let mut result = String::with_capacity(value.len() + width.saturating_sub(length));
    result.push_str(value);
    for _ in length..width {
        result.push(' ');
    }
    result
}
