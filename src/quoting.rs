use std::borrow::Cow;

/// Shell-quote a filename for terminal display, if needed.
///
/// Non-terminal output simply replaces newlines with `?` to preserve record
/// boundaries. Returns a borrowed slice when no quoting is needed.
pub fn quote_name<'a>(name: &'a str, tty: bool) -> Cow<'a, str> {
    if !tty {
        if !name.contains('\n') {
            return Cow::Borrowed(name);
        }
        return Cow::Owned(name.replace('\n', "?"));
    }

    if name.is_empty() {
        return Cow::Owned("''".to_string());
    }

    if !needs_shell_quoting(name) {
        return Cow::Borrowed(name);
    }

    if name.chars().any(char::is_control) {
        let mut result = String::from("$'");

        for ch in name.chars() {
            match ch {
                '\x07' => result.push_str("\\a"),
                '\x08' => result.push_str("\\b"),
                '\t' => result.push_str("\\t"),
                '\n' => result.push_str("\\n"),
                '\x0b' => result.push_str("\\v"),
                '\x0c' => result.push_str("\\f"),
                '\r' => result.push_str("\\r"),
                '\\' => result.push_str("\\\\"),
                '\'' => result.push_str("\\'"),
                ch if ch.is_ascii_control() => {
                    use std::fmt::Write as _;
                    let _ = write!(result, "\\{:03o}", ch as u32);
                }
                ch if ch.is_control() => {
                    use std::fmt::Write as _;
                    let _ = write!(result, "\\u{{{:x}}}", ch as u32);
                }
                ch => result.push(ch),
            }
        }

        result.push('\'');
        return Cow::Owned(result);
    }

    if !name.contains('\'') {
        return Cow::Owned(format!("'{}'", name));
    }

    if !name
        .chars()
        .any(|ch| matches!(ch, '"' | '$' | '`' | '\\' | '!'))
    {
        return Cow::Owned(format!("\"{}\"", name));
    }

    let mut result = String::new();
    for (index, part) in name.split('\'').enumerate() {
        if index > 0 {
            result.push_str("\\'");
        }
        if !part.is_empty() {
            result.push('\'');
            result.push_str(part);
            result.push('\'');
        }
    }

    if result.is_empty() {
        Cow::Owned("''".to_string())
    } else {
        Cow::Owned(result)
    }
}

/// Return `true` if `name` is ASCII and displays as-is on a terminal, so
/// `quote_name` would return it unchanged. Uses a table derived from
/// [`needs_shell_quoting`], avoiding UTF-8 decoding for the common case.
pub fn is_plain_ascii(name: &[u8]) -> bool {
    static PLAIN: [bool; 128] = {
        let mut table = [true; 128];
        let mut byte = 0;
        while byte < 128 {
            table[byte] = !ascii_needs_quoting(byte as u8);
            byte += 1;
        }
        table
    };
    !name.is_empty()
        && name
            .iter()
            .all(|&byte| PLAIN.get(usize::from(byte)) == Some(&true))
}

/// Whether `quote_name(name, true)` changes the name (quotes or escapes
/// it), decided from the bytes without building the quoted form.
///
/// Outside ASCII, `char::is_control` is true only for U+0080..=U+009F, and
/// `char::is_whitespace` for a short fixed list, so only those characters
/// need decoding. Names that are not valid UTF-8 are displayed escaped.
pub fn needs_terminal_quoting(name: &[u8]) -> bool {
    if is_plain_ascii(name) {
        return false;
    }
    if name
        .iter()
        .any(|&byte| byte.is_ascii() && ascii_needs_quoting(byte))
    {
        return true;
    }
    match std::str::from_utf8(name) {
        Err(_) => true,
        Ok(text) => text
            .chars()
            .any(|ch| !ch.is_ascii() && (ch.is_control() || ch.is_whitespace())),
    }
}

/// [`needs_shell_quoting`] for a single ASCII character.
const fn ascii_needs_quoting(byte: u8) -> bool {
    byte < 0x20
        || byte == 0x7f
        || matches!(
            byte,
            b' ' | b'\''
                | b'"'
                | b'\\'
                | b'$'
                | b'`'
                | b'&'
                | b';'
                | b'|'
                | b'<'
                | b'>'
                | b'('
                | b')'
                | b'['
                | b']'
                | b'{'
                | b'}'
                | b'*'
                | b'?'
                | b'#'
                | b'~'
                | b'!'
        )
}

/// Return `true` if `name` requires shell quoting.
fn needs_shell_quoting(name: &str) -> bool {
    name.chars().any(|ch| {
        ch.is_control()
            || ch.is_whitespace()
            || matches!(
                ch,
                '\'' | '"'
                    | '\\'
                    | '$'
                    | '`'
                    | '&'
                    | ';'
                    | '|'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '*'
                    | '?'
                    | '#'
                    | '~'
                    | '!'
            )
    })
}

/// Quote a name for use in an error message, always wrapping in quotes.
pub fn quote_for_message(name: &str) -> String {
    let quoted = quote_name(name, true);
    if quoted.starts_with('\'') || quoted.starts_with('"') || quoted.starts_with("$'") {
        quoted.into_owned()
    } else {
        format!("'{}'", quoted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_table_matches_shell_quoting_rules() {
        for byte in 0u8..128 {
            let name = (byte as char).to_string();
            assert_eq!(
                is_plain_ascii(name.as_bytes()),
                !needs_shell_quoting(&name),
                "byte {byte:#04x}"
            );
        }
        assert!(!is_plain_ascii("café".as_bytes()));
        assert!(!is_plain_ascii(b""));
    }

    #[test]
    fn terminal_quoting_check_matches_quote_name() {
        let changed = |name: &[u8]| match std::str::from_utf8(name) {
            Ok(text) => matches!(quote_name(text, true), std::borrow::Cow::Owned(_)),
            Err(_) => true,
        };
        let names: &[&[u8]] = &[
            b"plain.txt",
            b"with space",
            "café".as_bytes(),
            "資料_2024（1）".as_bytes(),
            "写真 2".as_bytes(),
            "nbsp\u{a0}here".as_bytes(),
            "ideographic\u{3000}space".as_bytes(),
            "c1\u{85}control".as_bytes(),
            "line\u{2028}sep".as_bytes(),
            "メモ$".as_bytes(),
            "e\u{301}".as_bytes(),
            b"bad\xffutf8",
            b"tab\there",
        ];
        for name in names {
            assert_eq!(
                needs_terminal_quoting(name),
                changed(name),
                "{:?}",
                String::from_utf8_lossy(name)
            );
        }
        // Every code point up to U+FFFF, embedded in an otherwise plain name.
        for code in 0x80u32..0x1_0000 {
            if let Some(ch) = char::from_u32(code) {
                let text = format!("a{ch}b");
                assert_eq!(
                    needs_terminal_quoting(text.as_bytes()),
                    changed(text.as_bytes()),
                    "U+{code:04X}"
                );
            }
        }
    }
}
