//! `fmt`: rewrite source files in the one standard layout (docs/06, milestone 11).
//!
//! The formatter keeps the program's line structure and comments, and normalises everything
//! else: four spaces per indentation level, one space around binary operators and after commas,
//! none inside brackets, keyword arguments written `name=value` but annotated defaults written
//! `name: T = value`, continuation lines inside brackets indented one level, at most one blank
//! line inside a block and two at the top level, no trailing whitespace, and one newline at the
//! end of the file. Strings, including f-strings and docstrings, are left exactly as written.
//!
//! It works on its own scan of the text, because the language's lexer drops comments. As a
//! safety net, the result is lexed with the real lexer and must give the same tokens as the
//! original; otherwise the file is left alone and the problem reported as a bug.

use lang_diag::{Diagnostics, SourceMap};
use lang_syntax::token::TokenKind;

/// A piece of source text, as the formatter sees it.
#[derive(Clone, Debug, PartialEq)]
enum Piece {
    Word(String),
    Str(String),
    Op(String),
    Comment(String),
    /// A line break inside brackets, which the formatter keeps.
    Break,
}

/// One logical line: its original indentation width and its pieces. Blank lines and lines with
/// only a comment are logical lines too.
struct Line {
    width: usize,
    pieces: Vec<Piece>,
    /// The number of blank lines before it.
    blank_before: usize,
}

const KEYWORDS_BEFORE_OPERAND: &[&str] = &[
    "and", "as", "assert", "case", "elif", "else", "for", "from", "if", "import", "in", "is", "lambda", "match", "not",
    "or", "return", "spawn", "while", "with", "yield", "await",
];

/// Scan `text` into logical lines. Returns `None` for text this scanner does not understand,
/// which the real lexer will then have reported anyway.
fn scan(text: &str) -> Option<Vec<Line>> {
    let bytes = text.as_bytes();
    let mut lines = Vec::new();
    let mut pieces: Vec<Piece> = Vec::new();
    let mut depth: i32 = 0;
    let mut width = 0;
    let mut at_line_start = true;
    let mut blank_run = 0;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if at_line_start {
            let start = i;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'\n' || bytes[i] == b'\r') {
                blank_run += 1;
                i += if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                continue;
            }
            width = i - start;
            at_line_start = false;
            continue;
        }
        match c {
            b' ' | b'\t' => i += 1,
            b'\r' | b'\n' => {
                i += if c == b'\r' && bytes.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                if depth > 0 {
                    pieces.push(Piece::Break);
                    // Skip the next line's indentation; it is recomputed.
                    while i < bytes.len() && bytes[i] == b' ' {
                        i += 1;
                    }
                } else {
                    lines.push(Line { width, pieces: std::mem::take(&mut pieces), blank_before: blank_run });
                    blank_run = 0;
                    at_line_start = true;
                }
            }
            b'#' => {
                let end = text[i..].find('\n').map_or(bytes.len(), |n| i + n);
                pieces.push(Piece::Comment(text[i..end].trim_end().to_string()));
                i = end;
            }
            b'"' | b'\'' => {
                let end = string_end(bytes, i)?;
                pieces.push(Piece::Str(text[i..end].to_string()));
                i = end;
            }
            b'0'..=b'9' => {
                let start = i;
                while i < bytes.len() {
                    let b = bytes[i];
                    let exponent_sign = (b == b'+' || b == b'-')
                        && matches!(bytes[i - 1], b'e' | b'E')
                        && !text[start..i].starts_with("0x");
                    if b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || exponent_sign {
                        i += 1;
                    } else {
                        break;
                    }
                }
                pieces.push(Piece::Word(text[start..i].to_string()));
            }
            _ if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] >= 0x80) {
                    i += 1;
                }
                let word = &text[start..i];
                // A string prefix: `f"..."`.
                if matches!(word, "f" | "F" | "b" | "r") && matches!(bytes.get(i), Some(b'"' | b'\'')) {
                    let end = string_end(bytes, i)?;
                    pieces.push(Piece::Str(text[start..end].to_string()));
                    i = end;
                } else {
                    pieces.push(Piece::Word(word.to_string()));
                }
            }
            _ => {
                const OPERATORS: &[&str] = &[
                    "//=", "**", "//", "->", "==", "!=", "<=", ">=", "<<", ">>", "+=", "-=", "*=", "/=", "%=", "+",
                    "-", "*", "/", "%", "&", "|", "^", "~", "<", ">", "=", "(", ")", "[", "]", "{", "}", ":", ",", ".",
                    "@", "?", ";", "\\",
                ];
                let op = OPERATORS.iter().find(|op| text[i..].starts_with(**op))?;
                match *op {
                    "(" | "[" | "{" => depth += 1,
                    ")" | "]" | "}" => depth -= 1,
                    _ => {}
                }
                pieces.push(Piece::Op(op.to_string()));
                i += op.len();
            }
        }
    }
    if !pieces.is_empty() {
        lines.push(Line { width, pieces, blank_before: blank_run });
    }
    Some(lines)
}

