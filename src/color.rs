use crate::entry::Kind;

/// ANSI color code for a file kind, or `None` for default coloring.
pub fn color_code(kind: Kind) -> Option<&'static str> {
    match kind {
        Kind::Regular => None,
        Kind::Dir => Some("1;34"),
        Kind::Link => Some("36"),
        Kind::Broken => Some("1;31"),
        Kind::Executable => Some("1;32"),
        Kind::Fifo => Some("33"),
        Kind::Socket => Some("1;35"),
        Kind::Device => Some("1;33"),
    }
}

/// Append `text` to `out`, wrapped in ANSI color escapes when the kind has a
/// color assigned. Writing directly avoids the temporary `String` that a
/// returning `paint()` helper would need.
pub fn push_paint(out: &mut String, kind: Kind, text: &str) {
    match color_code(kind) {
        Some(code) => {
            out.push_str("\x1b[");
            out.push_str(code);
            out.push('m');
            out.push_str(text);
            out.push_str("\x1b[0m");
        }
        None => out.push_str(text),
    }
}
