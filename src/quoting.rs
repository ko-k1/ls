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