/// Where the string starting at `start` (at its quote) ends.
fn string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let quote = bytes[start];
    let triple = bytes.get(start + 1) == Some(&quote) && bytes.get(start + 2) == Some(&quote);
    let mut i = start + if triple { 3 } else { 1 };
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b if b == quote => {
                if !triple {
                    return Some(i + 1);
                }
                if bytes.get(i + 1) == Some(&quote) && bytes.get(i + 2) == Some(&quote) {
                    return Some(i + 3);
                }
                i += 1;
            }
            b'\n' if !triple => return None,
            _ => i += 1,
        }
    }
    None
}

fn is_operand_end(piece: &Piece) -> bool {
    match piece {
        Piece::Word(w) => !KEYWORDS_BEFORE_OPERAND.contains(&w.as_str()),
        Piece::Str(_) => true,
        Piece::Op(op) => matches!(op.as_str(), ")" | "]" | "}" | "?"),
        _ => false,
    }
}

/// Write one logical line's pieces with normalised spacing. `indent` is its indentation.
fn render_line(pieces: &[Piece], indent: usize, out: &mut String) {
    // Per open bracket: the bracket, and whether a `:` (an annotation) has appeared in the
    // current element, which makes a following `=` a default rather than a keyword argument.
    let mut brackets: Vec<(String, bool)> = Vec::new();
    let mut prev: Option<&Piece> = None;
    // Whether the previous piece was a unary minus, plus or `~`, or `*`/`**` before a
    // parameter or argument name.
    let mut prev_unary = false;
    let first_word = pieces.iter().find_map(|p| if let Piece::Word(w) = p { Some(w.as_str()) } else { None });
    let lambda_depth = std::cell::Cell::new(None::<usize>);
    for (index, piece) in pieces.iter().enumerate() {
        let next = pieces.get(index + 1);
        let in_square = brackets.last().is_some_and(|(b, _)| b == "[");
        let space_before = match (prev, piece) {
            (None, _) => false,
            (Some(Piece::Break), _) => false,
            (_, Piece::Break) => false,
            (_, Piece::Comment(_)) => {
                out.push_str("  ");
                false
            }
            _ if prev_unary => false,
            (Some(Piece::Op(p)), _) if matches!(p.as_str(), "(" | "[" | "{" | "." | "@") => false,
            // The `...` of `tuple[T, ...]`.
            (Some(Piece::Op(p)), Piece::Op(op)) if p == "," && op == "." => true,
            (_, Piece::Op(op)) if matches!(op.as_str(), ")" | "]" | "}" | "," | "." | "?" | ";") => false,
            (Some(Piece::Op(p)), Piece::Op(op)) if p == "." && op == "." => false,
            (_, Piece::Op(op)) if op == ":" => false,
            (Some(Piece::Op(p)), _) if p == ":" => !(in_square && lambda_depth.get().is_none()),
            (Some(prev_piece), Piece::Op(op)) if matches!(op.as_str(), "(" | "[") => !is_operand_end(prev_piece),
            (Some(Piece::Op(p)), _) if p == "=" || op_is_binary(p) => true,
            (_, Piece::Op(op)) if op == "=" || op_is_binary(op) => true,
            _ => true,
        };
        // Keyword arguments and defaults without an annotation: `f(x=1)`.
        let tight_assign = |op: &str| {
            op == "="
                && brackets.last().is_some_and(|(b, annotated)| b == "(" && !annotated)
                && first_word != Some("type")
        };
        let space_before = match piece {
            Piece::Op(op) if tight_assign(op) => false,
            _ => match prev {
                Some(Piece::Op(p)) if tight_assign(p) => false,
                _ => space_before,
            },
        };
        if space_before {
            out.push(' ');
        }
        match piece {
            Piece::Break => {
                out.push('\n');
                let closing = matches!(next, Some(Piece::Op(op)) if matches!(op.as_str(), ")" | "]" | "}"));
                let level = indent + if closing { 0 } else { 4 };
                out.push_str(&" ".repeat(level));
            }
            Piece::Word(w) | Piece::Str(w) | Piece::Op(w) | Piece::Comment(w) => out.push_str(w),
        }
        // Track brackets and what the next piece's spacing depends on.
        prev_unary = false;
        if let Piece::Op(op) = piece {
            match op.as_str() {
                // Type parameters of a declaration, `def first[T: Ord](...)`, are not a slice.
                "[" if brackets.is_empty()
                    && matches!(first_word, Some("def" | "struct" | "enum" | "impl" | "trait")) =>
                {
                    brackets.push(("<".to_string(), false))
                }
                "(" | "[" | "{" => brackets.push((op.clone(), false)),
                ")" | "]" | "}" => {
                    brackets.pop();
                }
                "," => {
                    if let Some(top) = brackets.last_mut() {
                        top.1 = false;
                    }
                }
                ":" => {
                    if let Some(top) = brackets.last_mut() {
                        top.1 = true;
                    }
                    if lambda_depth.get() == Some(brackets.len()) {
                        lambda_depth.set(None);
                    }
                }
                "-" | "+" | "~" | "*" | "**" => {
                    // Unary when nothing that ends an operand comes before it.
                    prev_unary = !prev.is_some_and(is_operand_end);
                }
                _ => {}
            }
        }
        if let Piece::Word(w) = piece
            && w == "lambda"
        {
            lambda_depth.set(Some(brackets.len()));
        }
        prev = Some(piece);
    }
}

