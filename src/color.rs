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

/// Wrap `text` in ANSI color escapes if the kind has a color assigned.
pub fn paint(kind: Kind, text: &str) -> String {
    match color_code(kind) {
        Some(code) => format!("\x1b[{}m{}\x1b[0m", code, text),
        None => text.to_string(),
    }
}
