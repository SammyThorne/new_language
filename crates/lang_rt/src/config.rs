//! `.env` files for the `config` module (docs/14, milestone 7). The grammar, which docs/05
//! documents:
//!
//! - One `KEY=value` per line, with an optional `export ` before the key. A key is a letter or
//!   `_` followed by letters, digits and `_`; spaces may surround the `=`.
//! - Blank lines and lines starting with `#` are ignored.
//! - A value in single quotes is taken as it is. A value in double quotes reads `\n`, `\r`, `\t`,
//!   `\"` and `\\` as escapes and keeps any other backslash. Either kind may span lines. Only
//!   spaces and a `#` comment may follow the closing quote.
//! - An unquoted value runs to the end of the line, or to a `#` that follows a space or tab,
//!   and is trimmed. `KEY=` is the empty string.
//! - There is no `${VAR}` interpolation, a UTF-8 byte order mark is ignored, `\r\n` ends a line
//!   as `\n` does, and a later line for the same key wins.
//! - Anything else is an error that names its line.

use std::cell::RefCell;

use crate::Str;
use crate::sys::record_error;

thread_local! {
    /// The pairs of the last file parsed on this thread.
    static PAIRS: RefCell<Vec<(String, String)>> = const { RefCell::new(Vec::new()) };
}

/// The variables in a `.env` file, in order, or the first malformed line's error.
fn parse(text: &str) -> Result<Vec<(String, String)>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let chars: Vec<char> = text.chars().filter(|&c| c != '\r').collect();
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    let mut line = 1;
    while i < chars.len() {
        let start_line = line;
        let skip_spaces = |i: &mut usize| {
            while *i < chars.len() && matches!(chars[*i], ' ' | '\t') {
                *i += 1;
            }
        };
        skip_spaces(&mut i);
        if i >= chars.len() {
            break;
        }
        if chars[i] == '\n' {
            i += 1;
            line += 1;
            continue;
        }
        if chars[i] == '#' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        let error = |message: String| Err(format!("line {start_line}: {message}"));
        // `export KEY=...`: `export` followed by a space, and then a key.
        let rest: String = chars[i..chars.len().min(i + 7)].iter().collect();
        if rest == "export " || rest == "export\t" {
            i += 7;
            skip_spaces(&mut i);
        }
        let key_start = i;
        while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
            i += 1;
        }
        let key: String = chars[key_start..i].iter().collect();
        if key.is_empty() || key.starts_with(|c: char| c.is_ascii_digit()) {
            let found = chars.get(key_start).map_or("the end of the line".to_string(), |&c| describe(c));
            return error(format!(
                "expected a variable name (a letter or `_`, then letters, digits and `_`), found {found}"
            ));
        }
        skip_spaces(&mut i);
        if chars.get(i) != Some(&'=') {
            let found = chars.get(i).map_or("the end of the file".to_string(), |&c| describe(c));
            return error(format!("expected `=` after `{key}`, found {found}"));
        }
        i += 1;
        skip_spaces(&mut i);
        let value = match chars.get(i) {
            Some(&quote @ ('\'' | '"')) => {
                i += 1;
                let mut value = String::new();
                loop {
                    match chars.get(i) {
                        None => {
                            let kind = if quote == '"' { "double" } else { "single" };
                            return error(format!("the {kind}-quoted value of `{key}` has no closing quote"));
                        }
                        Some(&c) if c == quote => {
                            i += 1;
                            break;
                        }
                        Some('\\') if quote == '"' => {
                            let escaped = match chars.get(i + 1) {
                                Some('n') => Some('\n'),
                                Some('r') => Some('\r'),
                                Some('t') => Some('\t'),
                                Some('"') => Some('"'),
                                Some('\\') => Some('\\'),
                                _ => None,
                            };
                            match escaped {
                                Some(c) => {
                                    value.push(c);
                                    i += 2;
                                }
                                None => {
                                    value.push('\\');
                                    i += 1;
                                }
                            }
                        }
                        Some(&c) => {
                            if c == '\n' {
                                line += 1;
                            }
                            value.push(c);
                            i += 1;
                        }
                    }
                }
                skip_spaces(&mut i);
                match chars.get(i) {
                    None | Some('\n') => {}
                    Some('#') => {
                        while i < chars.len() && chars[i] != '\n' {
                            i += 1;
                        }
                    }
                    Some(&c) => {
                        return Err(format!(
                            "line {line}: unexpected {} after the closing quote of `{key}`; only a `#` comment may follow it",
                            describe(c)
                        ));
                    }
                }
                value
            }
            _ => {
                let mut value = String::new();
                while i < chars.len() && chars[i] != '\n' {
                    if chars[i] == '#' && (value.is_empty() || value.ends_with([' ', '\t'])) {
                        while i < chars.len() && chars[i] != '\n' {
                            i += 1;
                        }
                        break;
                    }
                    value.push(chars[i]);
                    i += 1;
                }
                value.trim().to_string()
            }
        };
        pairs.retain(|(k, _)| *k != key);
        pairs.push((key, value));
    }
    Ok(pairs)
}