fn op_is_binary(op: &str) -> bool {
    matches!(
        op,
        "+" | "-"
            | "*"
            | "/"
            | "//"
            | "%"
            | "**"
            | "<<"
            | ">>"
            | "&"
            | "|"
            | "^"
            | "<"
            | ">"
            | "<="
            | ">="
            | "=="
            | "!="
            | "->"
            | "+="
            | "-="
            | "*="
            | "/="
            | "//="
            | "%="
    )
}

/// Format a whole file. `None` if the scanner could not follow it.
pub fn format(text: &str) -> Option<String> {
    let lines = scan(text)?;
    let mut out = String::new();
    // Indentation widths of the open blocks, as written; the levels are their positions.
    let mut widths: Vec<usize> = vec![0];
    let levels: Vec<usize> = {
        let mut levels = Vec::with_capacity(lines.len());
        for line in &lines {
            let comment_only = matches!(line.pieces.as_slice(), [Piece::Comment(_)]);
            if comment_only {
                levels.push(usize::MAX);
                continue;
            }
            while line.width < *widths.last().unwrap() {
                widths.pop();
            }
            if line.width > *widths.last().unwrap() {
                widths.push(line.width);
            }
            levels.push(widths.len() - 1);
        }
        // A comment line is indented like the code line after it (or before it, at the end).
        let mut next_level = 0;
        for level in levels.iter_mut().rev() {
            if *level == usize::MAX {
                *level = next_level;
            } else {
                next_level = *level;
            }
        }
        levels
    };
    for (index, line) in lines.iter().enumerate() {
        let level = levels[index];
        if index > 0 && line.blank_before > 0 {
            let most = if level == 0 { 2 } else { 1 };
            out.push_str(&"\n".repeat(line.blank_before.min(most)));
        }
        let indent = level * 4;
        out.push_str(&" ".repeat(indent));
        render_line(&line.pieces, indent, &mut out);
        out.push('\n');
    }
    Some(out)
}

