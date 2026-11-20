//! The warning for an import nothing uses (docs/11, milestone 5).
//!
//! It works on the tokens of the file, not on what the checker resolved: an imported name counts
//! as used if it appears anywhere outside the import statements, in code, in an annotation or in
//! an f-string's braces. That can miss an unused import whose name a variable also has, but it
//! never calls a used import unused, which would be the worse mistake.

use super::{Checker, ModuleInput};
use lang_diag::{Diagnostic, Diagnostics, Span, codes};
use lang_syntax::ast;
use lang_syntax::token::{FPiece, TokenKind};

/// The names a file uses, each with the name after a `.` that follows it, if any.
fn names_used(
    tokens: &[lang_syntax::token::Token],
    skip: &[(u32, u32)],
    file: lang_diag::FileId,
) -> Vec<(String, Option<String>)> {
    let mut used = Vec::new();
    collect(tokens, skip, file, &mut used);
    used
}

fn collect(
    tokens: &[lang_syntax::token::Token],
    skip: &[(u32, u32)],
    file: lang_diag::FileId,
    used: &mut Vec<(String, Option<String>)>,
) {
    for (index, token) in tokens.iter().enumerate() {
        if skip.iter().any(|(lo, hi)| token.span.lo >= *lo && token.span.hi <= *hi) {
            continue;
        }
        match &token.kind {
            TokenKind::Name(name) => {
                let after = match (tokens.get(index + 1), tokens.get(index + 2)) {
                    (Some(dot), Some(next)) if dot.kind == TokenKind::Dot => match &next.kind {
                        TokenKind::Name(member) => Some(member.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                used.push((name.clone(), after));
            }
            TokenKind::FString(pieces) => {
                for piece in pieces {
                    if let FPiece::Code { text, offset, .. } = piece {
                        let mut ignored = Diagnostics::default();
                        let inner = lang_syntax::lex_fragment(file, text, *offset, &mut ignored);
                        collect(&inner, &[], file, used);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Checker<'_, '_> {
    pub(super) fn warn_unused_imports(&mut self, inputs: &[ModuleInput]) {
        for input in inputs.iter().filter(|m| !m.stdlib) {
            let imports: Vec<&ast::ImportDecl> = input
                .ast
                .items
                .iter()
                .filter_map(|item| if let ast::Item::Import(import) = item { Some(import) } else { None })
                .collect();
            let Some(first) = imports.first() else { continue };
            let file = first.span.file;
            let text = self.sources.get(file).text.clone();
            let mut ignored = Diagnostics::default();
            let tokens = lang_syntax::lex(file, &text, &mut ignored);
            let skip: Vec<(u32, u32)> = imports.iter().map(|i| (i.span.lo, i.span.hi)).collect();
            let used = names_used(&tokens, &skip, file);
            for import in imports {
                match &import.names {
                    None => {
                        let (key, span) = match &import.alias {
                            Some(alias) => (alias.name.clone(), alias.span),
                            None => {
                                let path: Vec<&str> = import.path.iter().map(|p| p.name.as_str()).collect();
                                (path.join("."), import.path[0].span.to(import.path.last().expect("a path").span))
                            }
                        };
                        let first = key.split('.').next().unwrap_or(&key).to_string();
                        let second = key.split('.').nth(1).map(String::from);
                        let is_used = used.iter().any(|(name, after)| {
                            *name == first && (second.is_none() || after.as_deref() == second.as_deref())
                        });
                        if !is_used {
                            self.unused_import(&key, span);
                        }
                    }
                    Some(names) => {
                        for (name, alias) in names {
                            let (key, span) = match alias {
                                Some(alias) => (alias.name.as_str(), alias.span),
                                None => (name.name.as_str(), name.span),
                            };
                            if !used.iter().any(|(n, _)| n == key) {
                                self.unused_import(key, span);
                            }
                        }
                    }
                }
            }
        }
    }

    fn unused_import(&mut self, key: &str, span: Span) {
        self.diags.out.push(
            Diagnostic::warning(
                codes::UNUSED_IMPORT,
                format!("`{key}` is imported but never used"),
                span,
                "imported here",
            )
            .with_help("remove the import, or use it"),
        );
    }
}
