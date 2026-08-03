pub(crate) fn disk_usage_readable(bytes: u64) -> String {
    let mut size = bytes as f64;
    let mut unit = "B";
    for next in ["KB", "MB", "GB"] {
        if size < 1000.0 {
            break;
        }
        size /= 1000.0;
        unit = next;
    }

    if ((size * 10.0).trunc() as u64).is_multiple_of(10) {
        format!("{}{}", size as u64, unit)
    } else {
        format!("{size:.1}{unit}")
    }
}
