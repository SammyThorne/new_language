//! The lexer. Hand-written, works on bytes, and turns indentation into layout tokens.
//!
//! Layout rules (docs/03-compiler-architecture.md):
//! - A stack of indentation widths starts as `[0]`.
//! - At the start of each line outside brackets, the leading spaces are measured. Blank and
//!   comment-only lines are ignored. A wider line pushes a width and emits `Indent`; a narrower
//!   line pops widths, emitting one `Dedent` each, until it matches an enclosing width.
//! - A line with tokens ends with `Newline`, unless it ends inside `()`, `[]` or `{}`.
//! - At end of file: a final `Newline` if needed, then a `Dedent` per open block, then `Eof`.

use crate::token::{FPiece, Token, TokenKind, keyword};
use lang_diag::{Diagnostic, Diagnostics, FileId, Span, codes};

pub fn lex(file: FileId, text: &str, diags: &mut Diagnostics) -> Vec<Token> {
    let mut lexer = Lexer::new(file, text, 0, diags);
    lexer.run();
    lexer.tokens
}

/// Lex an expression embedded in an f-string. `base` is the byte offset of `text` in the file,
/// so token spans point into the file. Layout tokens are never produced, as if inside brackets,
/// and the stream ends with `Eof` alone.
pub fn lex_fragment(file: FileId, text: &str, base: usize, diags: &mut Diagnostics) -> Vec<Token> {
    let mut lexer = Lexer::new(file, text, base, diags);
    lexer.depth = 1;
    lexer.at_line_start = false;
    lexer.fragment = true;
    lexer.run();
    lexer.tokens
}

struct Lexer<'a> {
    file: FileId,
    /// Offset of `text` within the file; non-zero only for f-string fragments.
    base: usize,
    fragment: bool,
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    /// Indentation widths of the open blocks; the bottom entry is always 0.
    indents: Vec<usize>,
    /// Bracket nesting depth. Layout tokens are suppressed while it is above zero.
    depth: usize,
    at_line_start: bool,
    /// Whether the current logical line has produced a token, and so needs a `Newline`.
    line_has_tokens: bool,
    diags: &'a mut Diagnostics,
}