/// The tokens of `text` as comparable strings, for checking that formatting changed nothing.
pub fn token_fingerprint(text: &str) -> Option<Vec<String>> {
    let mut sources = SourceMap::default();
    let file = sources.add("<fmt>", text.to_string());
    let mut diags = Diagnostics::default();
    let tokens = lang_syntax::lex(file, text, &mut diags);
    if diags.has_errors() {
        return None;
    }
    Some(
        tokens
            .iter()
            .map(|t| match &t.kind {
                // F-string pieces record their offsets; compare their text instead.
                TokenKind::FString(_) => format!("f:{}", &text[t.span.lo as usize..t.span.hi as usize]),
                kind => format!("{kind:?}"),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::format;

    fn check(input: &str, expected: &str) {
        assert_eq!(format(input).unwrap(), expected);
        assert_eq!(format(expected).unwrap(), expected, "formatting is not idempotent");
    }

    #[test]
    fn spacing() {
        check(
            "def f( x:int=1,*,y : str='a' )->int:\n  return x+y*-2\n",
            "def f(x: int = 1, *, y: str = 'a') -> int:\n    return x + y * -2\n",
        );
        check("print(a[1:2],g(k=3), {'a':1})\n", "print(a[1:2], g(k=3), {'a': 1})\n");
        check("x=lambda a,b:a+b\n", "x = lambda a, b: a + b\n");
        check("xs: tuple[int,...] = (1,2)\n", "xs: tuple[int, ...] = (1, 2)\n");
        check(
            "def first[T:Ord](xs:list[T])->T:\n    return xs[0:1][0]\n",
            "def first[T: Ord](xs: list[T]) -> T:\n    return xs[0:1][0]\n",
        );
        check(
            "@http.get( '/a', status=201 )\ndef g()->str?:\n    pass\n",
            "@http.get('/a', status=201)\ndef g() -> str?:\n    pass\n",
        );
        check("if not x and -y>2 :\n\tpass\n".replace('\t', "    ").as_str(), "if not x and -y > 2:\n    pass\n");
    }

    /// Every source file in the repository: formatting keeps its tokens and is idempotent.
    #[test]
    fn whole_repository() {
        use super::token_fingerprint;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut checked = 0;
        for dir in ["tests", "bench"] {
            for path in crate::source_files(&[root.join(dir)]) {
                let text = std::fs::read_to_string(&path).unwrap();
                let Some(before) = token_fingerprint(&text) else { continue };
                let once = format(&text).unwrap_or_else(|| panic!("cannot format {}", path.display()));
                assert_eq!(token_fingerprint(&once).as_ref(), Some(&before), "{} changed meaning", path.display());
                assert_eq!(format(&once).as_deref(), Some(once.as_str()), "{} is not stable", path.display());
                checked += 1;
            }
        }
        assert!(checked > 100, "only {checked} files checked");
    }

    #[test]
    fn comments_blank_lines_and_continuations() {
        check(
            "import io\n\n\n\n# about f\ndef f():\n    x = [1,\n  2,\n        ]   # trailing\n\n\n    return x\n",
            "import io\n\n\n# about f\ndef f():\n    x = [1,\n        2,\n    ]  # trailing\n\n    return x\n",
        );
    }
}