/// A character in an error message.
fn describe(c: char) -> String {
    match c {
        '\n' => "the end of the line".to_string(),
        ' ' => "a space".to_string(),
        '\t' => "a tab".to_string(),
        c => format!("`{c}`"),
    }
}

/// Parse a `.env` file's text: the number of variables, read back with `lrt_config_env_pair`,
/// or -1 after recording the error.
///
/// # Safety
/// `ptr` and `len` describe a UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn lrt_config_parse_env(ptr: *const u8, len: i64) -> i64 {
    match parse(unsafe { Str::from_raw(ptr, len) }) {
        Ok(pairs) => {
            let count = pairs.len() as i64;
            PAIRS.with_borrow_mut(|stored| *stored = pairs);
            count
        }
        Err(message) => {
            record_error(message);
            -1
        }
    }
}

/// Variable `i` of the last file parsed: its name (`part` 0) or value (1).
#[unsafe(no_mangle)]
pub extern "C-unwind" fn lrt_config_env_pair(i: i64, part: i64) -> Str {
    PAIRS.with_borrow(|pairs| {
        let text = usize::try_from(i)
            .ok()
            .and_then(|i| pairs.get(i))
            .map_or("", |(key, value)| if part == 0 { key.as_str() } else { value.as_str() });
        Str::from_bytes(text.as_bytes())
    })
}

#[cfg(test)]
mod tests {
    use super::parse;

    fn pairs(text: &str) -> Vec<(String, String)> {
        parse(text).unwrap_or_else(|e| panic!("{e}"))
    }

    fn one(text: &str) -> String {
        let all = pairs(text);
        assert_eq!(all.len(), 1, "{all:?}");
        all[0].1.clone()
    }

    #[test]
    fn unquoted_values() {
        assert_eq!(one("A=b"), "b");
        assert_eq!(one("  A  =  spaced out value  "), "spaced out value");
        assert_eq!(one("A="), "");
        assert_eq!(one("A=   # only a comment"), "");
        assert_eq!(one("A=value # comment"), "value");
        assert_eq!(one("A=value\t# comment"), "value");
        assert_eq!(one("A=http://x/#fragment"), "http://x/#fragment");
        assert_eq!(one("A=it's \"fine\""), "it's \"fine\"");
        assert_eq!(one("A=x=y"), "x=y");
        assert_eq!(one("export A=1"), "1");
        assert_eq!(one("export\tA=1"), "1");
        assert_eq!(pairs("export=1"), [("export".to_string(), "1".to_string())]);
    }

    #[test]
    fn quoted_values() {
        assert_eq!(one("A='a \\n b # c'"), "a \\n b # c");
        assert_eq!(one("A=\"a\\nb\\tc\\\"d\\\\e\\qf\""), "a\nb\tc\"d\\e\\qf");
        assert_eq!(one("A=\"x\" # comment"), "x");
        assert_eq!(one("A='x'   "), "x");
        assert_eq!(one("A=\"\""), "");
        assert_eq!(one("A=\"  kept  \""), "  kept  ");
        assert_eq!(one("A=\"one\ntwo\""), "one\ntwo");
        assert_eq!(
            one("A='-----BEGIN KEY-----\nabc\n-----END KEY-----'"),
            "-----BEGIN KEY-----\nabc\n-----END KEY-----"
        );
    }

    #[test]
    fn files() {
        let text = "\u{feff}# settings\r\n\r\nexport PORT=8080\r\nNAME=\"a\r\nb\"\r\nPORT=9090 # again\r\n";
        assert_eq!(pairs(text), [("NAME".to_string(), "a\nb".to_string()), ("PORT".to_string(), "9090".to_string())]);
        assert_eq!(pairs(""), []);
        assert_eq!(pairs("\n\n   \n# just comments\n"), []);
    }

    #[test]
    fn errors_name_the_line() {
        assert_eq!(parse("A=1\nB\n").unwrap_err(), "line 2: expected `=` after `B`, found the end of the line");
        assert_eq!(parse("A=1\n\nB 2").unwrap_err(), "line 3: expected `=` after `B`, found `2`");
        assert_eq!(
            parse("1A=2").unwrap_err(),
            "line 1: expected a variable name (a letter or `_`, then letters, digits and `_`), found `1`"
        );
        assert_eq!(
            parse("=2").unwrap_err(),
            "line 1: expected a variable name (a letter or `_`, then letters, digits and `_`), found `=`"
        );
        assert_eq!(parse("A-B=2").unwrap_err(), "line 1: expected `=` after `A`, found `-`");
        assert_eq!(
            parse("A=\"open\nstill open").unwrap_err(),
            "line 1: the double-quoted value of `A` has no closing quote"
        );
        assert_eq!(
            parse("X=1\nA='x'y").unwrap_err(),
            "line 2: unexpected `y` after the closing quote of `A`; only a `#` comment may follow it"
        );
        assert_eq!(
            parse("A=\"two\nlines\" extra").unwrap_err(),
            "line 2: unexpected `e` after the closing quote of `A`; only a `#` comment may follow it"
        );
        assert_eq!(parse("B=1\nexport").unwrap_err(), "line 2: expected `=` after `export`, found the end of the file");
    }
}
