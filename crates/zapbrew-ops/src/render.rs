const GAP: usize = 2;

/// Render already-sorted ASCII package names in Homebrew's column-major layout.
pub(crate) fn columns(items: &[String], width: usize) -> String {
    if items.is_empty() {
        return String::new();
    }
    if width == 0 {
        return one_per_line(items);
    }

    let max_len = items.iter().map(String::len).max().unwrap_or_default();
    let mut cols = width.saturating_add(GAP) / max_len.saturating_add(GAP);
    if cols < 2 {
        return one_per_line(items);
    }

    let rows = items.len().div_ceil(cols);
    cols = items.len().div_ceil(rows);
    let col_width = width.saturating_add(GAP) / cols - GAP;
    let mut output = String::new();

    for row in 0..rows {
        let mut first = true;
        for index in (row..items.len()).step_by(rows) {
            if !first {
                output.push_str("  ");
            }
            first = false;
            output.push_str(&items[index]);
            if index + rows < items.len() {
                output.extend(std::iter::repeat_n(' ', col_width - items[index].len()));
            }
        }
        output.push('\n');
    }

    output
}

fn one_per_line(items: &[String]) -> String {
    let capacity = items.iter().map(|item| item.len() + 1).sum();
    let mut output = String::with_capacity(capacity);
    for item in items {
        output.push_str(item);
        output.push('\n');
    }
    output
}