impl<'a> Lexer<'a> {
    fn new(file: FileId, text: &'a str, base: usize, diags: &'a mut Diagnostics) -> Lexer<'a> {
        Lexer {
            file,
            base,
            fragment: false,
            text,
            bytes: text.as_bytes(),
            pos: 0,
            tokens: Vec::new(),
            indents: vec![0],
            depth: 0,
            at_line_start: true,
            line_has_tokens: false,
            diags,
        }
    }

    fn span(&self, lo: usize, hi: usize) -> Span {
        Span::new(self.file, self.base + lo, self.base + hi)
    }

    fn push(&mut self, kind: TokenKind, lo: usize, hi: usize) {
        let span = self.span(lo, hi);
        self.tokens.push(Token { kind, span });
    }

    fn byte(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn current_char(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }

    fn run(&mut self) {
        loop {
            if self.at_line_start {
                self.at_line_start = false;
                if self.depth == 0 {
                    self.layout();
                }
            }
            while matches!(self.byte(0), Some(b' ' | b'\t' | b'\r')) {
                self.pos += 1;
            }
            let Some(b) = self.byte(0) else { break };
            match b {
                b'#' => {
                    while !matches!(self.byte(0), None | Some(b'\n')) {
                        self.pos += 1;
                    }
                }
                b'\n' => {
                    if self.depth == 0 && self.line_has_tokens {
                        self.push(TokenKind::Newline, self.pos, self.pos + 1);
                        self.line_has_tokens = false;
                    }
                    self.pos += 1;
                    self.at_line_start = true;
                }
                _ => {
                    self.token();
                    self.line_has_tokens = true;
                }
            }
        }

        let end = self.bytes.len();
        if self.line_has_tokens && !self.fragment {
            self.push(TokenKind::Newline, end, end);
        }
        while self.indents.len() > 1 {
            self.indents.pop();
            self.push(TokenKind::Dedent, end, end);
        }
        self.push(TokenKind::Eof, end, end);
    }

    /// Measure the indentation of a new line and emit `Indent` or `Dedent` tokens.
    fn layout(&mut self) {
        let start = self.pos;
        let mut width = 0;
        let mut first_tab = None;
        while let Some(b) = self.byte(0) {
            match b {
                b' ' => width += 1,
                b'\t' => {
                    first_tab.get_or_insert(self.pos);
                    width += 1;
                }
                _ => break,
            }
            self.pos += 1;
        }
        // Blank and comment-only lines do not affect layout.
        if matches!(self.byte(0), None | Some(b'\n' | b'\r' | b'#')) {
            return;
        }
        if let Some(tab) = first_tab {
            self.diags.push(
                Diagnostic::error(
                    codes::TAB_IN_INDENT,
                    "tab character in indentation",
                    self.span(tab, tab + 1),
                    "tab here",
                )
                .with_help("indent with spaces only"),
            );
        }

        let top = *self.indents.last().unwrap();
        if width > top {
            self.indents.push(width);
            self.push(TokenKind::Indent, start, self.pos);
        } else if width < top {
            while width < *self.indents.last().unwrap() {
                self.indents.pop();
                self.push(TokenKind::Dedent, self.pos, self.pos);
            }
            if width != *self.indents.last().unwrap() {
                self.diags.push(
                    Diagnostic::error(
                        codes::BAD_DEDENT,
                        "unindent does not match any outer indentation level",
                        self.span(start, self.pos),
                        "this indentation",
                    )
                    .with_help("indent this line by the same number of spaces as an enclosing block"),
                );
            }
        }
    }

    fn token(&mut self) {
        let start = self.pos;
        let c = self.current_char().unwrap();
        if c == '_' || c.is_alphabetic() {
            self.name_or_keyword(start);
        } else if c.is_ascii_digit() {
            self.number(start);
        } else if c == '"' || c == '\'' {
            let value = self.string(start);
            self.push(TokenKind::Str(value), start, self.pos);
        } else {
            self.punctuation(start, c);
        }
    }

    fn name_or_keyword(&mut self, start: usize) {
        while let Some(c) = self.current_char() {
            if c == '_' || c.is_alphanumeric() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
        let word = &self.text[start..self.pos];

        // String prefixes: `f"..."`, and the unsupported `b"..."`, `r"..."` and combinations.
        if matches!(self.byte(0), Some(b'"' | b'\'')) && matches!(word, "f" | "F") {
            let pieces = self.fstring(start);
            self.push(TokenKind::FString(pieces), start, self.pos);
            return;
        }
        if matches!(self.byte(0), Some(b'"' | b'\''))
            && matches!(word.to_ascii_lowercase().as_str(), "b" | "r" | "rb" | "br" | "fr" | "rf")
        {
            let what = match word.to_ascii_lowercase().as_str() {
                "fr" | "rf" => "raw f-strings are",
                "b" | "rb" | "br" => "bytes literals are",
                _ => "raw strings are",
            };
            let prefix_span = self.span(start, self.pos);
            self.diags.push(Diagnostic::error(
                codes::NOT_SUPPORTED_SYNTAX,
                format!("{what} not supported yet"),
                prefix_span,
                "string prefix",
            ));
            let value = self.string(self.pos);
            self.push(TokenKind::Str(value), start, self.pos);
            return;
        }

        let kind = keyword(word).unwrap_or_else(|| TokenKind::Name(word.to_string()));
        self.push(kind, start, self.pos);
    }

    fn number(&mut self, start: usize) {
        let radix = match (self.byte(0), self.byte(1)) {
            (Some(b'0'), Some(b'x' | b'X')) => 16,
            (Some(b'0'), Some(b'o' | b'O')) => 8,
            (Some(b'0'), Some(b'b' | b'B')) => 2,
            _ => 10,
        };
        if radix != 10 {
            self.pos += 2;
        }
        let digits_start = self.pos;
        while matches!(self.byte(0), Some(b) if b.is_ascii_digit() || b == b'_') {
            self.pos += 1;
        }

        if radix == 10 {
            let fraction = self.byte(0) == Some(b'.') && self.byte(1).is_some_and(|b| b.is_ascii_digit());
            let exponent = matches!(self.byte(0), Some(b'e' | b'E'))
                && (self.byte(1).is_some_and(|b| b.is_ascii_digit())
                    || (matches!(self.byte(1), Some(b'+' | b'-')) && self.byte(2).is_some_and(|b| b.is_ascii_digit())));
            if fraction || exponent {
                self.float(start);
                return;
            }
        }

        // Letters directly after a number are part of a malformed literal (`12abc`, `0xFFg`).
        while matches!(self.byte(0), Some(b) if b.is_ascii_alphanumeric() || b == b'_') {
            self.pos += 1;
        }
        let digits: String = self.text[digits_start..self.pos].chars().filter(|&c| c != '_').collect();
        let span = self.span(start, self.pos);
        let value = match u64::from_str_radix(&digits, radix) {
            Ok(value) => value,
            Err(error) if *error.kind() == std::num::IntErrorKind::PosOverflow => {
                self.diags.push(
                    Diagnostic::error(
                        codes::INT_TOO_LARGE,
                        "integer literal is too large",
                        span,
                        "does not fit in 64 bits",
                    )
                    .with_help("the largest `int` is 9223372036854775807"),
                );
                0
            }
            Err(_) => {
                let literal = &self.text[start..self.pos];
                self.diags.push(Diagnostic::error(
                    codes::BAD_NUMBER,
                    format!("invalid number literal `{literal}`"),
                    span,
                    "not a valid number",
                ));
                0
            }
        };
        self.push(TokenKind::Int(value), start, self.pos);
    }

    fn float(&mut self, start: usize) {
        if self.byte(0) == Some(b'.') {
            self.pos += 1;
            while matches!(self.byte(0), Some(b) if b.is_ascii_digit() || b == b'_') {
                self.pos += 1;
            }
        }
        if matches!(self.byte(0), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.byte(0), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.byte(0), Some(b) if b.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let text: String = self.text[start..self.pos].chars().filter(|&c| c != '_').collect();
        let value = text.parse::<f64>().unwrap_or(0.0);
        self.push(TokenKind::Float(value), start, self.pos);
    }

    /// Lex a string literal starting at the opening quote under `self.pos`. `start` is where the
    /// whole literal, including any prefix, begins.
    fn string(&mut self, start: usize) -> String {
        let quote = self.byte(0).unwrap();
        let triple = self.byte(1) == Some(quote) && self.byte(2) == Some(quote);
        let open_len = if triple { 3 } else { 1 };
        let open_span = self.span(start, self.pos + open_len);
        self.pos += open_len;

        let mut value = String::new();
        loop {
            let Some(b) = self.byte(0) else {
                self.unterminated(open_span);
                return value;
            };
            match b {
                b'\n' if !triple => {
                    self.unterminated(open_span);
                    return value;
                }
                b'\\' => self.escape(&mut value),
                _ if b == quote => {
                    if !triple {
                        self.pos += 1;
                        return value;
                    }
                    if self.byte(1) == Some(quote) && self.byte(2) == Some(quote) {
                        self.pos += 3;
                        return value;
                    }
                    value.push(quote as char);
                    self.pos += 1;
                }
                _ => {
                    let c = self.current_char().unwrap();
                    value.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
    }

    /// Lex an f-string whose opening quote is under `self.pos`; `start` is the `f`.
    fn fstring(&mut self, start: usize) -> Vec<FPiece> {
        let quote = self.byte(0).unwrap();
        let triple = self.byte(1) == Some(quote) && self.byte(2) == Some(quote);
        let open_len = if triple { 3 } else { 1 };
        let open_span = self.span(start, self.pos + open_len);
        self.pos += open_len;

        let mut pieces = Vec::new();
        let mut literal = String::new();
        loop {
            let Some(b) = self.byte(0) else {
                self.unterminated(open_span);
                break;
            };
            match b {
                b'\n' if !triple => {
                    self.unterminated(open_span);
                    break;
                }
                b'\\' => self.escape(&mut literal),
                _ if b == quote && (!triple || (self.byte(1) == Some(quote) && self.byte(2) == Some(quote))) => {
                    self.pos += open_len;
                    break;
                }
                b'{' if self.byte(1) == Some(b'{') => {
                    literal.push('{');
                    self.pos += 2;
                }
                b'}' if self.byte(1) == Some(b'}') => {
                    literal.push('}');
                    self.pos += 2;
                }
                b'}' => {
                    self.diags.push(
                        Diagnostic::error(
                            codes::UNEXPECTED_CHAR,
                            "single `}` in an f-string",
                            self.span(self.pos, self.pos + 1),
                            "not matched by a `{`",
                        )
                        .with_help("write `}}` for a literal brace"),
                    );
                    self.pos += 1;
                }
                b'{' => {
                    if !literal.is_empty() {
                        pieces.push(FPiece::Literal(std::mem::take(&mut literal)));
                    }
                    match self.fstring_code(quote, triple) {
                        Some(piece) => pieces.push(piece),
                        None => {
                            // The rest of the string cannot be lexed reliably.
                            while !matches!(self.byte(0), None | Some(b'\n')) && self.byte(0) != Some(quote) {
                                self.pos += 1;
                            }
                            if self.byte(0) == Some(quote) {
                                self.pos += 1;
                            }
                            return pieces;
                        }
                    }
                }
                _ => {
                    let c = self.current_char().unwrap();
                    literal.push(c);
                    self.pos += c.len_utf8();
                }
            }
        }
        if !literal.is_empty() {
            pieces.push(FPiece::Literal(literal));
        }
        pieces
    }

    /// Lex `{expression:spec}` starting at the `{`. Returns `None` after reporting an error.
    fn fstring_code(&mut self, quote: u8, triple: bool) -> Option<FPiece> {
        let open = self.pos;
        self.pos += 1;
        let code_start = self.pos;
        let mut depth = 0usize;
        let mut in_string: Option<u8> = None;
        let mut spec_start = None;
        while let Some(b) = self.byte(0) {
            if b == b'\n' && !triple {
                break;
            }
            if let Some(q) = in_string {
                if b == b'\\' {
                    self.pos += 2;
                    continue;
                }
                if b == q {
                    in_string = None;
                }
                self.pos += 1;
                continue;
            }
            if spec_start.is_some() {
                if b == b'}' {
                    break;
                }
                if b == b'{' {
                    let span = self.span(self.pos, self.pos + 1);
                    self.diags.push(Diagnostic::error(
                        codes::NOT_SUPPORTED_SYNTAX,
                        "nested `{...}` in a format spec is not supported yet",
                        span,
                        "not supported yet",
                    ));
                    return None;
                }
                self.pos += self.current_char().unwrap().len_utf8();
                continue;
            }
            match b {
                _ if b == quote && !triple => break,
                b'\'' | b'"' => in_string = Some(b),
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' => depth = depth.saturating_sub(1),
                b'}' if depth == 0 => break,
                b'}' => depth -= 1,
                b':' if depth == 0 => {
                    spec_start = Some(self.pos + 1);
                }
                b'!' if depth == 0 && self.byte(1) != Some(b'=') => {
                    let span = self.span(self.pos, self.pos + 1);
                    self.diags.push(Diagnostic::error(
                        codes::NOT_SUPPORTED_SYNTAX,
                        "conversions such as `!r` in f-strings are not supported yet",
                        span,
                        "not supported yet",
                    ));
                    return None;
                }
                _ => {}
            }
            self.pos += self.current_char().unwrap().len_utf8();
        }
        if self.byte(0) != Some(b'}') {
            self.diags.push(
                Diagnostic::error(
                    codes::UNTERMINATED_STRING,
                    "unclosed `{` in an f-string",
                    self.span(open, open + 1),
                    "opened here",
                )
                .with_help("close the expression with `}`; write `{{` for a literal brace"),
            );
            return None;
        }
        let code_end = spec_start.map_or(self.pos, |s| s - 1);
        let text = self.text[code_start..code_end].to_string();
        let spec = spec_start.map(|s| (self.text[s..self.pos].to_string(), self.base + s));
        self.pos += 1;
        if text.trim().is_empty() {
            self.diags.push(Diagnostic::error(
                codes::UNEXPECTED_TOKEN,
                "empty expression in an f-string",
                self.span(open, self.pos),
                "expected an expression between the braces",
            ));
            return None;
        }
        Some(FPiece::Code { text, offset: self.base + code_start, spec })
    }

    fn unterminated(&mut self, open_span: Span) {
        self.diags.push(
            Diagnostic::error(
                codes::UNTERMINATED_STRING,
                "unterminated string literal",
                open_span,
                "string starts here",
            )
            .with_help(
                "close the string with a matching quote on the same line, or use `\"\"\"` for text spanning lines",
            ),
        );
    }

    fn escape(&mut self, value: &mut String) {
        let start = self.pos;
        self.pos += 1; // the backslash
        let Some(c) = self.current_char() else { return };
        self.pos += c.len_utf8();
        match c {
            'n' => value.push('\n'),
            't' => value.push('\t'),
            'r' => value.push('\r'),
            '0' => value.push('\0'),
            '\\' => value.push('\\'),
            '\'' => value.push('\''),
            '"' => value.push('"'),
            '\n' => {} // a backslash at the end of a line joins the lines
            'u' if self.byte(0) == Some(b'{') => {
                let digits_start = self.pos + 1;
                let mut end = digits_start;
                while self.bytes.get(end).is_some_and(|b| b.is_ascii_hexdigit()) {
                    end += 1;
                }
                let closed = self.bytes.get(end) == Some(&b'}');
                let decoded = u32::from_str_radix(&self.text[digits_start..end], 16).ok().and_then(char::from_u32);
                match (closed, decoded) {
                    (true, Some(ch)) => {
                        value.push(ch);
                        self.pos = end + 1;
                    }
                    _ => {
                        let hi = if closed { end + 1 } else { end };
                        self.pos = hi;
                        self.diags.push(Diagnostic::error(
                            codes::BAD_ESCAPE,
                            "invalid unicode escape",
                            self.span(start, hi),
                            "expected `\\u{` followed by hex digits and `}`",
                        ));
                    }
                }
            }
            other => {
                let span = self.span(start, self.pos);
                self.diags.push(
                    Diagnostic::error(
                        codes::BAD_ESCAPE,
                        format!("unknown escape sequence `\\{other}`"),
                        span,
                        "unknown escape",
                    )
                    .with_help("write `\\\\` for a literal backslash"),
                );
            }
        }
    }

    fn punctuation(&mut self, start: usize, c: char) {
        use TokenKind::*;
        let rest = &self.bytes[self.pos..];
        let matched = match rest.get(..3) {
            Some(b"//=") => Some((SlashSlashEq, 3)),
            _ => None,
        }
        .or_else(|| {
            let kind = match rest.get(..2)? {
                b"->" => Arrow,
                b"**" => StarStar,
                b"//" => SlashSlash,
                b"==" => EqEq,
                b"!=" => NotEq,
                b"<=" => Le,
                b">=" => Ge,
                b"<<" => Shl,
                b">>" => Shr,
                b"+=" => PlusEq,
                b"-=" => MinusEq,
                b"*=" => StarEq,
                b"/=" => SlashEq,
                b"%=" => PercentEq,
                _ => return None,
            };
            Some((kind, 2))
        })
        .or_else(|| {
            let kind = match rest[0] {
                b'(' => LParen,
                b')' => RParen,
                b'[' => LBracket,
                b']' => RBracket,
                b'{' => LBrace,
                b'}' => RBrace,
                b':' => Colon,
                b',' => Comma,
                b'.' => Dot,
                b'@' => At,
                b'?' => Question,
                b'=' => Assign,
                b'+' => Plus,
                b'-' => Minus,
                b'*' => Star,
                b'/' => Slash,
                b'%' => Percent,
                b'&' => Amp,
                b'|' => Pipe,
                b'^' => Caret,
                b'~' => Tilde,
                b'<' => Lt,
                b'>' => Gt,
                _ => return None,
            };
            Some((kind, 1))
        });

        let Some((kind, len)) = matched else {
            self.pos += c.len_utf8();
            let help = match c {
                '!' => Some("use `not` for negation"),
                ';' => Some("statements end at the end of the line; remove the `;`"),
                '\\' => {
                    Some("line continuation with `\\` is not supported; wrap the expression in parentheses instead")
                }
                _ => None,
            };
            let mut diagnostic = Diagnostic::error(
                codes::UNEXPECTED_CHAR,
                format!("unexpected character `{c}`"),
                self.span(start, self.pos),
                "not valid here",
            );
            if let Some(help) = help {
                diagnostic = diagnostic.with_help(help);
            }
            self.diags.push(diagnostic);
            return;
        };

        match kind {
            LParen | LBracket | LBrace => self.depth += 1,
            RParen | RBracket | RBrace => self.depth = self.depth.saturating_sub(1),
            _ => {}
        }
        self.pos += len;
        self.push(kind, start, self.pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> (Vec<String>, Diagnostics) {
        let mut diags = Diagnostics::default();
        let tokens = lex(FileId(0), text, &mut diags);
        (tokens.iter().map(|t| t.kind.to_string()).collect(), diags)
    }

    fn ok(text: &str) -> String {
        let (tokens, diags) = kinds(text);
        assert!(diags.is_empty(), "unexpected diagnostics for {text:?}");
        tokens.join(" ")
    }

    #[test]
    fn simple_function() {
        assert_eq!(
            ok("def main():\n    print(\"hi\")\n"),
            "def Name(main) ( ) : NEWLINE INDENT Name(print) ( Str(\"hi\") ) NEWLINE DEDENT EOF"
        );
    }

    #[test]
    fn nested_blocks_close_together() {
        assert_eq!(
            ok("a:\n  b:\n    c\nd\n"),
            "Name(a) : NEWLINE INDENT Name(b) : NEWLINE INDENT Name(c) NEWLINE DEDENT DEDENT Name(d) NEWLINE EOF"
        );
    }

    #[test]
    fn blank_and_comment_lines_do_not_change_layout() {
        assert_eq!(
            ok("a:\n    b\n\n  # comment at another depth\n    c  # trailing\n"),
            "Name(a) : NEWLINE INDENT Name(b) NEWLINE Name(c) NEWLINE DEDENT EOF"
        );
    }

    #[test]
    fn brackets_suppress_layout() {
        assert_eq!(
            ok("f(1,\n        2,\n  3)\nx\n"),
            "Name(f) ( Int(1) , Int(2) , Int(3) ) NEWLINE Name(x) NEWLINE EOF"
        );
    }

    #[test]
    fn missing_final_newline_and_open_blocks_at_eof() {
        assert_eq!(ok("a:\n    b"), "Name(a) : NEWLINE INDENT Name(b) NEWLINE DEDENT EOF");
    }

    #[test]
    fn crlf_line_endings() {
        assert_eq!(ok("a:\r\n    b\r\n"), "Name(a) : NEWLINE INDENT Name(b) NEWLINE DEDENT EOF");
    }

    #[test]
    fn empty_file() {
        assert_eq!(ok(""), "EOF");
        assert_eq!(ok("\n\n# only a comment\n"), "EOF");
    }

    #[test]
    fn tab_in_indentation_is_an_error() {
        let (_, diags) = kinds("a:\n\tb\n");
        assert_eq!(diags.sorted()[0].code, codes::TAB_IN_INDENT);
    }

    #[test]
    fn inconsistent_dedent_is_an_error() {
        let (_, diags) = kinds("a:\n    b\n  c\n");
        assert_eq!(diags.sorted()[0].code, codes::BAD_DEDENT);
    }

    #[test]
    fn numbers() {
        assert_eq!(ok("0 42 1_000 0xff 0o17 0b101"), "Int(0) Int(42) Int(1000) Int(255) Int(15) Int(5) NEWLINE EOF");
        assert_eq!(ok("1.5 2e3"), "Float(1.5) Float(2000.0) NEWLINE EOF");
        assert_eq!(ok("18446744073709551615"), "Int(18446744073709551615) NEWLINE EOF");
        let (_, diags) = kinds("18446744073709551616");
        assert_eq!(diags.sorted()[0].code, codes::INT_TOO_LARGE);
        let (_, diags) = kinds("12abc");
        assert_eq!(diags.sorted()[0].code, codes::BAD_NUMBER);
    }

    #[test]
    fn strings_and_escapes() {
        assert_eq!(ok(r#"'a\tb' "q\"" "\u{e9}""#), r#"Str("a\tb") Str("q\"") Str("é") NEWLINE EOF"#);
        assert_eq!(ok("\"\"\"two\nlines\"\"\""), r#"Str("two\nlines") NEWLINE EOF"#);
        assert_eq!(ok("\"héllo wörld\""), r#"Str("héllo wörld") NEWLINE EOF"#);
        let (_, diags) = kinds("\"open\nx");
        assert_eq!(diags.sorted()[0].code, codes::UNTERMINATED_STRING);
        let (_, diags) = kinds(r#""\q""#);
        assert_eq!(diags.sorted()[0].code, codes::BAD_ESCAPE);
    }

    #[test]
    fn operators_use_longest_match() {
        assert_eq!(ok("a //= b ** c -> d != e"), "Name(a) //= Name(b) ** Name(c) -> Name(d) != Name(e) NEWLINE EOF");
    }

    #[test]
    fn keywords() {
        assert_eq!(ok("pub def None True self"), "pub def None True self NEWLINE EOF");
    }

    #[test]
    fn f_strings() {
        let mut diags = Diagnostics::default();
        let tokens = lex(FileId(0), "f\"a{x + 1}b{{}}{y:>5}\"", &mut diags);
        assert!(diags.is_empty());
        let TokenKind::FString(pieces) = &tokens[0].kind else { panic!("expected an f-string") };
        assert_eq!(
            pieces,
            &[
                FPiece::Literal("a".into()),
                FPiece::Code { text: "x + 1".into(), offset: 4, spec: None },
                FPiece::Literal("b{}".into()),
                FPiece::Code { text: "y".into(), offset: 16, spec: Some((">5".into(), 18)) },
            ]
        );
        let (_, diags) = kinds("f\"{x\"");
        assert_eq!(diags.sorted()[0].code, codes::UNTERMINATED_STRING);
        let (_, diags) = kinds("f\"{}\"");
        assert_eq!(diags.sorted()[0].code, codes::UNEXPECTED_TOKEN);
    }

    #[test]
    fn fragments_are_offset_and_have_no_layout() {
        let mut diags = Diagnostics::default();
        let tokens = lex_fragment(FileId(0), "a +\n b", 10, &mut diags);
        let kinds: Vec<String> = tokens.iter().map(|t| t.kind.to_string()).collect();
        assert_eq!(kinds.join(" "), "Name(a) + Name(b) EOF");
        assert_eq!(tokens[0].span.lo, 10);
    }

    #[test]
    fn unexpected_character() {
        let (_, diags) = kinds("a ! b");
        assert_eq!(diags.sorted()[0].code, codes::UNEXPECTED_CHAR);
    }
}
