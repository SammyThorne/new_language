//! Source files, spans and diagnostics shared by every phase of the compiler.
//!
//! Spans live here rather than in `lang_syntax` so that every phase, including the lexer, can
//! report diagnostics without a dependency cycle.

pub mod codes;
mod render;

pub use render::render;

/// Identifies one file in a [`SourceMap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FileId(pub u32);

/// A byte range in one source file. Always on UTF-8 character boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: FileId,
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(file: FileId, lo: usize, hi: usize) -> Span {
        Span { file, lo: lo as u32, hi: hi as u32 }
    }

    /// The smallest span covering both `self` and `other`.
    pub fn to(self, other: Span) -> Span {
        Span { file: self.file, lo: self.lo.min(other.lo), hi: self.hi.max(other.hi) }
    }

    /// A zero-width span at the end of this one.
    pub fn end(self) -> Span {
        Span { file: self.file, lo: self.hi, hi: self.hi }
    }
}

/// One loaded source file.
pub struct SourceFile {
    /// The path as the user wrote it; shown in diagnostics.
    pub name: String,
    pub text: String,
    line_starts: Vec<usize>,
}

impl SourceFile {
    pub fn new(name: String, text: String) -> SourceFile {
        let line_starts = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
        SourceFile { name, text, line_starts }
    }

    /// 1-based line and column of a byte offset. Columns count characters, not bytes.
    pub fn line_col(&self, offset: usize) -> (usize, usize) {
        let offset = offset.min(self.text.len());
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let col = self.text[self.line_starts[line]..offset].chars().count() + 1;
        (line + 1, col)
    }

    /// The text of a 1-based line, without its line terminator.
    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line - 1];
        let end = self.line_starts.get(line).map_or(self.text.len(), |&next| next - 1);
        self.text[start..end].trim_end_matches('\r')
    }
}

/// All source files of one compilation.
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn add(&mut self, name: impl Into<String>, text: impl Into<String>) -> FileId {
        self.files.push(SourceFile::new(name.into(), text.into()));
        FileId(self.files.len() as u32 - 1)
    }

    pub fn get(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

/// A span with a short message, drawn under the source line.
#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

/// One error or warning. Codes are stable and listed in [`codes`].
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: &'static str,
    pub message: String,
    pub primary: Label,
    pub secondary: Vec<Label>,
    pub help: Option<String>,
}

impl Diagnostic {
    pub fn error(code: &'static str, message: impl Into<String>, span: Span, label: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            code,
            message: message.into(),
            primary: Label { span, message: label.into() },
            secondary: Vec::new(),
            help: None,
        }
    }

    pub fn warning(code: &'static str, message: impl Into<String>, span: Span, label: impl Into<String>) -> Diagnostic {
        Diagnostic { severity: Severity::Warning, ..Diagnostic::error(code, message, span, label) }
    }

    pub fn with_help(mut self, help: impl Into<String>) -> Diagnostic {
        self.help = Some(help.into());
        self
    }

    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.secondary.push(Label { span, message: message.into() });
        self
    }
}

/// The diagnostics collected during one compilation.
#[derive(Default)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn push(&mut self, diagnostic: Diagnostic) {
        self.items.push(diagnostic);
    }

    pub fn error_count(&self) -> usize {
        self.items.iter().filter(|d| d.severity == Severity::Error).count()
    }

    pub fn has_errors(&self) -> bool {
        self.error_count() > 0
    }

    /// Whether an error has already been reported starting inside `lo..=hi` of `file`. Used to
    /// avoid reporting a follow-on error caused by an earlier one on the same line.
    pub fn has_error_in(&self, file: FileId, lo: u32, hi: u32) -> bool {
        self.items.iter().any(|d| {
            d.severity == Severity::Error && d.primary.span.file == file && (lo..=hi).contains(&d.primary.span.lo)
        })
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Diagnostics in source order. Phases report in phase order, so this interleaves them by
    /// position, which is the order a reader wants.
    pub fn sorted(&self) -> Vec<&Diagnostic> {
        let mut items: Vec<&Diagnostic> = self.items.iter().collect();
        items.sort_by_key(|d| (d.primary.span.file, d.primary.span.lo));
        items
    }
}
