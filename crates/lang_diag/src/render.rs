//! Plain-text rendering of diagnostics, in the style of rustc:
//!
//! ```text
//! error[E0300]: expected `str`, found `int`
//!  --> app.src:2:12
//!   |
//! 1 | def test(a: int) -> str:
//!   |                     --- expected because of this return type
//! 2 |     return a
//!   |            ^ expected `str`, found `int`
//! ```
//!
//! No colours, so output is stable for golden tests.

use crate::{Diagnostic, Label, Severity, SourceMap};
use std::fmt::Write;

pub fn render(diagnostic: &Diagnostic, sources: &SourceMap) -> String {
    let file = sources.get(diagnostic.primary.span.file);
    let severity = match diagnostic.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };

    // Labels in another file are not drawn; there is only ever one file for now.
    let mut labels: Vec<(&Label, bool)> = std::iter::once((&diagnostic.primary, true))
        .chain(diagnostic.secondary.iter().map(|l| (l, false)))
        .filter(|(l, _)| l.span.file == diagnostic.primary.span.file)
        .collect();
    labels.sort_by_key(|(l, _)| (l.span.lo, l.span.hi));

    let lines: Vec<usize> = labels.iter().map(|(l, _)| file.line_col(l.span.lo as usize).0).collect();
    let width = lines.iter().max().copied().unwrap_or(1).to_string().len();
    let pad = " ".repeat(width);

    let mut out = String::new();
    let (line, col) = file.line_col(diagnostic.primary.span.lo as usize);
    let _ = writeln!(out, "{severity}[{}]: {}", diagnostic.code, diagnostic.message);
    let _ = writeln!(out, "{pad}--> {}:{line}:{col}", file.name);
    let _ = writeln!(out, "{pad} |");

    let mut previous_line = None;
    for ((label, primary), &line) in labels.iter().zip(&lines) {
        let text = file.line_text(line);
        if previous_line != Some(line) {
            if previous_line.is_some_and(|p| line > p + 1) {
                let _ = writeln!(out, "{pad} ...");
            }
            // Tabs are shown as single spaces so that markers stay aligned with characters.
            let _ = writeln!(out, "{line:>width$} | {}", text.replace('\t', " ").trim_end());
            previous_line = Some(line);
        }
        let (_, start_col) = file.line_col(label.span.lo as usize);
        let line_chars = text.chars().count();
        let (end_line, end_col) = file.line_col(label.span.hi as usize);
        let end_col = if end_line == line { end_col } else { line_chars + 1 };
        let marks = end_col.saturating_sub(start_col).max(1);
        let mark = if *primary { "^" } else { "-" };
        let indent = " ".repeat(start_col - 1);
        let message = if label.message.is_empty() { String::new() } else { format!(" {}", label.message) };
        let _ = writeln!(out, "{pad} | {indent}{}{message}", mark.repeat(marks));
    }

    if let Some(help) = &diagnostic.help {
        let _ = writeln!(out, "{pad} |");
        let _ = writeln!(out, "{pad} = help: {help}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Diagnostic, Span};

    #[test]
    fn renders_primary_and_secondary_labels() {
        let mut sources = SourceMap::default();
        let file = sources.add("app.src", "def test(a: int) -> str:\n    return a\n");
        let diagnostic = Diagnostic::error(
            "E0300",
            "expected `str`, found `int`",
            Span::new(file, 36, 37),
            "expected `str`, found `int`",
        )
        .with_label(Span::new(file, 20, 23), "expected because of this return type")
        .with_help("convert it with `str(a)`");
        let expected = "\
error[E0300]: expected `str`, found `int`
 --> app.src:2:12
  |
1 | def test(a: int) -> str:
  |                     --- expected because of this return type
2 |     return a
  |            ^ expected `str`, found `int`
  |
  = help: convert it with `str(a)`
";
        assert_eq!(render(&diagnostic, &sources), expected);
    }

    #[test]
    fn zero_width_span_gets_one_marker() {
        let mut sources = SourceMap::default();
        let file = sources.add("app.src", "x\n");
        let diagnostic = Diagnostic::warning("W0001", "message", Span::new(file, 1, 1), "");
        assert_eq!(render(&diagnostic, &sources), "warning[W0001]: message\n --> app.src:1:2\n  |\n1 | x\n  |  ^\n");
    }

    #[test]
    fn columns_count_characters() {
        let mut sources = SourceMap::default();
        let file = sources.add("app.src", "s = \"héllo\" + x\n");
        let lo = "s = \"héllo\" + ".len();
        let (line, col) = sources.get(file).line_col(lo);
        assert_eq!((line, col), (1, 15));
    }
}
